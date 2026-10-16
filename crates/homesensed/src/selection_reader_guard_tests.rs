use super::*;
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::sync::{Arc,Mutex,Condvar,atomic::{AtomicBool,Ordering}};

struct Fixture {
    directory:std::path::PathBuf,
    admission:Admission,
    control:Control,
    receiver:Receiver,
    document:Document,
    controller:Uuid,
    extra:Uuid,
    stop:tokio::sync::watch::Sender<bool>,
    authorized:Arc<AtomicBool>,
    hold_next:Arc<AtomicBool>,
    entered:Arc<tokio::sync::Notify>,
    release:Arc<(Mutex<bool>,Condvar)>,
}
impl Fixture {
    async fn new()->Self {
        let directory=std::env::temp_dir().join(format!("selection-reader-guards-{}",Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
        let devices=include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json");
        let rules=include_bytes!("../../../config/test/native-app-keypad-away/automation.json");
        let path=directory.join("runtime.sqlite");Store::open(&path).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let extra=Uuid::new_v4();let authorized=Arc::new(AtomicBool::new(true));let hold_next=Arc::new(AtomicBool::new(false));let entered=Arc::new(tokio::sync::Notify::new());let release=Arc::new((Mutex::new(false),Condvar::new()));
        let (permission,hold,started,gate)=(authorized.clone(),hold_next.clone(),entered.clone(),release.clone());
        let admission=Admission::open(path,crate::admission::Options {context:ExecutionContext::Simulation,..Default::default()},move|id|{
            if id==controller && hold.swap(false,Ordering::AcqRel) {
                started.notify_one();let (lock,wake)=&*gate;let mut released=lock.lock().unwrap();while !*released {released=wake.wait(released).unwrap();}
            }
            (id==controller && permission.load(Ordering::Acquire)) || id==extra
        }).await.unwrap();admission.synchronized(1,1).await.unwrap();
        let generation=admission.task_generation().await.unwrap();let bundle=admission.automation_bundle(controller,generation.clone()).await.unwrap();let document=bundle.automation.unwrap();
        let (control,mut receiver)=channel(controller,generation);let (stop,receive)=tokio::sync::watch::channel(false);receiver.bind_selection_reader(&admission,&document,receive).await.unwrap();
        Self {directory,admission,control,receiver,document,controller,extra,stop,authorized,hold_next,entered,release}
    }
    async fn read(&self,controller:Uuid,revision:u64)->Result<Reply,String>{self.control.request(Action::SceneSelectionDetails {controller,revision}).await}
    async fn finish(mut self) {
        self.receiver.close();self.admission.shutdown().await.unwrap();
        let sql=rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|row|row.get::<_,i64>(0)).unwrap(),0);drop(sql);std::fs::remove_dir_all(self.directory).unwrap();
    }
}
#[tokio::test]
async fn selection_reader_rechecks_both_controllers_revision_and_unavailable_instead_of_caching() {
    let mut f=Fixture::new().await;
    assert!(matches!(f.read(f.controller,1).await,Ok(Reply::SceneSelectionDetails(states)) if !states.is_empty()));
    assert!(f.read(Uuid::new_v4(),1).await.is_err());
    assert!(f.read(f.controller,2).await.is_err());
    f.authorized.store(false,Ordering::Release);
    assert!(f.read(f.extra,1).await.is_err(),"an authorized requested controller cannot replace the revoked base owner");
    f.authorized.store(true,Ordering::Release);
    let gated=f.control.unavailable(f.admission.clone(),f.receiver.generation.clone());
    assert!(gated.request(Action::SceneSelectionDetails {controller:f.controller,revision:1}).await.is_err());
    // A write remains on the original queue; the read capability cannot execute it.
    {
        let mut write=Box::pin(f.control.request(Action::SceneAction {controller:f.controller,revision:1,scene:"lights-on".into(),execute:true}));
        assert!(tokio::time::timeout(Duration::from_millis(50),&mut write).await.is_err());
    }
    f.receiver.close();
    assert!(f.read(f.controller,1).await.is_err(),"closed owner cannot return its earlier valid selection");
    f.finish().await;
}
#[tokio::test]
async fn selection_reader_old_transport_generation_cannot_borrow_a_new_channel() {
    let mut f=Fixture::new().await;assert!(f.read(f.controller,1).await.is_ok());
    f.admission.transport_failed().await.unwrap();assert!(f.read(f.controller,1).await.is_err());
    f.admission.recover_synchronized(2,1).await.unwrap();assert!(f.read(f.controller,1).await.is_err(),"old generation is still invalid after a real recovery");
    let (fresh,mut receiver)=channel(f.controller,f.admission.task_generation().await.unwrap());receiver.bind_selection_reader(&f.admission,&f.document,f.stop.subscribe()).await.unwrap();
    assert!(fresh.request(Action::SceneSelectionDetails {controller:f.controller,revision:1}).await.is_ok());
    assert!(f.read(f.controller,1).await.is_err());receiver.close();f.receiver.close();f.finish().await;
}
#[tokio::test]
async fn selection_reader_is_gated_during_configuration_publication() {
    let f=Fixture::new().await;assert!(f.read(f.controller,1).await.is_ok());
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json")).unwrap();devices["revision"]=2.into();
    let mut rules:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad-away/automation.json")).unwrap();rules["configuration_revision"]=2.into();
    f.admission.begin_automation_publication(serde_json::to_vec(&devices).unwrap(),serde_json::to_vec(&rules).unwrap(),1,Duration::from_secs(1)).await.unwrap();
    assert!(f.read(f.controller,1).await.is_err());f.admission.abort_publication().await.unwrap();
    assert!(f.read(f.controller,1).await.is_err(),"publication's changed control epoch cannot revive an old capability");f.finish().await;
}
#[tokio::test]
async fn selection_reader_close_during_actor_wait_rejects_a_late_result() {
    let mut f=Fixture::new().await;f.hold_next.store(true,Ordering::Release);
    let control=f.control.clone();let controller=f.controller;
    let read=tokio::spawn(async move{control.request(Action::SceneSelectionDetails {controller,revision:1}).await});
    let entered=tokio::time::timeout(Duration::from_secs(2),f.entered.notified()).await;
    f.receiver.close();
    {let (lock,wake)=&*f.release;*lock.lock().unwrap()=true;wake.notify_all();}
    assert!(tokio::time::timeout(Duration::from_secs(2),read).await.unwrap().unwrap().is_err());assert!(entered.is_ok());f.finish().await;
}

