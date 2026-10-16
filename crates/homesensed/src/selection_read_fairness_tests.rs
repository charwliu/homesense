//! Same production drive, held physical worker, unchanged Web 500ms read bound.
use super::*;
use crate::{admission::{Admission,Options},automation_runtime::{AutomationRuntime,StartupPolicy},feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{io::{Read,Write},sync::{Arc,Mutex,Condvar,atomic::{AtomicBool,Ordering}},time::Duration};

struct HeldRead {
    entered:Arc<tokio::sync::Notify>,
    release:Arc<(Mutex<bool>,Condvar)>,
    blocked:Arc<AtomicBool>,
    writes:Arc<Mutex<Vec<u8>>>,
}
impl Read for HeldRead {
    fn read(&mut self,_:&mut [u8])->std::io::Result<usize> {
        let (lock,wake)=&*self.release;
        let mut released=lock.lock().unwrap();
        if !*released {
            self.blocked.store(true,Ordering::Release);
            self.entered.notify_one();
            while !*released {released=wake.wait(released).unwrap();}
            self.blocked.store(false,Ordering::Release);
        }
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl Write for HeldRead {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.writes.lock().unwrap().extend_from_slice(bytes);Ok(bytes.len())}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
async fn selection_during_held_production_work(nonempty:bool) {
    let directory=std::env::temp_dir().join(format!("selection-fairness-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
    let (devices,rules):(&[u8],&[u8])=if nonempty {
        (include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json"),include_bytes!("../../../config/test/native-app-keypad-away/automation.json"))
    }else{(include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#)};
    Store::open(&path).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=uuid::Uuid::new_v4();let admission=Admission::open(path.clone(),Options {context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();
    admission.synchronized(1,1).await.unwrap();
    let configuration=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,1_800_000_000_000,1000,16,Default::default(),StartupPolicy::Suppress).await.unwrap();
    let control=runtime.control();
    let entered=Arc::new(tokio::sync::Notify::new());let release=Arc::new((Mutex::new(false),Condvar::new()));let blocked=Arc::new(AtomicBool::new(false));let writes=Arc::new(Mutex::new(vec![]));
    let feedback=Feedback::new(&configuration,ExecutionContext::Simulation,uuid::Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap();
    let gateway=configuration.gateways().keys().next().unwrap().clone();
    let mut owner=ObservedTransport::new(TransportOwner::new(admission.clone(),[(gateway,hb_transport::Connection::for_simulation(HeldRead {entered:entered.clone(),release:release.clone(),blocked:blocked.clone(),writes:writes.clone()},1))].into()),feedback);
    let (stop,receive)=tokio::sync::watch::channel(false);
    let drive=tokio::spawn(async move{runtime.drive(&mut owner,Duration::from_millis(2),5000,receive,||Ok(1_800_000_000_000)).await});
    tokio::time::timeout(Duration::from_secs(2),entered.notified()).await.unwrap();
    assert!(blocked.load(Ordering::Acquire));assert!(writes.lock().unwrap().is_empty());
    // Same API helper and original 500ms timeout as real HTTP/WSS. The drive
    // cannot poll its control queue until this held worker has been released.
    let result=selection_details(&control,controller,1).await;
    let was_still_blocked=blocked.load(Ordering::Acquire);
    stop.send(true).unwrap();
    {let (lock,wake)=&*release;*lock.lock().unwrap()=true;wake.notify_all();}
    assert!(tokio::time::timeout(Duration::from_secs(2),drive).await.unwrap().unwrap().is_ok());
    admission.shutdown().await.unwrap();
    let sql=rusqlite::Connection::open(&path).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|row|row.get::<_,i64>(0)).unwrap(),0);drop(sql);
    assert!(writes.lock().unwrap().is_empty(),"read-only selection must never submit controls");
    std::fs::remove_dir_all(directory).unwrap();
    assert!(was_still_blocked,"read completed before allowing production work to resume");
    let states=result.expect("selection read must not expire behind unrelated production work");
    assert_eq!(states.is_empty(),!nonempty);
}
#[tokio::test]
async fn empty_selection_read_is_not_starved_by_held_production_work(){selection_during_held_production_work(false).await;}
#[tokio::test]
async fn nonempty_selection_read_is_not_starved_by_held_production_work(){selection_during_held_production_work(true).await;}
