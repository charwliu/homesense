//! Explicit bounded automation over TCP with optional supervised Web/HAP endpoints.
use crate::{
    admission::{Admission, Options},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    reconnect::{RetryPolicy, reconnect_tcp},
};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

struct Stop(Admission);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}
fn wall_ms() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|e| e.to_string())
}
/// This entry point executes enabled, reviewed rules, including startup rules.
/// The caller explicitly confirms concurrent gateway clients are supported.
/// Loss of transport ends this run; a new invocation never replays old commands.
pub async fn serve(
    devices: &[u8],
    rules: &[u8],
    directory: &Path,
    duration: Duration,
    coexistence_confirmed: bool,
) -> Result<(), String> {
    serve_supervised(
        devices,
        rules,
        directory,
        duration,
        coexistence_confirmed,
        0,
    )
    .await
}
/// Opt-in bounded recovery after an explicitly classified, fully cleaned disconnect.
/// The duration is shared by all connections after initial synchronization.
pub async fn serve_supervised(
    devices: &[u8],
    rules: &[u8],
    directory: &Path,
    duration: Duration,
    coexistence_confirmed: bool,
    recovery_limit: u8,
) -> Result<(), String> {
    serve_with_publications(devices, rules, directory, duration, coexistence_confirmed, recovery_limit, None).await
}
/// Trusted in-process publication entry point. The receiver is an administrative
/// capability; external adapters must authenticate before handing out its sender.
pub async fn serve_with_publications(
    devices: &[u8], rules: &[u8], directory: &Path, duration: Duration,
    coexistence_confirmed: bool, recovery_limit: u8,
    publications: Option<crate::automation_publication::Receiver>,
) -> Result<(), String> {
    serve_with_write_interval(devices,rules,directory,duration,coexistence_confirmed,recovery_limit,publications,Duration::from_millis(20)).await
}
/// The physical write gap is retained across recovery and publication because
/// all transport generations share the same Admission owner.
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_write_interval(
    devices: &[u8], rules: &[u8], directory: &Path, duration: Duration,
    coexistence_confirmed: bool, recovery_limit: u8,
    publications: Option<crate::automation_publication::Receiver>,
    write_interval: Duration,
) -> Result<(), String> {
    serve_inner(devices,rules,directory,duration,coexistence_confirmed,recovery_limit,publications,write_interval,None,None,None,None,#[cfg(test)] None).await
}
#[allow(clippy::too_many_arguments)]
pub async fn serve_managed(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,coexistence_confirmed:bool,recovery_limit:u8,write_interval:Duration,settings:crate::service::WebSettings)->Result<(),String> {
    if let Some(tls)=&settings.tls {tls.validate_address(settings.address)?;} else if !settings.address.ip().is_loopback() || settings.address.port()==0 {return Err("plaintext management requires loopback".into());}
    let (publisher,receiver)=crate::automation_publication::channel();
    serve_inner(devices,rules,directory,duration,coexistence_confirmed,recovery_limit,Some(receiver),write_interval,Some(settings),Some(publisher),None,None,#[cfg(test)] None).await
}
#[allow(clippy::too_many_arguments)]
pub async fn serve_combined(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,coexistence_confirmed:bool,recovery_limit:u8,write_interval:Duration,web:crate::service::WebSettings,hap:crate::hap_endpoint::Settings)->Result<(),String> {
    if let Some(tls)=&web.tls {tls.validate_address(web.address)?;} else if !web.address.ip().is_loopback() || web.address.port()==0 {return Err("plaintext management requires loopback".into());}
    if hap.address==web.address {return Err("HAP and Web require distinct endpoints".into());}
    let (publisher,receiver)=crate::automation_publication::channel();
    serve_inner(devices,rules,directory,duration,coexistence_confirmed,recovery_limit,Some(receiver),write_interval,Some(web),Some(publisher),Some(hap),None,#[cfg(test)] None).await
}
/// Publish explicitly selected independent bridges using one transport/runtime.
#[allow(clippy::too_many_arguments)]
pub async fn serve_bridges(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,coexistence_confirmed:bool,recovery_limit:u8,write_interval:Duration,web:crate::service::WebSettings,plan:Vec<u8>)->Result<(),String> {
    if let Some(tls)=&web.tls {tls.validate_address(web.address)?;} else if !web.address.ip().is_loopback() || web.address.port()==0 {return Err("plaintext management requires loopback".into());}
    let (publisher,receiver)=crate::automation_publication::channel();
    serve_inner(devices,rules,directory,duration,coexistence_confirmed,recovery_limit,Some(receiver),write_interval,Some(web),Some(publisher),None,Some(plan),#[cfg(test)] None).await
}
fn validate_bridge_plan(plan:&[u8],config:&homesense_config::Configuration,rules:&[u8],web:Option<std::net::SocketAddr>)->Result<(),String> {
    let document=crate::configuration_publish::validate_automation(config,rules,ExecutionContext::Live).map_err(|e|e.to_string())?;
    let parsed=crate::hap_bridge_plan::Plan::parse(plan,config,&document)?;
    if parsed.bridges.iter().any(|b|Some(b.address)==web) {return Err("HAP and Web require distinct endpoints".into());}
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn serve_inner(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,coexistence_confirmed:bool,recovery_limit:u8,mut publications:Option<crate::automation_publication::Receiver>,write_interval:Duration,mut management_settings:Option<crate::service::WebSettings>,publisher:Option<crate::automation_publication::Publisher>,hap_settings:Option<crate::hap_endpoint::Settings>,hap_plan:Option<Vec<u8>>,#[cfg(test)] hap_failure_after:Option<Duration>)->Result<(),String> {
    if !(Duration::from_millis(20)..=Duration::from_secs(1)).contains(&write_interval) {
        return Err("write interval must be 20..1000 ms".into());
    }
    if recovery_limit > 16 {
        return Err("recovery limit must be 0..16".into());
    }
    if !coexistence_confirmed || duration.is_zero() || duration > Duration::from_secs(86400) {
        return Err("automation TCP requires confirmed coexistence and duration in (0,24h]".into());
    }
    let mut config = validate_candidate_with_write_interval(devices, rules, write_interval)?;
    if let Some(settings)=&hap_settings {settings.validate(&config,&crate::configuration_publish::validate_automation(&config,rules,ExecutionContext::Live).map_err(|e|e.to_string())?)?;}
    if hap_settings.is_some() && hap_plan.is_some() {return Err("single and multiple HAP publication are exclusive".into());}
    if let Some(plan)=&hap_plan {validate_bridge_plan(plan,&config,rules,management_settings.as_ref().map(|s|s.address))?;}
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join("automation-tcp.sqlite");
    let mut store = Store::open(&path).map_err(|e| e.to_string())?;
    if let Some(active) = store.active().map_err(|e| e.to_string())? {
        if active.payload != devices
            || store
                .automation_payload(active.revision)
                .map_err(|e| e.to_string())?
                .as_deref()
                != Some(rules)
        {
            return Err("stored automation differs; explicit publication required".into());
        }
    } else {
        store
            .publish_automation(None, devices, rules, ExecutionContext::Live, &[])
            .map_err(|e| e.to_string())?;
    }
    drop(store);
    let controller = if management_settings.is_some(){crate::service::web_controller(directory)?}else{Uuid::new_v4()};
    let identity=match &hap_settings {Some(settings)=>Some(settings.identity(directory).await?),None=>None};
    let mut paired=identity.clone().into_iter().collect::<Vec<_>>();
    if let Some(plan)=&hap_plan {
        let document=crate::configuration_publish::validate_automation(&config,rules,ExecutionContext::Live).map_err(|e|e.to_string())?;
        paired.extend(crate::hap_bridges::prepare(plan,&config,&document,directory).await?.into_iter().map(|bridge|bridge.identity));
    }
    let admission = Admission::open(path.clone(), Options {write_interval,..Options::default()}, move |id| id == controller || paired.iter().any(|store|store.controller_is_paired(id).unwrap_or(false))).await?;
    let _guard = Stop(admission.clone());
    let result = async {
        let (_stop, receiver) = tokio::sync::watch::channel(false);
        let mut endpoints: std::collections::BTreeMap<_,_> = config
            .gateways()
            .iter()
            .map(|(id, g)| (id.clone(), g.endpoint))
            .collect();
        let (transport, mut epoch) = reconnect_tcp(
            admission.clone(),
            endpoints.clone(),
            1,
            RetryPolicy::default(),
            true,
            receiver.clone(),
        )
        .await?;
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Live,
            Uuid::parse_str(admission.boot_id())
                .map_err(|e| e.to_string())?
                .as_u128(),
            epoch,
        )?;
        let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
        owner
            .query_recovery_with_policy(&config, ExecutionContext::Live, 1000, 5000, crate::observed_transport::Synchronization::OutputsAndCalibratedMotors)
            .await?;
        let deadline=tokio::time::Instant::now()+duration;
        let mut recoveries=0;
        let mut prepared_runtime = None;
        let mut hap:Option<crate::hap_endpoint::Endpoint>=None;
        let mut bridges:Option<crate::hap_bridge_group::Group>=None;
        let mut management:Option<crate::management_endpoint::Endpoint>=None;
        loop {
            use crate::{automation_drive::Exit,automation_runtime::StartupPolicy};
            let startup=if recoveries==0 {StartupPolicy::Run} else {StartupPolicy::Suppress};
            let (runtime_stop, runtime_receive) = tokio::sync::watch::channel(false);
            let mut pending = None;
            let mut runtime=match prepared_runtime.take() {
                Some(runtime) => runtime,
                None => AutomationRuntime::load_with_startup(admission.clone(),controller,wall_ms()?,1000,64,Default::default(),startup).await?,
            };
            if hap.is_none() && let (Some(settings),Some(identity))=(&hap_settings,&identity) {hap=crate::hap_endpoint::Endpoint::start(settings,identity,&admission,controller,&runtime,owner.snapshot(),management.is_some() || management_settings.is_some()).await?;}
            if bridges.is_none() && let Some(plan)=&hap_plan {bridges=Some(crate::hap_bridge_group::Group::start(plan,directory,&admission,controller,&runtime,owner.snapshot()).await?);}
            #[cfg(test)]
            if let (Some(delay),Some(endpoint))=(hap_failure_after,&hap) {
                let handle=endpoint.abort_handle();
                tokio::spawn(async move {tokio::time::sleep(delay).await;handle.abort();});
            }
            if let Some(settings)=management_settings.take() {
                management=Some(crate::management_endpoint::Endpoint::start(settings,directory,&admission,controller,&runtime,publisher.clone().ok_or("management publisher missing")?).await?);
            } else if let Some(endpoint)=&management {endpoint.install(&admission,controller,&runtime).await?;}
            if let Some(endpoint)=&management {endpoint.set_hap_status(if hap.is_some(){crate::web_api::HapStatus::Listening}else if hap_settings.is_some(){crate::web_api::HapStatus::Degraded}else{crate::web_api::HapStatus::Disabled})?;}
            if let (Some(endpoint),Some(group))=(&management,&bridges) {endpoint.set_hap_bridges(group.statuses.borrow().clone())?;}
            let outcome={
                let (ready,mut started)=tokio::sync::oneshot::channel();
                let mut running=false;
                let drive=runtime.drive_recoverable_projected(&mut owner,Duration::from_millis(10),5000,runtime_receive,wall_ms,Some(ready),hap.as_ref().map(|endpoint|endpoint.projection.clone()).or_else(||bridges.as_ref().map(|group|group.projection.clone())));
                tokio::pin!(drive);
                loop {
                    tokio::select! {
                        result=&mut drive=>break result,
                        changed=async {match bridges.as_mut(){Some(group)=>group.statuses.changed().await,None=>std::future::pending().await}}=>{
                            if changed.is_err() {
                                if let Some(group)=bridges.take() {
                                    let mut statuses=group.statuses.borrow().clone();
                                    for status in statuses.values_mut() {*status=crate::hap_bridge_group::Status::Degraded;}
                                    group.shutdown().await;
                                    if let Some(endpoint)=&management {endpoint.set_hap_bridges(statuses)?;}
                                }
                            } else if let (Some(endpoint),Some(group))=(&management,&bridges) {endpoint.set_hap_bridges(group.statuses.borrow().clone())?;}
                        },
                        result=async {match hap.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>{
                            log::error!("HAP channel degraded: {}",result.err().unwrap_or_else(||"HAP listener ended".into()));
                            // Retire sessions/projection without cancelling the shared automation
                            // owner. It retains its bounded watch sender until this generation ends.
                            if let Some(endpoint)=hap.take() {endpoint.shutdown().await;}
                            if let Some(endpoint)=&management {endpoint.set_hap_status(crate::web_api::HapStatus::Degraded)?;}

                        },
                        result=async {match management.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>{
                            let _=runtime_stop.send(true);let cleanup=drive.await;
                            break cleanup.and_then(|_|Err(result.err().unwrap_or_else(||"management listener ended".into())));
                        },
                        _=tokio::time::sleep_until(deadline)=>{let _=runtime_stop.send(true);break drive.await},
                        result=&mut started, if !running => {if result.is_err() {break drive.await;} running=true;},
                        request=async {
                            match publications.as_mut() {
                                Some(receiver) => receiver.receive().await,
                                None => std::future::pending().await,
                            }
                        }, if running => {
                            let Some(request)=request else { publications=None; continue; };
                            let candidate=match validate_candidate_with_write_interval(request.devices(),request.rules(),write_interval) {
                                Ok(config)=>config,
                                Err(error)=>{request.complete(Err(error));continue;}
                            };
                            if let Some(settings)=&hap_settings {
                                let validation=crate::configuration_publish::validate_automation(&candidate,request.rules(),ExecutionContext::Live).map_err(|e|e.to_string()).and_then(|document|settings.validate(&candidate,&document));
                                if let Err(error)=validation {request.complete(Err(error));continue;}
                            }
                            if let Some(plan)=&hap_plan && let Err(error)=validate_bridge_plan(plan,&candidate,request.rules(),None) {request.complete(Err(error));continue;}
                            if request.abandoned() {request.complete(Err("publication abandoned before barrier".into()));continue;}
                            if management.is_some() {
                                let document=crate::configuration_publish::validate_automation(&candidate,request.rules(),ExecutionContext::Live).map_err(|e|e.to_string())?;
                                if document.timed_scenes.values().filter(|s|s.enabled && s.review_evidence.as_ref().is_some_and(|v|!v.trim().is_empty())).count()>64 {request.complete(Err("management scene capacity exceeded".into()));continue;}
                            }
                            let previous=match &management {Some(endpoint)=>endpoint.pause()?,None=>None};
                            let source=path.clone();let expected=request.expected_revision();
                            let backup=tokio::task::spawn_blocking(move || {
                                let folder=source.parent().ok_or("database parent missing")?.join("publication-backups");
                                let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
                                #[cfg(unix)]{use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
                                builder.create(&folder).map_err(|e|e.to_string())?;
                                homesense_storage::backup::create(&source,&folder.join(format!("before-{expected}-{}.sqlite",Uuid::new_v4())),Duration::from_secs(10))
                            }).await.map_err(|e|e.to_string()).and_then(|v|v);
                            if backup.is_err() || request.abandoned() {
                                if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                request.complete(Err("backup failed or publication abandoned".into()));continue;
                            }
                            if let Err(error)=admission.begin_automation_publication(request.devices().to_vec(),request.rules().to_vec(),request.expected_revision(),Duration::from_secs(3)).await {
                                if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                request.complete(Err(format!("publication rejected: {error:?}")));continue;
                            }
                            pending=Some((request,candidate));
                            let _=runtime_stop.send(true);
                            break drive.await;
                        }
                    }
                }
            };
            if let Some(endpoint)=&management {endpoint.pause()?;if hap_settings.is_some() || hap_plan.is_some(){endpoint.set_hap_status(crate::web_api::HapStatus::Reconfiguring)?;}}
            if let Some(endpoint)=hap.take() {endpoint.shutdown().await;}
            if let Some(group)=bridges.take() {group.shutdown().await;}
            drop(runtime);
            let outcome=match outcome {
                Ok(outcome)=>outcome,
                Err(error)=>{
                    if let Some((request,_))=pending.take() {
                        request.complete(Err(format!("publication runtime handoff failed; inspect active revision before retrying: {error}")));
                    }
                    return Err(error);
                }
            };
            match outcome {
                Exit::Stopped => return Ok(()),
                Exit::PublicationPaused => {
                    let (request, candidate)=pending.take().ok_or("publication paused without request")?;
                    let transition=async {
                        while owner.drain_publication(&admission).await?.is_some() {}
                        let revision=admission.commit_publication().await.map_err(|e|format!("publication commit: {e:?}"))?;
                        let next=epoch.checked_add(1).ok_or("transport epoch exhausted")?;
                        let mut connections=std::collections::BTreeMap::new();
                        for (id,gateway) in candidate.gateways() {
                            let address=gateway.endpoint;
                            let connection=tokio::task::spawn_blocking(move || hb_transport::Connection::connect(address,next,Duration::from_secs(2),Duration::from_millis(100)))
                                .await.map_err(|e|format!("publication connect worker for {id}: {e}"))?.map_err(|e|format!("publication connect gateway {id}: {e}"))?;
                            connections.insert(id.clone(),connection);
                        }
                        let transport=crate::transport_owner::TransportOwner::new(admission.clone(),connections);
                        let (runtime,fresh)=crate::publication_recovery::rebuild_with_policy(
                            admission.clone(),controller,transport,next,
                            crate::publication_recovery::Settings{query_timeout_ms:1000,max_age_ms:5000,wall_unix_ms:wall_ms()?,calendar_lateness_ms:1000,scene_capacity:64},
                            crate::publication_recovery::Synchronization::OutputsAndCalibratedMotors,
                        ).await?;
                        Ok::<_,String>((revision,next,runtime,fresh))
                    };
                    match tokio::time::timeout_at(deadline,transition).await {
                        Ok(Ok((revision,next,runtime,fresh)))=>{
                            owner=fresh;
                            epoch=next;
                            endpoints=candidate.gateways().iter().map(|(id,g)|(id.clone(),g.endpoint)).collect();
                            config=candidate;
                            if let (Some(settings),Some(identity))=(&hap_settings,&identity) {hap=crate::hap_endpoint::Endpoint::start(settings,identity,&admission,controller,&runtime,owner.snapshot(),management.is_some() || management_settings.is_some()).await?;}
                            if let Some(plan)=&hap_plan {bridges=Some(crate::hap_bridge_group::Group::start(plan,directory,&admission,controller,&runtime,owner.snapshot()).await?);}
                            if let Some(endpoint)=&management {endpoint.install(&admission,controller,&runtime).await?;}
                            prepared_runtime=Some(runtime);
                            request.complete(Ok(revision));
                        }
                        failure=>{
                            admission.stop_now();
                            let error=match failure {Ok(Err(error))=>format!("publication failed; inspect active revision before retrying: {error}"),_=>"publication exceeded service deadline; inspect active revision".into()};
                            request.complete(Err(error.clone()));
                            return Err(error);
                        }
                    }
                },
                Exit::Disconnected {gateway} => {
                    if recoveries>=recovery_limit {return Err(format!("gateway disconnected: {gateway}; recovery limit reached"));}
                    recoveries+=1;
                    log::info!("automation recovery={recoveries} gateway={gateway:?} previous_epoch={epoch}");
                    let recovery=async {
                        let (transport,next)=reconnect_tcp(admission.clone(),endpoints.clone(),epoch,RetryPolicy::default(),true,receiver.clone()).await?;
                        epoch=next;
                        owner.recover(transport,epoch)?;
                        owner.query_recovery_with_policy(&config,ExecutionContext::Live,1000,5000,crate::observed_transport::Synchronization::OutputsAndCalibratedMotors).await
                    };
                    tokio::select! {
                        result=recovery=>result?,
                        _=tokio::time::sleep_until(deadline)=>return Ok(()),
                    }
                }
            }
        }
    };
    let result=crate::daily_backup::supervise(path.clone(),ExecutionContext::Live,result).await;
    admission.stop_now();
    let shutdown = admission
        .shutdown()
        .await
        .map_err(|e| format!("automation shutdown: {e:?}"));
    result.and(shutdown)
}

/// Validate both documents and the TCP synchronization policy without opening
/// storage or connections. Publication coordinators must call this before
/// cancelling the old runtime; semantic validity alone does not prove that the
/// new mapping can pass the feedback recovery gate.
pub fn validate_candidate(
    devices: &[u8],
    rules: &[u8],
) -> Result<homesense_config::Configuration, String> {
    validate_candidate_with_write_interval(devices,rules,Duration::from_millis(20))
}
/// Reject individually impossible enabled fades before storage or transport I/O.
/// Concurrent allocations still require the runtime's shared reservation budget.
pub fn validate_candidate_with_write_interval(
    devices: &[u8], rules: &[u8], write_interval: Duration,
) -> Result<homesense_config::Configuration, String> {
    validate_candidate_in_context(devices,rules,write_interval,ExecutionContext::Live)
}
pub(crate) fn validate_candidate_in_context(devices:&[u8],rules:&[u8],write_interval:Duration,context:ExecutionContext)->Result<homesense_config::Configuration,String> {
    if !(Duration::from_millis(20)..=Duration::from_secs(1)).contains(&write_interval) {
        return Err("write interval must be 20..1000 ms".into());
    }
    let config = homesense_config::Configuration::parse(devices, context)
        .map_err(|e| e.to_string())?;
    let document = crate::configuration_publish::validate_automation(&config, rules, context)
        .map_err(|e| e.to_string())?;
    let budget = crate::fade_budget::Budget::for_write_interval(write_interval);
    for (id, scene) in &document.timed_scenes {
        if !scene.enabled { continue; }
        for (index, timing) in &scene.fades {
            let device = config.devices().get(&scene.steps[*index].device).ok_or("fade device missing")?;
            let frames = if device.supply_relay.is_some() { 2 } else { 1 };
            let samples = timing.samples().map_err(|e|e.to_string())?;
            // Drop each trial reservation: this checks one step, not an assumed
            // simultaneous execution of every configured scene.
            drop(budget.reserve(samples * frames, 1000u64.div_ceil(timing.interval_ms) * frames)
                .map_err(|e|format!("scene {id} fade step {index}: {e}"))?);
        }
    }
    if config.gateways().is_empty()
        || !config
            .devices()
            .keys()
            .any(|id| config.binding(id).is_some())
    {
        return Err("enabled verified devices required".into());
    }
    // Reject unsupported synchronization before entering a publication barrier.
    let mut outputs = 0;
    for id in config.devices().keys() {
        if let Some((device, module)) = config.binding(id) {
            if context == ExecutionContext::Live && module.kind() == homesense_core::ModuleKind::Keypad {
                module.validate_context(context).map_err(|e|e.to_string())?;
                module.validate_channel(device.channel).map_err(|e|e.to_string())?;
                continue;
            }
            if module.kind() == homesense_core::ModuleKind::Motor {
                let stop=config.motor_command(id,homesense_core::MotorAction::Stop).map_err(|e|e.to_string())?;
                hb_protocol::encode_in_context(module,device.channel,stop,context).map_err(|e|e.to_string())?;
                continue;
            }
            outputs += 1;
            hb_protocol::encode_in_context(
                module,
                device.channel,
                homesense_core::Command::Query,
                context,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    if outputs == 0 {
        return Err("automation TCP requires at least one queryable output".into());
    }
    Ok(config)
}

#[cfg(test)]
#[path = "automation_tcp_fault_tests.rs"]
pub(crate) mod fault_tests;

#[cfg(test)]
#[path = "automation_tcp_bridge_tests.rs"]
mod bridge_tests;
