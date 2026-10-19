use super::*;
use crate::admission::Options;
use homesense_core::{Command,DeviceId,ExecutionContext};
use homesense_hap::requests::{Batch,Intent};
use homesense_storage::Store;
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering}},path::PathBuf,io::{Read,Write}};
use uuid::Uuid;
struct Fixture {
    admission:Admission,controller:Uuid,generation:TaskGeneration,path:PathBuf,
    clock:&'static AtomicU64,costly:Arc<AtomicBool>,authorized:Arc<AtomicBool>,calls:Arc<AtomicUsize>,
    batches:Vec<Vec<u64>>,
}
impl Fixture {
    async fn new(count:usize)->Self {
        let modules=(0..count.div_ceil(6)).map(|n|serde_json::json!({"id":format!("panel{n}"),"address":format!("{:02X}",0x60+n),"profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}})).collect::<Vec<_>>();
        let devices=(0..count).map(|n|serde_json::json!({"id":format!("led{n}"),"name":"Synthetic indicator","module_id":format!("panel{}",n/6),"channel":n%6+1,"enabled":true})).collect::<Vec<_>>();
        let config=serde_json::to_vec(&serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
        let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
        let path=std::env::temp_dir().join(format!("led-phase-{}.sqlite",Uuid::new_v4()));
        Store::open(&path).unwrap().publish_automation(None,&config,rules,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let clock=&*Box::leak(Box::new(AtomicU64::new(500)));
        let costly=Arc::new(AtomicBool::new(false));let authorized=Arc::new(AtomicBool::new(true));let calls=Arc::new(AtomicUsize::new(0));
        let flag=costly.clone();let auth=authorized.clone();let counter=calls.clone();
        let admission=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,command_capacity:count.max(128),..Default::default()},move|_|{
            if flag.load(Ordering::Acquire){clock.fetch_add(3,Ordering::SeqCst);counter.fetch_add(1,Ordering::SeqCst);}
            auth.load(Ordering::Acquire)
        },clock).await.unwrap();admission.synchronized(1,1).await.unwrap();let generation=admission.task_generation().await.unwrap();
        let fence=admission.manual_fence().await.unwrap();let mut batches=vec![];
        for n in 0..count {batches.push(admission.submit_scene(generation.clone(),Batch{controller,intents:vec![Intent{device:DeviceId::new(format!("led{n}")).unwrap(),command:Command::PanelLed(false)}],subscriptions:vec![]},fence).await.unwrap());}
        Self{admission,controller,generation,path,clock,costly,authorized,calls,batches}
    }
    fn phase(&self)->Phase {Phase::new(self.generation.clone(),self.batches.clone(),2000)}
    fn statuses(&self)->Vec<Status> {let store=Store::open(&self.path).unwrap();self.batches.iter().flatten().map(|id|store.command_status(self.admission.boot_id(),*id).unwrap().unwrap()).collect()}
    async fn close(self) {self.admission.shutdown().await.unwrap();for suffix in ["","-wal","-shm"]{let _=std::fs::remove_file(format!("{}{suffix}",self.path.display()));}let _=std::fs::remove_file(self.path.with_extension("runtime.lock"));}
}
#[derive(Clone)]
struct Frames(Arc<Mutex<Vec<Vec<u8>>>>);
impl Read for Frames {fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for Frames {fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().push(bytes.to_vec());Ok(bytes.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
#[tokio::test]
async fn away_preemption_retires_only_unwritten_background_colors_without_replay() {
    let f=Fixture::new(3).await;let phase=f.phase();
    let frames=Arc::new(Mutex::new(vec![]));let mut wire=hb_transport::Connection::for_simulation(Frames(frames.clone()),1);
    let ticket=f.admission.dispatch().await.unwrap().unwrap();
    crate::transport_owner::send_ticket(&ticket,&mut wire,||f.clock.load(Ordering::SeqCst)).unwrap();
    f.admission.finish_write(ticket.command.id,true).await.unwrap();
    phase.cancel(&f.admission,Some(f.controller)).await;
    assert_eq!(f.statuses(),vec![Status::Written,Status::Cancelled,Status::Cancelled]);
    assert_eq!(phase.completion(&f.admission,Some(f.controller)).await,Some(false));
    assert!(f.admission.dispatch().await.unwrap().is_none());
    assert_eq!(frames.lock().unwrap().len(),1,"preemption cannot replay cancelled colors");
    f.close().await;
}
#[tokio::test]
async fn fifty_one_initial_leds_keep_original_phase_cap_with_bounded_status_reads() {
    let f=Fixture::new(51).await;let phase=f.phase();let frames=Arc::new(Mutex::new(vec![]));let mut wire=hb_transport::Connection::for_simulation(Frames(frames.clone()),1);
    f.costly.store(true,Ordering::Release);let mut polls=vec![];
    for _ in 0..32 {
        let before=f.calls.load(Ordering::Acquire);let done=phase.finished(&f.admission,Some(f.controller)).await;polls.push(f.calls.load(Ordering::Acquire)-before);
        if done {break;}
        // Same four-dispatch quantum and >20ms frame spacing; real Admission
        // chooses tickets and audits Writing/Written under the unchanged2s TTL.
        for _ in 0..4 {
            let Some(ticket)=f.admission.dispatch().await.unwrap() else{break;};
            crate::transport_owner::send_ticket(&ticket,&mut wire,||f.clock.load(Ordering::SeqCst)).unwrap();
            f.admission.finish_write(ticket.command.id,true).await.unwrap();f.clock.fetch_add(21,Ordering::SeqCst);
        }
    }
    let statuses=f.statuses();let captured=frames.lock().unwrap().clone();
    assert!(statuses.iter().all(|s|*s==Status::Written),"phase cancelled known-unwritten LED work before its originaldeadline: statuses={statuses:?} clock={} readRPCcosts={polls:?} frames={}",f.clock.load(Ordering::SeqCst),captured.len());
    assert!(f.clock.load(Ordering::SeqCst)<2000,"original phasecap must remain unchanged");assert_eq!(polls[0],1);
    eprintln!("syntheticLEDphase written={} final_ms={} phasecap=2000 original_command_deadline=2500 statusRPCs={polls:?}",captured.len(),f.clock.load(Ordering::SeqCst));
    assert_eq!(captured.len(),51);let mut physical=std::collections::BTreeSet::new();for frame in captured {assert_eq!([frame[0],frame[3],frame[4]],[0xae,0xf2,0xee]);assert!(physical.insert((frame[1],frame[2])));}
    let sql=rusqlite::Connection::open(&f.path).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE deadline_ms=2500 AND status='written' AND updated_ms<deadline_ms",[],|r|r.get::<_,i64>(0)).unwrap(),51);drop(sql);f.close().await;
}
#[tokio::test]
async fn phase_cap_true_expiry_and_revoked_controller_never_replay_leds() {
    for boundary in ["phase","expiry","revoke","stop"] {
        let f=Fixture::new(3).await;let phase=f.phase();
        match boundary {
            "phase"=>{f.clock.store(2000,Ordering::SeqCst);assert!(phase.finished(&f.admission,Some(f.controller)).await);},
            "expiry"=>{f.clock.store(2501,Ordering::SeqCst);},
            "revoke"=>{f.authorized.store(false,Ordering::Release);assert!(!phase.finished(&f.admission,Some(f.controller)).await);},
            _=>f.admission.stop_now(),
        }
        assert!(!matches!(f.admission.dispatch().await,Ok(Some(_))),"{boundary} cannot deliver any frame ticket");
        assert!(f.statuses().iter().all(|s|*s!=Status::Written));
        if matches!(boundary,"phase"|"expiry") {
            let sql=rusqlite::Connection::open(&f.path).unwrap();let expected=if boundary=="phase"{"RunCancelled"}else{"Expired"};
            assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE cancellation_reason=? AND status='cancelled'",[expected],|r|r.get::<_,i64>(0)).unwrap(),3);
        }
        if boundary=="revoke" {f.authorized.store(true,Ordering::Release);assert!(f.admission.dispatch().await.unwrap().is_none(),"restoring authorization must not replay cancelled old work");}
        f.close().await;
    }
}

#[tokio::test]
async fn large_phase_status_reads_keep_the_existing128_id_bound() {
    let f=Fixture::new(129).await;let phase=f.phase();f.costly.store(true,Ordering::Release);
    let before=f.calls.load(Ordering::Acquire);
    assert!(!phase.finished(&f.admission,Some(f.controller)).await);
    assert_eq!(f.calls.load(Ordering::Acquire)-before,2,"129accepted IDs must use two bounded reads");
    assert!(f.statuses().iter().all(|s|*s==Status::Accepted));f.close().await;
}

struct PartialWire {written:usize}
impl Read for PartialWire {fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for PartialWire {
    fn write(&mut self,_:&[u8])->std::io::Result<usize>{if self.written==0 {self.written=2;Ok(2)}else{Err(std::io::ErrorKind::BrokenPipe.into())}}
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
#[tokio::test]
async fn uncertain_led_write_ends_phase_without_being_replayed_or_claimed_complete() {
    let f=Fixture::new(3).await;let phase=f.phase();let ticket=f.admission.dispatch().await.unwrap().unwrap();
    let mut wire=hb_transport::Connection::for_simulation(PartialWire {written:0},1);
    let error=crate::transport_owner::send_ticket(&ticket,&mut wire,||f.clock.load(Ordering::SeqCst)).unwrap_err();assert_eq!(error.bytes_written,2);
    f.admission.finish_write(ticket.command.id,false).await.unwrap();
    assert_eq!(f.statuses()[0],Status::Uncertain);assert!(phase.finished(&f.admission,Some(f.controller)).await);
    assert!(!matches!(f.admission.dispatch().await,Ok(Some(_))));
    let sql=rusqlite::Connection::open(&f.path).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),3);assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),0);drop(sql);f.close().await;
}
