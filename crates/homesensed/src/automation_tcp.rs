//! Explicit finite or continuously supervised automation with shared Web/HAP endpoints.
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
/// Transport loss gates controls while the established service stays available;
/// fresh recovery suppresses startup rules and never replays old commands.
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
/// Bounded retry rounds after an explicitly classified, fully cleaned disconnect.
/// `recovery_limit` selects the fast retry budget, not a service lifetime cap.
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
/// Optional endpoints owned by the same automation supervisor.
#[derive(Default)]
pub struct Endpoints {
    pub management:Option<crate::service::WebSettings>,
    pub hap:Option<crate::hap_endpoint::Settings>,
    pub hap_plan:Option<Vec<u8>>,
}
/// Explicit finite or forever mode with a graceful external stop channel.
#[allow(clippy::too_many_arguments)]
pub async fn serve_running(devices:&[u8],rules:&[u8],directory:&Path,control:crate::lifecycle::RunControl,coexistence_confirmed:bool,recovery_limit:u8,write_interval:Duration,endpoints:Endpoints)->Result<(),String>{
    if let Some(web)=&endpoints.management {if let Some(tls)=&web.tls {tls.validate_address(web.address)?;}else if !web.address.ip().is_loopback() || web.address.port()==0{return Err("plaintext management requires loopback".into());}}
    if endpoints.hap.is_some() && endpoints.management.as_ref().is_some_and(|web|Some(web.address)==endpoints.hap.as_ref().map(|hap|hap.address)){return Err("HAP and Web require distinct endpoints".into());}
    let (publisher,publications)=if endpoints.management.is_some(){let (publisher,receiver)=crate::automation_publication::channel();(Some(publisher),Some(receiver))}else{(None,None)};
    serve_inner_running(devices,rules,directory,control,coexistence_confirmed,recovery_limit,publications,write_interval,endpoints.management,publisher,endpoints.hap,endpoints.hap_plan,#[cfg(test)] None).await
}
fn validate_bridge_plan(plan:&[u8],config:&homesense_config::Configuration,rules:&[u8],web:Option<std::net::SocketAddr>)->Result<(),String> {
    let document=crate::configuration_publish::validate_automation(config,rules,ExecutionContext::Live).map_err(|e|e.to_string())?;
    let parsed=crate::hap_bridge_plan::Plan::parse(plan,config,&document)?;
    if parsed.bridges.iter().any(|b|Some(b.address)==web) {return Err("HAP and Web require distinct endpoints".into());}
    Ok(())
}
pub(crate) enum HandoffPublicationFailure {
    Deadline,
    Admission(hap::server::WriteStatus),
}
/// Called while the coordinator owns its runtime quantum permit. The same
/// deadline check is used by live TCP and scheduled simulation publication.
pub(crate) async fn begin_handoff_before_deadline(
    admission:&Admission, devices:&[u8], rules:&[u8], expected:u64,
    deadline:tokio::time::Instant,
) -> Result<(),HandoffPublicationFailure> {
    begin_authorized_handoff_before_deadline(admission,devices,rules,expected,deadline,None).await
}
pub(crate) async fn begin_authorized_handoff_before_deadline(admission:&Admission,devices:&[u8],rules:&[u8],expected:u64,deadline:tokio::time::Instant,source:Option<crate::publication_auth::Source>)->Result<(),HandoffPublicationFailure> {
    if tokio::time::Instant::now()>=deadline {return Err(HandoffPublicationFailure::Deadline);}
    admission.begin_authorized_automation_publication(devices.to_vec(),rules.to_vec(),expected,Duration::from_secs(3),source).await
        .map_err(HandoffPublicationFailure::Admission)
}
#[allow(clippy::too_many_arguments)]
async fn serve_inner(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,coexistence_confirmed:bool,recovery_limit:u8,publications:Option<crate::automation_publication::Receiver>,write_interval:Duration,management_settings:Option<crate::service::WebSettings>,publisher:Option<crate::automation_publication::Publisher>,hap_settings:Option<crate::hap_endpoint::Settings>,hap_plan:Option<Vec<u8>>,#[cfg(test)] hap_failure_after:Option<Duration>)->Result<(),String> {
    let (_hold,stop)=tokio::sync::watch::channel(false);
    serve_inner_running(devices,rules,directory,crate::lifecycle::RunControl::new(crate::lifecycle::RunMode::For(duration),stop),coexistence_confirmed,recovery_limit,publications,write_interval,management_settings,publisher,hap_settings,hap_plan,#[cfg(test)] hap_failure_after).await
}
#[allow(clippy::too_many_arguments)]
async fn serve_inner_running(devices:&[u8],rules:&[u8],directory:&Path,control:crate::lifecycle::RunControl,coexistence_confirmed:bool,recovery_limit:u8,mut publications:Option<crate::automation_publication::Receiver>,write_interval:Duration,mut management_settings:Option<crate::service::WebSettings>,publisher:Option<crate::automation_publication::Publisher>,hap_settings:Option<crate::hap_endpoint::Settings>,hap_plan:Option<Vec<u8>>,#[cfg(test)] hap_failure_after:Option<Duration>)->Result<(),String> {
    #[cfg(test)] let mut stop_after_barrier=control.stop_after_barrier.clone();
    #[cfg(test)] let mut queue_stop_before_barrier=control.queue_stop_before_barrier.clone();
    let receipt_policy=management_settings.as_ref().map(|_|crate::web_receipt_policy::Policy::load(directory)).transpose()?;
    if !(Duration::from_millis(20)..=Duration::from_secs(1)).contains(&write_interval) {
        return Err("write interval must be 20..1000 ms".into());
    }
    if recovery_limit > 16 {
        return Err("recovery limit must be 0..16".into());
    }
    control.mode.validate()?;
    if !coexistence_confirmed {return Err("automation TCP requires confirmed coexistence".into());}
    if control.stopped(){return Ok(());}
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
    let daily_backup=crate::daily_backup::Monitor::default();
    let result = async {
        let receiver=control.stop.clone();
        let mut endpoints: std::collections::BTreeMap<_,_> = config
            .gateways()
            .iter()
            .map(|(id, g)| (id.clone(), g.endpoint))
            .collect();
        let (mut owner,mut epoch)=if matches!(control.mode,crate::lifecycle::RunMode::Forever){
            let synchronized=crate::runtime_transport_recovery::until_ready_with_lifetime(admission.clone(),&config,endpoints.clone(),1,recovery_limit,crate::lifecycle::Deadline::start(control.mode),receiver.clone()).await?;
            let Some(synchronized)=synchronized else{return Ok(());};synchronized
        }else{
        let connected=tokio::select! {
            biased;
            _=crate::lifecycle::wait_for_stop(receiver.clone())=>None,
            result=reconnect_tcp(
            admission.clone(),
            endpoints.clone(),
            1,
            RetryPolicy::default(),
            true,
            receiver.clone(),
            )=>Some(result),
        };
        let Some(connected)=connected else{return Ok(());};
        let (transport,epoch)=connected?;
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Live,
            Uuid::parse_str(admission.boot_id())
                .map_err(|e| e.to_string())?
                .as_u128(),
            epoch,
        )?;
        let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
        tokio::select! {
            biased;
            _=crate::lifecycle::wait_for_stop(receiver.clone())=>return Ok(()),
            result=owner.query_recovery_with_policy(&config, ExecutionContext::Live, 1000, 5000, crate::observed_transport::Synchronization::OutputsAndCalibratedMotors)=>result?,
        }
            (owner,epoch)
        };
        let deadline=crate::lifecycle::Deadline::start(control.mode);
        let mut recoveries=0u64;
        let mut prepared_runtime = None;
        let mut hap:Option<crate::hap_endpoint::Endpoint>=None;
        let mut bridges:Option<crate::hap_bridge_group::Group>=None;
        let mut management:Option<crate::management_endpoint::Endpoint>=None;
        loop {
            if control.stopped() || deadline.expired(){return Ok(());}
            use crate::{automation_drive::Exit,automation_runtime::StartupPolicy};
            let startup=if recoveries==0 {StartupPolicy::Run} else {StartupPolicy::Suppress};
            let (runtime_stop, runtime_receive) = tokio::sync::watch::channel(false);
            let mut pending = None;
            if config.gateways().len()>1 && !owner.independent() {
                let destinations=endpoints.clone();
                let connector:crate::transport_owner::Connector<std::net::TcpStream>=std::sync::Arc::new(move|id,epoch| {
                    let address=destinations.get(id).copied().ok_or_else(||std::io::Error::new(std::io::ErrorKind::InvalidInput,"unknown configured gateway"))?;
                    hb_transport::Connection::connect(address,epoch,Duration::from_secs(2),Duration::from_millis(100))
                });
                owner.enable_independent(&config,connector).await?;
            }
            let mut runtime=match prepared_runtime.take() {
                Some(runtime) => runtime,
                None => AutomationRuntime::load_with_startup(admission.clone(),controller,wall_ms()?,1000,64,Default::default(),startup).await?,
            };
            owner.configure_output_refresh(&config,controller,admission.task_generation().await.map_err(|e|format!("{e:?}"))?,ExecutionContext::Live,1000,5000)?;
            if hap.is_none() && let (Some(settings),Some(identity))=(&hap_settings,&identity) {hap=crate::hap_endpoint::Endpoint::start(settings,identity,&admission,controller,&runtime,owner.snapshot(),management.is_some() || management_settings.is_some()).await?;}
            if bridges.is_none() && let Some(plan)=&hap_plan {bridges=Some(crate::hap_bridge_group::Group::start(plan,directory,&admission,controller,&runtime,owner.snapshot()).await?);}
            #[cfg(test)]
            if let (Some(delay),Some(endpoint))=(hap_failure_after,&hap) {
                let handle=endpoint.abort_handle();
                tokio::spawn(async move {tokio::time::sleep(delay).await;handle.abort();});
            }
            if let Some(settings)=management_settings.take() {
                management=Some(crate::management_endpoint::Endpoint::start((settings,receipt_policy.clone().ok_or("management receipt policy missing")?),directory,&admission,controller,&runtime,publisher.clone().ok_or("management publisher missing")?,daily_backup.clone()).await?);
            } else if let Some(endpoint)=&management {endpoint.install(&admission,controller,&runtime).await?;}
            if let Some(endpoint)=&management {endpoint.set_hap_status(if hap.is_some(){crate::web_api::HapStatus::Listening}else if hap_settings.is_some(){crate::web_api::HapStatus::Degraded}else{crate::web_api::HapStatus::Disabled})?;}
            if let (Some(endpoint),Some(group))=(&management,&bridges) {endpoint.set_hap_bridges(group.statuses.borrow().clone())?;}
            // Initialization may span async identity/listener setup. A stop that
            // arrived there must not start a fresh runtime/startup rule quantum.
            if control.stopped() || deadline.expired(){return Ok(());}
            let handoff_gate=runtime.handoff_gate();
            let outcome={
                let (ready,mut started)=tokio::sync::oneshot::channel();
                let mut running=false;
                let drive=runtime.drive_recoverable_published(&mut owner,Duration::from_millis(10),5000,runtime_receive,wall_ms,Some(ready),hap.as_ref().map(|endpoint|endpoint.projection.clone()).or_else(||bridges.as_ref().map(|group|group.projection.clone())),bridges.as_ref().map(|group|group.panel_feeds.clone()).unwrap_or_default());
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
                        _=deadline.wait()=>{let _=runtime_stop.send(true);break drive.await},
                        _=crate::lifecycle::wait_for_stop(receiver.clone())=>{let _=runtime_stop.send(true);break drive.await},
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
                                if document.scene_catalog().len()>64 {request.complete(Err("management scene capacity exceeded".into()));continue;}
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
                            if control.stopped() {
                                if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                request.complete(Err("service stopped before publication barrier".into()));
                                let _=runtime_stop.send(true);break drive.await;
                            }
                            if backup.is_err() || request.abandoned() {
                                if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                request.complete(Err("backup failed or publication abandoned".into()));continue;
                            }
                            // Continue polling the borrowed drive while waiting for its
                            // current quantum; otherwise this lock could deadlock it.
                            let quantum=tokio::select! {
                                permit=handoff_gate.clone().lock_owned()=>permit,
                                outcome=&mut drive=>{
                                    request.complete(Err("runtime ended before publication barrier".into()));
                                    break outcome;
                                },
                                _=crate::lifecycle::wait_for_stop(receiver.clone())=>{
                                    request.complete(Err("service stopped before publication barrier".into()));
                                    let _=runtime_stop.send(true);break drive.await;
                                },
                                _=deadline.wait()=>{
                                    request.complete(Err("service deadline reached before publication barrier".into()));
                                    let _=runtime_stop.send(true);
                                    break drive.await;
                                },
                            };
                            if control.stopped() {
                                drop(quantum);request.complete(Err("service stopped before publication barrier".into()));
                                let _=runtime_stop.send(true);break drive.await;
                            }
                            if request.abandoned() {
                                drop(quantum);
                                if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                request.complete(Err("publication abandoned before barrier".into()));continue;
                            }
                            // Test-only scheduling boundary: make the safety STOP an
                            // actual accepted ticket before publication adopts it.
                            #[cfg(test)]
                            if let Some(device)=queue_stop_before_barrier.take(){
                                let ids=admission.submit_motor_for_revision(config.revision(),controller,device,homesense_core::MotorAction::Stop).await.map_err(|e|format!("synthetic queued STOP: {e:?}"))?;
                                if ids.len()!=1{return Err("synthetic STOP was not accepted".into());}
                            }
                            match request.begin_with_lifetime(&admission,deadline).await {
                                Ok(())=>{},
                                Err(HandoffPublicationFailure::Deadline)=>{
                                    request.complete(Err("service deadline reached before publication barrier".into()));
                                    let _=runtime_stop.send(true);drop(quantum);break drive.await;
                                },
                                Err(HandoffPublicationFailure::Admission(error))=>{
                                    if let Some(endpoint)=&management {endpoint.restore(previous)?;}
                                    request.complete(Err(format!("publication rejected: {error:?}")));continue;
                                },
                            }
                            #[cfg(test)]
                            if let Some(stop)=stop_after_barrier.take(){stop.send_replace(true);}
                            pending=Some((request,candidate));
                            let _=runtime_stop.send(true);
                            drop(quantum);
                            break drive.await;
                        }
                    }
                }
            };
            let outcome=match outcome {
                Ok(outcome)=>outcome,
                Err(error)=>{
                    if let Some((request,_))=pending.take() {
                        request.complete(Err(format!("publication runtime handoff failed; inspect active revision before retrying: {error}")));
                    }
                    return Err(error);
                }
            };
            let unavailable_control=runtime.unavailable_control();
            if matches!(&outcome,Exit::Disconnected{..}|Exit::PublicationPaused) {
                if matches!(&outcome,Exit::Disconnected{..}) && let Some(endpoint)=&management {endpoint.unavailable(unavailable_control.clone())?;}
                let mut stale=owner.snapshot();
                for state in stale.devices.values_mut(){if state.quality==homesense_state::Quality::Confirmed{state.quality=homesense_state::Quality::Stale;}}
                // Existing HAP servers and identities survive the outage. Their
                // old command receivers are closed and the projection is stale.
                if let Some(endpoint)=&hap {endpoint.projection.send_replace(stale.clone());}
                if let Some(group)=&bridges {
                    group.projection.send_replace(stale);
                    if let Some(endpoint)=&management {let mut states=group.statuses.borrow().clone();for state in states.values_mut(){*state=crate::hap_bridge_group::Status::Degraded;}endpoint.set_hap_bridges(states)?;}
                }
            }else{
                if let Some(endpoint)=&management {endpoint.pause()?;if hap_settings.is_some() || hap_plan.is_some(){endpoint.set_hap_status(crate::web_api::HapStatus::Reconfiguring)?;}}
                if let Some(endpoint)=hap.take() {endpoint.shutdown().await;}
                if let Some(group)=bridges.take() {group.shutdown().await;}
            }
            drop(runtime);
            match outcome {
                Exit::Stopped => return Ok(()),
                Exit::PublicationPaused => {
                    let (request, candidate)=pending.take().ok_or("publication paused without request")?;
                    // The accepted handoff still owns its reserved STOPs and
                    // atomic durable commit. Only network recovery is retried.
                    let commit=async {
                        while owner.drain_publication(&admission).await?.is_some() {}
                        admission.commit_publication().await.map_err(|e|format!("publication commit: {e:?}"))
                    };
                    let revision=match tokio::time::timeout_at(deadline.operation(Duration::from_secs(30)),commit).await {
                        Ok(Ok(revision))=>revision,
                        failure=>{
                            admission.stop_now();
                            let error=match failure{Ok(Err(error))=>format!("publication failed; inspect active revision before retrying: {error}"),_=>"publication exceeded service deadline; inspect active revision".into()};
                            request.complete(Err(error.clone()));return Err(error);
                        }
                    };
                    endpoints=candidate.gateways().iter().map(|(id,g)|(id.clone(),g.endpoint)).collect();
                    config=candidate;
                    owner.invalidate();
                    drop(owner); // Retire old streams after reserved STOP drain and durable commit.
                    // Every error after the durable commit must complete the accepted
                    // request explicitly: a lost reply must not hide a committed revision.
                    let activation=async {
                        if control.stopped(){return Ok(None);}
                        if let Some(endpoint)=&management {endpoint.unavailable_mapping(&admission,controller,unavailable_control,&config,request.rules()).await?;}
                        let recovery=crate::runtime_transport_recovery::until_ready_with_lifetime(admission.clone(),&config,endpoints.clone(),epoch,recovery_limit,deadline,receiver.clone());
                        let recovered=tokio::select! {
                            result=recovery=>result?,
                            result=async{match management.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>return Err(result.err().unwrap_or_else(||"management listener ended during publication recovery".into())),
                        };
                        let Some((fresh,next))=recovered else{return Ok(None);};
                        if control.stopped(){return Ok(None);}
                        let failed_round=next!=epoch.checked_add(1).ok_or("transport epoch exhausted")?;
                        let startup=if failed_round{StartupPolicy::Suppress}else{StartupPolicy::Run};
                        let runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,wall_ms()?,1000,64,Default::default(),startup).await?;
                        if let Some(endpoint)=hap.take(){endpoint.shutdown().await;}
                        if let Some(group)=bridges.take(){group.shutdown().await;}
                        if let (Some(settings),Some(identity))=(&hap_settings,&identity) {hap=crate::hap_endpoint::Endpoint::start(settings,identity,&admission,controller,&runtime,fresh.snapshot(),management.is_some() || management_settings.is_some()).await?;}
                        if let Some(plan)=&hap_plan {bridges=Some(crate::hap_bridge_group::Group::start(plan,directory,&admission,controller,&runtime,fresh.snapshot()).await?);}
                        if let Some(endpoint)=&management {endpoint.install(&admission,controller,&runtime).await?;}
                        Ok(Some((fresh,next,runtime)))
                    }.await;
                    match activation {
                        Ok(Some((fresh,next,runtime)))=>{
                            owner=fresh;epoch=next;prepared_runtime=Some(runtime);
                            request.complete(Ok(revision));
                        },
                        Ok(None)=>{
                            request.complete(Err(format!("publication committed revision {revision}, but feedback unavailable before service deadline or explicit shutdown; inspect active revision before retrying")));
                            return Ok(());
                        },
                        Err(error)=>{
                            let error=format!("publication committed revision {revision}, but runtime activation failed; inspect active revision before retrying: {error}");
                            request.complete(Err(error.clone()));
                            return Err(error);
                        },
                    }
                },
                Exit::Disconnected {gateway} => {
                    recoveries=recoveries.checked_add(1).ok_or("recovery counter exhausted")?;
                    log::warn!("automation_unavailable outage={recoveries} gateway={gateway:?} previous_epoch={epoch}; listeners retained, controls gated");
                    drop(owner); // Snapshots are owned by listeners; dead streams must not retain gateway slots.
                    let recovery=crate::runtime_transport_recovery::until_ready_with_lifetime(admission.clone(),&config,endpoints.clone(),epoch,recovery_limit,deadline,receiver.clone());
                    let recovered=tokio::select! {
                        result=recovery=>result?,
                        result=async {match management.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>return Err(result.err().unwrap_or_else(||"management listener ended during recovery".into())),
                    };
                    let Some((fresh,next))=recovered else{return Ok(());};
                    owner=fresh;epoch=next;
                    // Rebind runtime request channels only after full fresh truth.
                    // Pairing databases remain the same; startup is suppressed.
                    if let Some(endpoint)=hap.take(){endpoint.shutdown().await;}
                    if let Some(group)=bridges.take(){group.shutdown().await;}
                }
            }
        }
    };
    let result=crate::daily_backup::supervise_monitored(path.clone(),ExecutionContext::Live,result,daily_backup.clone()).await;
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
    if config.gateways().is_empty() {
        return Err("registered gateways required".into());
    }
    let targets = crate::physical_queries::plan(&config, context)?;
    crate::physical_queries::interval(&targets, write_interval.as_millis() as u64, 5000)?;
    // Reject unsupported synchronization before entering a publication barrier.
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
            hb_protocol::encode_in_context(
                module,
                device.channel,
                homesense_core::Command::Query,
                context,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(config)
}

#[cfg(test)]
#[path = "automation_tcp_fault_tests.rs"]
pub(crate) mod fault_tests;

#[cfg(test)]
#[path = "automation_tcp_bridge_tests.rs"]
mod bridge_tests;

#[cfg(test)]
#[path="automation_tcp_lifecycle_tests.rs"]
mod lifecycle_tests;
