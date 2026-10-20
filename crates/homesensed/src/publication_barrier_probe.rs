//! Deterministic actor/scheduler/router ordering probe. Synthetic SQLite only.
//! It does not change production code or connect any gateway.
use homesense_core::{ExecutionContext, rule::Budget};
use crate::{admission::{Admission,Options},deferred_router::Router,scene_scheduler::SceneScheduler};
use serde_json::json;
use std::time::{Duration,Instant};
use uuid::Uuid;
#[tokio::test]
async fn actual_publication_barrier_during_causal_scene_poll_poisoned_owner_without_any_service_deadline() {
    let dir=std::env::temp_dir().join(format!("publication-race-probe-{}",Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let database=dir.join("runtime.sqlite");
    let devices=include_bytes!("../../../config/simulation-home.json");
    let old_rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"startup","enabled":true,"review_evidence":"synthetic","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"remember","action":{"kind":"set","variable":"remember","value":{"kind":"integer","value":17}}},{"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}]}]})).unwrap();
    homesense_storage::Store::open(&database).unwrap().publish_automation(None,devices,&old_rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let actor=Admission::open(database.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();
    actor.synchronized(1,1).await.unwrap();
    let generation=actor.task_generation().await.unwrap();
    let mut scheduler=SceneScheduler::new(actor.clone(),controller,8,actor.monotonic_ms()).unwrap();
    let mut router=Router::new(actor.clone(),controller,&scheduler,8).unwrap();
    router.start("startup",&mut scheduler,None,Budget::default(),60000).await.unwrap();
    assert_eq!(router.retained_count(),1);
    // Ordinary polling before the barrier is a positive control. The future
    // output remains 60 seconds away, so no action or request TTL can expire.
    router.poll(&mut scheduler,None).await.unwrap();
    let mut next:serde_json::Value=serde_json::from_slice(devices).unwrap();next["revision"]=json!(2);
    let new_rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":2,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],"rules":[]})).unwrap();
    let started=Instant::now();
    actor.begin_automation_publication(serde_json::to_vec(&next).unwrap(),new_rules,1,Duration::from_secs(30)).await.unwrap();
    let before=actor.diagnostics(controller,generation.clone()).await.unwrap();
    assert!(before.publishing && !before.failed && !before.stopping);
    let result=router.poll(&mut scheduler,None).await;
    let after=actor.diagnostics(controller,generation.clone()).await.unwrap();
    let committed=actor.commit_publication().await;
    let sql=rusqlite::Connection::open(&database).unwrap();
    let commands:i64=sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get(0)).unwrap();
    let remembered:Option<i64>=sql.query_row("SELECT value FROM logical_variable WHERE revision=1 AND name='remember'",[],|r|r.get(0)).unwrap();
    let active=homesense_storage::Store::open(&database).unwrap().active().unwrap().unwrap().revision;
    eprintln!("{}",json!({"before":{"publishing":before.publishing,"failed":before.failed,"stopping":before.stopping},"poll_error":result.as_ref().err(),"after":{"publishing":after.publishing,"failed":after.failed,"stopping":after.stopping},"commit_error":format!("{:?}",committed.as_ref().err()),"elapsed_ms":started.elapsed().as_millis(),"command_count":commands,"remembered":remembered,"active_revision":active,"service_deadline":null,"scene_offset_ms":60000,"barrier_timeout_ms":30000}));
    assert_eq!(result.unwrap_err(),"ServiceCommunicationFailure");
    assert!(after.failed && after.stopping);
    assert!(committed.is_err());
    assert_eq!(commands,0);
    assert_eq!(remembered,Some(17));
    assert_eq!(active,1);
    assert!(started.elapsed()<Duration::from_secs(3),"must not involve the 30-second publication timeout");
    drop(sql);drop(router);drop(scheduler);actor.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
}

struct BlockingMemory {
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    fault: bool,
    release: std::sync::mpsc::Receiver<()>,
    writes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}
