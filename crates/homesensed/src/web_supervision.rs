//! Explicit simulation publication supervisor. No physical endpoint is opened.
use crate::{
    admission::Admission,
    automation_runtime::AutomationRuntime,
    observed_transport::ObservedTransport,
    web_api::{Api, Runtime},
};
use homesense_core::ExecutionContext;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    api: Arc<Api>,
    admission: Admission,
    controller: Uuid,
    mut runtime: AutomationRuntime,
    mut owner: ObservedTransport<crate::service::SimulatedStream>,
    mut publications: crate::automation_publication::Receiver,
    mut stop: tokio::sync::watch::Receiver<bool>,
    source: PathBuf,
) -> Result<(), String> {
    let mut epoch = 1u64;
    loop {
        let (runtime_stop, runtime_receive) = tokio::sync::watch::channel(false);
        let mut pending = None;
        let handoff_gate=runtime.handoff_gate();
        let outcome = {
            let (ready, mut started) = tokio::sync::oneshot::channel();
            let mut running = false;
            let drive = runtime.drive_recoverable_ready(
                &mut owner,
                Duration::from_millis(10),
                5000,
                runtime_receive,
                crate::service::wall_ms,
                Some(ready),
            );
            tokio::pin!(drive);
            loop {
                tokio::select! {
                    result=&mut drive=>break result,
                    changed=stop.changed()=>{if changed.is_err() || *stop.borrow(){let _=runtime_stop.send(true);break drive.await;}},
                    result=&mut started,if !running=>{if result.is_err(){break drive.await;}running=true;},
                    request=publications.receive(),if running=>{
                        let Some(request)=request else {let _=runtime_stop.send(true);break drive.await;};
                        let candidate=match crate::automation_tcp::validate_candidate_in_context(request.devices(),request.rules(),Duration::from_millis(20),ExecutionContext::Simulation){Ok(value)=>value,Err(e)=>{request.complete(Err(e.to_string()));continue;}};
                        let document=match crate::configuration_publish::validate_automation(&candidate,request.rules(),ExecutionContext::Simulation){Ok(value)=>value,Err(e)=>{request.complete(Err(e.to_string()));continue;}};
                        let selected:std::collections::BTreeMap<String,String>=document.scene_catalog();
                        if selected.len()>64 {request.complete(Err("management scene limit exceeded".into()));continue;}
                        if request.abandoned(){request.complete(Err("publication abandoned".into()));continue;}
                        let previous=api.runtime.write().map_err(|_|"management view poisoned")?.take();
                        let source=source.clone();let expected=request.expected_revision();
                        let backup=tokio::task::spawn_blocking(move || {
                            let parent=source.parent().ok_or("database parent missing")?.join("publication-backups");
                            let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
                            #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
                            builder.create(&parent).map_err(|e|e.to_string())?;
                            homesense_storage::backup::create(&source,&parent.join(format!("before-{expected}-{}.sqlite",Uuid::new_v4())),Duration::from_secs(10))
                        }).await.map_err(|e|e.to_string()).and_then(|v|v);
                        if backup.is_err() || request.abandoned() || *stop.borrow() {
                            *api.runtime.write().map_err(|_|"management view poisoned")?=previous;
                            request.complete(Err("backup failed or publication abandoned".into()));continue;
                        }
                        let quantum=tokio::select! {
                            permit=handoff_gate.clone().lock_owned()=>permit,
                            result=&mut drive=>{request.complete(Err("runtime ended before publication barrier".into()));break result;},
                            _=async { let _=stop.wait_for(|stopped|*stopped).await; }=>{
                                request.complete(Err("service stopped before publication barrier".into()));
                                let _=runtime_stop.send(true);break drive.await;
                            },
                        };
                        // A ready permit may win select at the same turn as stop.
                        // Revalidate the service before any publication side effect.
                        let stopped=*stop.borrow() || stop.has_changed().is_err();
                        if stopped {
                            request.complete(Err("service stopped before publication barrier".into()));
                            let _=runtime_stop.send(true);
                            drop(quantum);
                            break drive.await;
                        }
                        if request.abandoned() {
                            drop(quantum);
                            *api.runtime.write().map_err(|_|"management view poisoned")?=previous;
                            request.complete(Err("publication abandoned before barrier".into()));continue;
                        }
                        if let Err(e)=request.begin(&admission,Duration::from_secs(3)).await {
                            *api.runtime.write().map_err(|_|"management view poisoned")?=previous;
                            request.complete(Err(format!("publication rejected: {e:?}")));continue;
                        }
                        pending=Some((request,candidate,selected));let _=runtime_stop.send(true);drop(quantum);break drive.await;
                    }
                }
            }
        };
        drop(runtime);
        match outcome? {
            crate::automation_drive::Exit::Stopped => return Ok(()),
            crate::automation_drive::Exit::Disconnected { .. } => {
                return Err("simulation disconnected".into());
            }
            crate::automation_drive::Exit::PublicationPaused => {
                let (request, candidate, scenes) =
                    pending.take().ok_or("publication missing request")?;
                let transition = async {
                    while owner.drain_publication(&admission).await?.is_some() {}
                    let revision = admission
                        .commit_publication()
                        .await
                        .map_err(|e| format!("{e:?}"))?;
                    epoch = epoch.checked_add(1).ok_or("epoch exhausted")?;
                    let transport = crate::transport_owner::TransportOwner::new(
                        admission.clone(),
                        crate::service::simulation_connections_at(&candidate, epoch)?,
                    );
                    let (next, fresh) = crate::publication_recovery::rebuild(
                        admission.clone(),
                        controller,
                        transport,
                        epoch,
                        crate::publication_recovery::Settings {
                            query_timeout_ms: 1000,
                            max_age_ms: 5000,
                            wall_unix_ms: crate::service::wall_ms()?,
                            calendar_lateness_ms: 1000,
                            scene_capacity: 64,
                        },
                    )
                    .await?;
                    Ok::<_, String>((revision, next, fresh))
                };
                match tokio::time::timeout(Duration::from_secs(15), transition).await {
                    Ok(Ok((revision, next, fresh))) => {
                        *api.runtime
                            .write()
                            .map_err(|_| "management view poisoned")? = Some(Runtime {
                            control: next.control(),
                            revision,
                            boot: admission.boot_id().to_owned(),
                            scenes,
                        });
                        runtime = next;
                        owner = fresh;
                        request.complete(Ok(revision));
                    }
                    _ => {
                        admission.stop_now();
                        request.complete(Err(
                            "publication outcome uncertain; inspect active revision".into(),
                        ));
                        return Err("publication rebuild failed".into());
                    }
                }
            }
        }
    }
}