#[tokio::test]
async fn selection_reader_rejects_binding_a_foreign_boot_even_with_equal_epochs() {
    let mut owner=Fixture::new().await;let other=Fixture::new().await;
    assert_ne!(owner.admission.boot_id(),other.admission.boot_id());
    assert_eq!(owner.receiver.generation.epochs,other.receiver.generation.epochs);
    assert!(owner.receiver.bind_selection_reader(&other.admission,&owner.document,owner.stop.subscribe()).await.is_err());
    assert!(owner.read(owner.controller,1).await.is_ok(),"rejected foreign binding cannot overwrite the original capability");
    owner.finish().await;other.finish().await;
}

#[tokio::test]
async fn selection_reader_stop_during_actor_wait_rejects_a_late_result() {
    let f=Fixture::new().await;f.hold_next.store(true,Ordering::Release);
    let control=f.control.clone();let controller=f.controller;
    let read=tokio::spawn(async move{control.request(Action::SceneSelectionDetails {controller,revision:1}).await});
    let entered=tokio::time::timeout(Duration::from_secs(2),f.entered.notified()).await;
    f.stop.send(true).unwrap();assert!(!f.control.sender.is_closed());
    {let (lock,wake)=&*f.release;*lock.lock().unwrap()=true;wake.notify_all();}
    assert!(tokio::time::timeout(Duration::from_secs(2),read).await.unwrap().unwrap().is_err());assert!(entered.is_ok());f.finish().await;
}