impl std::io::Read for BlockingMemory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ready)=self.ready.take() {
            ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fault { return Err(std::io::ErrorKind::PermissionDenied.into()); }
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for BlockingMemory {
    fn write(&mut self, bytes:&[u8])->std::io::Result<usize>{self.writes.lock().unwrap().extend_from_slice(bytes);Ok(bytes.len())}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
#[tokio::test]
async fn actual_runtime_quantum_lease_keeps_publication_out_of_causal_poll_and_cancels_future_run_before_commit() {
    use crate::{automation_runtime::{AutomationRuntime,StartupPolicy},feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let dir=std::env::temp_dir().join(format!("publication-quantum-probe-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let database=dir.join("runtime.sqlite");
    let devices=include_bytes!("../../../config/simulation-home.json");
    let old_rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"startup","enabled":true,"review_evidence":"synthetic","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"remember","action":{"kind":"set","variable":"remember","value":{"kind":"integer","value":17}}},{"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}]}]})).unwrap();
    let bundle=homesense_storage::Store::open(&database).unwrap().publish_automation(None,devices,&old_rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let actor=Admission::open(database.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();
    let mut runtime=AutomationRuntime::load_with_startup(actor.clone(),controller,1_800_000_000_000,1000,8,Budget::default(),StartupPolicy::Run).await.unwrap();
    let gate=runtime.handoff_gate();let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let (ready,blocked)=tokio::sync::oneshot::channel();let (release,receive)=std::sync::mpsc::channel();
    let feedback=Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
    let mut owner=ObservedTransport::new(TransportOwner::new(actor.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(BlockingMemory{ready:Some(ready),fault:false,release:receive,writes:writes.clone()},1))].into()),feedback);
    let (stop,stopped)=tokio::sync::watch::channel(false);
    let driver=runtime.drive_recoverable(&mut owner,Duration::from_millis(1),5000,stopped,||Ok(1_800_000_000_000));
    let publisher=async {
        blocked.await.unwrap();
        let permit=gate.lock_owned();tokio::pin!(permit);
        assert!(futures_util::poll!(permit.as_mut()).is_pending(),"publication must wait for the in-flight runtime quantum");
        let before=actor.diagnostics(controller,generation.clone()).await.unwrap();assert!(!before.publishing && !before.failed && !before.stopping);
        release.send(()).unwrap();
        let permit=permit.await;
        let mut next:serde_json::Value=serde_json::from_slice(devices).unwrap();next["revision"]=json!(2);
        let new_rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":2,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],"rules":[]})).unwrap();
        actor.begin_automation_publication(serde_json::to_vec(&next).unwrap(),new_rules,1,Duration::from_secs(30)).await.unwrap();stop.send(true).unwrap();drop(permit);
    };
    let (outcome,())=tokio::time::timeout(Duration::from_secs(10),async {tokio::join!(driver,publisher)}).await.unwrap();
    assert_eq!(outcome.unwrap(),crate::automation_drive::Exit::PublicationPaused);
    let after=actor.diagnostics(controller,generation).await.unwrap();assert!(after.publishing && !after.failed && !after.stopping);
    assert_eq!(actor.commit_publication().await.unwrap(),2);
    let sql=rusqlite::Connection::open(&database).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run WHERE definition='later' AND status='cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(sql.query_row("SELECT value FROM logical_variable WHERE revision=2 AND name='remember'",[],|r|r.get::<_,i64>(0)).unwrap(),17);
    assert!(writes.lock().unwrap().is_empty());
    eprintln!("{}",json!({"gate_wait_before_barrier":true,"exit":"PublicationPaused","commit_revision":2,"scene_cancelled":1,"command_count":0,"remembered":17,"guards_kept_failed_false_stopping_false":true}));
    drop(sql);drop(runtime);actor.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn abandoning_a_runtime_with_a_quantum_lease_still_fails_closed() {
    tokio::task::LocalSet::new().run_until(async {
    use crate::{automation_runtime::AutomationRuntime,feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let dir=std::env::temp_dir().join(format!("publication-quantum-abandon-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();
    let data=include_bytes!("../../../config/simulation-home.json");let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let database=dir.join("runtime.sqlite");let bundle=homesense_storage::Store::open(&database).unwrap().publish_automation(None,data,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let actor=Admission::open(database,Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();
    let mut runtime=AutomationRuntime::load(actor.clone(),controller,1_800_000_000_000,1000,8,Budget::default()).await.unwrap();let gate=runtime.handoff_gate();
    let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let (ready,blocked)=tokio::sync::oneshot::channel();let (release,receive)=std::sync::mpsc::channel();let feedback=Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
    let mut owner=ObservedTransport::new(TransportOwner::new(actor.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(BlockingMemory{ready:Some(ready),fault:false,release:receive,writes:writes.clone()},1))].into()),feedback);
    let (_stop,stopped)=tokio::sync::watch::channel(false);
    let handle=tokio::task::spawn_local(async move {runtime.drive_recoverable(&mut owner,Duration::from_millis(1),5000,stopped,||Ok(1_800_000_000_000)).await});
    tokio::time::timeout(Duration::from_secs(5),blocked).await.unwrap().unwrap();assert!(gate.try_lock().is_err());
    handle.abort();assert!(handle.await.unwrap_err().is_cancelled());release.send(()).unwrap();
    let after=actor.diagnostics(controller,generation).await.unwrap();assert!(after.failed && after.stopping);assert!(!after.publishing);assert!(gate.try_lock().is_ok());assert!(writes.lock().unwrap().is_empty());
    eprintln!("{}",json!({"future_aborted":true,"actor_failed":after.failed,"actor_stopping":after.stopping,"publication_active":after.publishing,"mutex_released":true,"wire_bytes":0}));
    actor.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
    }).await;
}

struct WebLab {
    dir: std::path::PathBuf,
    actor: Admission,
    controller: Uuid,
    generation: crate::admission::TaskGeneration,
    api: std::sync::Arc<crate::web_api::Api>,
    gate: std::sync::Arc<tokio::sync::Mutex<()>>,
    publisher: crate::automation_publication::Publisher,
    runtime: Option<crate::automation_runtime::AutomationRuntime>,
    owner: Option<crate::observed_transport::ObservedTransport<crate::service::SimulatedStream>>,
    receiver: Option<crate::automation_publication::Receiver>,
}
impl WebLab {
    async fn new()->Self {
        let dir=std::env::temp_dir().join(format!("publication-web-boundary-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let path=dir.join("runtime.sqlite");let devices=include_bytes!("../../../config/simulation-home.json");let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
        let bundle=homesense_storage::Store::open(&path).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();let controller=Uuid::new_v4();let actor=Admission::open(path,Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();
        let runtime=crate::automation_runtime::AutomationRuntime::load(actor.clone(),controller,1_800_000_000_000,1000,8,Budget::default()).await.unwrap();let gate=runtime.handoff_gate();let feedback=crate::feedback::Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();let owner=crate::observed_transport::ObservedTransport::new(crate::transport_owner::TransportOwner::new(actor.clone(),crate::service::simulation_connections_at(&bundle.configuration,1).unwrap()),feedback);let(publisher,receiver)=crate::automation_publication::channel();
        let api=std::sync::Arc::new(crate::web_api::Api{daily_backup:Default::default(),hap_placement:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),backup_source:None,publisher:Some(publisher.clone()),origin:"http://127.0.0.1:1".into(),tokens:std::sync::Arc::new(crate::web_auth::Tokens::default()),runtime:std::sync::RwLock::new(Some(crate::web_api::Runtime{control:runtime.control(),revision:1,boot:actor.boot_id().into(),scenes:Default::default()})),receipts:std::sync::Mutex::new(crate::web_receipts::Receipts::open(&dir.join("web.sqlite")).unwrap()),drafts:None});
        Self{dir,actor,controller,generation,api,gate,publisher,runtime:Some(runtime),owner:Some(owner),receiver:Some(receiver)}
    }
    fn spawn(&mut self,stop:tokio::sync::watch::Receiver<bool>)->tokio::task::JoinHandle<Result<(),String>> {
        let api=self.api.clone();let actor=self.actor.clone();let controller=self.controller;let runtime=self.runtime.take().unwrap();let owner=self.owner.take().unwrap();let receiver=self.receiver.take().unwrap();let path=self.dir.join("runtime.sqlite");
        tokio::task::spawn_local(crate::web_supervision::run(api,actor,controller,runtime,owner,receiver,stop,path))
    }
    async fn owner_waiting(&self) {
        // Only this controlled fixture holds the lease. Two actual waiter Arc
        // clones distinguish the driver and the supervisor after durable backup;
        // behavior assertions below still inspect real actor/SQLite/API state.
        tokio::time::timeout(Duration::from_secs(5),async{while std::sync::Arc::strong_count(&self.gate)<6 {tokio::task::yield_now().await;}}).await.unwrap();
    }
    async fn publication_waiting(&self) {
        tokio::time::timeout(Duration::from_secs(5),async{while std::sync::Arc::strong_count(&self.gate)<7 || self.api.runtime.read().unwrap().is_some() {tokio::task::yield_now().await;}}).await.unwrap();
        assert!(self.dir.join("publication-backups").read_dir().unwrap().next().is_some());
        let state=self.actor.diagnostics(self.controller,self.generation.clone()).await.unwrap();assert!(!state.publishing && !state.failed && !state.stopping);
    }
    fn publish(&self)->tokio::task::JoinHandle<Result<u64,String>> {
        let publisher=self.publisher.clone();let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();devices["revision"]=json!(2);let rules=br#"{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}"#.to_vec();tokio::task::spawn_local(async move{publisher.publish(serde_json::to_vec(&devices).unwrap(),rules,1).await})
    }
    fn assert_no_effect(&self) {
        let sql=rusqlite::Connection::open(self.dir.join("runtime.sqlite")).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);assert_eq!(homesense_storage::Store::open(self.dir.join("runtime.sqlite")).unwrap().active().unwrap().unwrap().revision,1);
    }
    async fn close(self) {self.actor.shutdown().await.unwrap();std::fs::remove_dir_all(self.dir).unwrap();}
}
#[tokio::test]
async fn actual_web_coordinator_abandoned_request_waiting_for_quantum_restores_old_view_without_barrier_or_effect() {
    tokio::task::LocalSet::new().run_until(async {
        let mut lab=WebLab::new().await;let held=lab.gate.clone().lock_owned().await;let(stop,receive)=tokio::sync::watch::channel(false);let supervisor=lab.spawn(receive);lab.owner_waiting().await;let publication=lab.publish();lab.publication_waiting().await;
        publication.abort();assert!(publication.await.unwrap_err().is_cancelled());drop(held);
        tokio::time::timeout(Duration::from_secs(5),async{while lab.api.runtime.read().unwrap().is_none(){tokio::task::yield_now().await;}}).await.unwrap();assert_eq!(lab.api.runtime.read().unwrap().as_ref().unwrap().revision,1);
        let state=lab.actor.diagnostics(lab.controller,lab.generation.clone()).await.unwrap();assert!(!state.publishing && !state.failed && !state.stopping);lab.assert_no_effect();stop.send(true).unwrap();tokio::time::timeout(Duration::from_secs(5),supervisor).await.unwrap().unwrap().unwrap();lab.assert_no_effect();
        eprintln!("{}",json!({"actual_web_coordinator":true,"abandoned_while_waiting":true,"view_restored_revision":1,"barrier_entered":false,"actor_healthy_before_stop":true,"command_count":0}));lab.close().await;
    }).await;
}
#[tokio::test]
async fn actual_web_coordinator_service_stop_while_waiting_for_quantum_exits_without_waiting_for_external_lease_release() {
    tokio::task::LocalSet::new().run_until(async {
        let mut lab=WebLab::new().await;let held=lab.gate.clone().lock_owned().await;let(stop,receive)=tokio::sync::watch::channel(false);let supervisor=lab.spawn(receive);lab.owner_waiting().await;let publication=lab.publish();lab.publication_waiting().await;
        stop.send(true).unwrap();let error=tokio::time::timeout(Duration::from_secs(5),publication).await.unwrap().unwrap().unwrap_err();assert_eq!(error,"service stopped before publication barrier");tokio::time::timeout(Duration::from_secs(5),supervisor).await.unwrap().unwrap().unwrap();
        // The lease remains held until both actual futures have exited.
        assert!(lab.gate.try_lock().is_err());lab.assert_no_effect();let state=lab.actor.diagnostics(lab.controller,lab.generation.clone()).await.unwrap();assert!(state.failed && state.stopping);assert!(!state.publishing);drop(held);
        eprintln!("{}",json!({"actual_web_coordinator":true,"stop_while_waiting":true,"publication_error":error,"exited_before_external_lease_release":true,"active_revision":1,"actor_failed":state.failed,"actor_stopping":state.stopping,"barrier_entered":false,"command_count":0}));lab.close().await;
    }).await;
}

#[tokio::test]
async fn unrelated_runtime_failure_closes_actor_before_a_waiting_publisher_can_enter_its_barrier() {
    use crate::{automation_runtime::AutomationRuntime,feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let dir=std::env::temp_dir().join(format!("publication-quantum-fault-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let database=dir.join("runtime.sqlite");let data=include_bytes!("../../../config/simulation-home.json");let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let bundle=homesense_storage::Store::open(&database).unwrap().publish_automation(None,data,rules,ExecutionContext::Simulation,&[]).unwrap();let controller=Uuid::new_v4();let actor=Admission::open(database.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();let mut runtime=AutomationRuntime::load(actor.clone(),controller,1_800_000_000_000,1000,8,Budget::default()).await.unwrap();let gate=runtime.handoff_gate();let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let(ready,blocked)=tokio::sync::oneshot::channel();let(release,receive)=std::sync::mpsc::channel();let feedback=Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();let mut owner=ObservedTransport::new(TransportOwner::new(actor.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(BlockingMemory{ready:Some(ready),fault:true,release:receive,writes:writes.clone()},1))].into()),feedback);let(_stop,stopped)=tokio::sync::watch::channel(false);
    let driver=runtime.drive_recoverable(&mut owner,Duration::from_millis(1),5000,stopped,||Ok(1_800_000_000_000));
    let publisher=async {
        blocked.await.unwrap();let permit=gate.lock_owned();tokio::pin!(permit);assert!(futures_util::poll!(permit.as_mut()).is_pending());release.send(()).unwrap();let permit=permit.await;
        let after=actor.diagnostics(controller,generation.clone()).await.unwrap();assert!(after.failed && after.stopping,"an unrelated error must fail closed before the publication waiter acquires its lease");let mut next:serde_json::Value=serde_json::from_slice(data).unwrap();next["revision"]=json!(2);let new_rules=br#"{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}"#.to_vec();let error=actor.begin_automation_publication(serde_json::to_vec(&next).unwrap(),new_rules,1,Duration::from_secs(30)).await.unwrap_err();assert_eq!(error,hap::server::WriteStatus::ServiceCommunicationFailure);drop(permit);
    };
    let(outcome,())=tokio::time::timeout(Duration::from_secs(10),async{tokio::join!(driver,publisher)}).await.unwrap();assert!(outcome.is_err());assert!(!runtime.publication_paused());assert!(writes.lock().unwrap().is_empty());assert_eq!(homesense_storage::Store::open(&database).unwrap().active().unwrap().unwrap().revision,1);eprintln!("{}",json!({"unrelated_runtime_error":outcome.err(),"failed_before_publication_lease_released":true,"publication_rejected":true,"active_revision":1,"wire_bytes":0}));drop(runtime);actor.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
}

type BeforeBarrierWrites = std::sync::Arc<std::sync::Mutex<Vec<(bool,Vec<u8>)>>>;
struct BeforeBarrierWire {
    barrier_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    writes: BeforeBarrierWrites,
}
impl std::io::Read for BeforeBarrierWire {
    fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}
}
impl std::io::Write for BeforeBarrierWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.writes.lock().unwrap().push((self.barrier_requested.load(std::sync::atomic::Ordering::SeqCst),bytes.to_vec()));Ok(bytes.len())}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
#[tokio::test]
async fn projected_first_frame_after_original100ms_scene_deadline_contains_valid_output_before_publication_is_requested() {
    use crate::{automation_runtime::AutomationRuntime,feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let dir=std::env::temp_dir().join(format!("publication-prebarrier-timing-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let database=dir.join("runtime.sqlite");let data=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    let bundle=homesense_storage::Store::open(&database).unwrap().publish_automation(None,data,rules,ExecutionContext::Simulation,&[]).unwrap();let controller=Uuid::new_v4();let actor=Admission::open(database,Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();actor.synchronized(1,1).await.unwrap();let mut runtime=AutomationRuntime::load(actor.clone(),controller,1_800_000_000_000,1000,8,Budget::default()).await.unwrap();let control=runtime.control();runtime.start_scene("light").await.unwrap();
    // This diagnostic-only delay models a late first drive/projection. It does
    // not change the original 100ms scene offset or any production timeout.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let barrier_requested=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let feedback=Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();let mut owner=ObservedTransport::new(TransportOwner::new(actor.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(BeforeBarrierWire{barrier_requested:barrier_requested.clone(),writes:writes.clone()},1))].into()),feedback);let(projection,mut updates)=tokio::sync::watch::channel(owner.snapshot());let(stop,stopped)=tokio::sync::watch::channel(false);
    let pause=async {
        tokio::time::timeout(Duration::from_secs(5),updates.changed()).await.unwrap().unwrap();let _subscription=control.subscribe("light".into()).await.unwrap();
        assert_eq!(writes.lock().unwrap().as_slice(),&[(false,vec![0xae,0x15,1,30,0xee])]);
        barrier_requested.store(true,std::sync::atomic::Ordering::SeqCst);
        let mut next:serde_json::Value=serde_json::from_slice(data).unwrap();next["revision"]=json!(2);let mut next_rules:serde_json::Value=serde_json::from_slice(rules).unwrap();next_rules["configuration_revision"]=json!(2);
        let result=actor.begin_automation_publication(serde_json::to_vec(&next).unwrap(),serde_json::to_vec(&next_rules).unwrap(),1,Duration::from_secs(3)).await;stop.send(true).unwrap();result
    };
    let driver=runtime.drive_recoverable_projected(&mut owner,Duration::from_millis(1),5000,stopped,||Ok(1_800_000_000_000),None,Some(projection));let(result,publication)=tokio::time::timeout(Duration::from_secs(10),async{tokio::join!(driver,pause)}).await.unwrap();
    assert_eq!(writes.lock().unwrap().as_slice(),&[(false,vec![0xae,0x15,1,30,0xee])]);eprintln!("{}",json!({"diagnostic_first_drive_delay_ms":150,"original_scene_offset_ms":100,"valid_write_before_barrier_request":true,"post_request_writes":0,"publication_result":format!("{publication:?}"),"driver_result":format!("{result:?}"),"production_timeouts_changed":false,"original_study_test_modified":false,"not_a_complete_cancellation_qualification":true}));drop(runtime);actor.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn root_service_stop_racing_released_quantum_cannot_commit_a_new_revision() {
    tokio::task::LocalSet::new().run_until(async {
        for attempt in 0..32 {
            let mut lab=WebLab::new().await;
            let held=lab.gate.clone().lock_owned().await;
            let(stop,receive)=tokio::sync::watch::channel(false);
            let supervisor=lab.spawn(receive);
            lab.owner_waiting().await;
            let publication=lab.publish();
            lab.publication_waiting().await;
            stop.send(true).unwrap();
            drop(held);
            let result=tokio::time::timeout(Duration::from_secs(5),publication).await.unwrap().unwrap();
            tokio::time::timeout(Duration::from_secs(5),supervisor).await.unwrap().unwrap().unwrap();
            let revision=homesense_storage::Store::open(lab.dir.join("runtime.sqlite")).unwrap().active().unwrap().unwrap().revision;
            eprintln!("{}",json!({"root_stop_and_lease_release_same_turn":true,"attempt":attempt,"publication":format!("{result:?}"),"active_revision":revision}));
            lab.assert_no_effect();
            assert!(result.is_err(),"a stopped coordinator must not accept publication");
            lab.close().await;
        }
    }).await;
}

#[tokio::test]
async fn root_stop_racing_a_publication_waiter_ahead_of_the_next_runtime_cycle() {
    tokio::task::LocalSet::new().run_until(async {
        for attempt in 0..32 {
            let mut lab=WebLab::new().await;
            let(stop,receive)=tokio::sync::watch::channel(false);
            let control=lab.api.runtime.read().unwrap().as_ref().unwrap().control.clone();
            let supervisor=lab.spawn(receive);
            control.request(crate::automation_control::Action::CalendarStatus).await.unwrap();
            let held=lab.gate.clone().lock_owned().await;
            let publication=lab.publish();
            lab.publication_waiting().await;
            stop.send(true).unwrap();
            drop(held);
            let result=tokio::time::timeout(Duration::from_secs(5),publication).await.unwrap().unwrap();
            tokio::time::timeout(Duration::from_secs(5),supervisor).await.unwrap().unwrap().unwrap();
            let revision=homesense_storage::Store::open(lab.dir.join("runtime.sqlite")).unwrap().active().unwrap().unwrap().revision;
            eprintln!("{}",json!({"root_stopped_publication_waiter_precedes_next_cycle":true,"attempt":attempt,"publication":format!("{result:?}"),"active_revision":revision}));
            lab.assert_no_effect();
            assert!(result.is_err(),"a stopped coordinator must not accept publication");
            lab.close().await;
        }
    }).await;
}

#[tokio::test]
async fn warm_closed_service_and_released_permit_cannot_publish_a_new_revision() {
    tokio::task::LocalSet::new().run_until(async {
        for _ in 0..32 {
            let mut lab=WebLab::new().await;let(stop,receive)=tokio::sync::watch::channel(false);let control=lab.api.runtime.read().unwrap().as_ref().unwrap().control.clone();let supervisor=lab.spawn(receive);control.request(crate::automation_control::Action::CalendarStatus).await.unwrap();let held=lab.gate.clone().lock_owned().await;let publication=lab.publish();lab.publication_waiting().await;
            drop(stop);drop(held);let result=tokio::time::timeout(Duration::from_secs(5),publication).await.unwrap().unwrap();tokio::time::timeout(Duration::from_secs(5),supervisor).await.unwrap().unwrap().unwrap();lab.assert_no_effect();assert!(result.is_err());lab.close().await;
        }
    }).await;
}
#[tokio::test]
async fn real_tcp_simulation_barrier_entry_rejects_expired_deadline_after_permit_without_mutation() {
    let lab=WebLab::new().await;let held=lab.gate.clone().lock_owned().await;let mut next:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();next["revision"]=json!(2);let data=serde_json::to_vec(&next).unwrap();let rules=br#"{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}"#;
    let expired=tokio::time::Instant::now()-Duration::from_secs(1);
    let result=crate::automation_tcp::begin_handoff_before_deadline(&lab.actor,&data,rules,1,expired).await;assert!(matches!(result,Err(crate::automation_tcp::HandoffPublicationFailure::Deadline)));lab.assert_no_effect();let state=lab.actor.diagnostics(lab.controller,lab.generation.clone()).await.unwrap();assert!(!state.publishing && !state.failed && !state.stopping);
    // Positive control: the identical production entry accepts the same valid
    // payload under a live deadline; only the expired attempt is rejected.
    assert!(crate::automation_tcp::begin_handoff_before_deadline(&lab.actor,&data,rules,1,tokio::time::Instant::now()+Duration::from_secs(30)).await.is_ok());let state=lab.actor.diagnostics(lab.controller,lab.generation.clone()).await.unwrap();assert!(state.publishing && !state.failed && !state.stopping);lab.assert_no_effect();lab.actor.abort_publication().await.unwrap();drop(held);lab.close().await;
}
