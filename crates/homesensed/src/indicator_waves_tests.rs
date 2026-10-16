use super::*;
use crate::{admission::Options,transport_owner::GatewayRead,automation_drive::background_led_phase::Phase};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use serde_json::json;
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering}},io::{Read,Write},path::PathBuf};
struct Fixture {actor:Admission,controller:Uuid,clock:&'static AtomicU64,auth:Arc<AtomicBool>,path:PathBuf,document:Document,feedback:Feedback,scenes:SceneScheduler}
impl Fixture {
    async fn new()->Self {
        let mut modules=(0..28).map(|n|json!({"id":format!("panel{n}"),"address":format!("{:02X}",0x60+n),"profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}})).collect::<Vec<_>>();
        modules.push(json!({"id":"relay","address":"20","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
        let mut devices=(0..168).map(|n|json!({"id":format!("led{n}"),"name":"Synthetic indicator","module_id":format!("panel{}",n/6),"channel":n%6+1,"enabled":true})).collect::<Vec<_>>();
        devices.push(json!({"id":"light","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true}));
        let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
        let rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"indicator_links":(0..3).map(|n|json!({"id":format!("link{n}"),"enabled":true,"review_evidence":"synthetic wave capacity","lights":["light"],"leds":(n*64..168.min((n+1)*64)).map(|i|format!("led{i}")).collect::<Vec<_>>()})).collect::<Vec<_>>()})).unwrap();
        let parsed=homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();let document=Document::parse(&rules,&parsed).unwrap();
        let path=std::env::temp_dir().join(format!("led168-{}.sqlite",Uuid::new_v4()));Store::open(&path).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
        let clock=&*Box::leak(Box::new(AtomicU64::new(500)));let auth=Arc::new(AtomicBool::new(true));let flag=auth.clone();
        let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(20),..Default::default()},move |_|flag.load(Ordering::Acquire),clock).await.unwrap();actor.synchronized(1,1).await.unwrap();
        let controller=Uuid::new_v4();let mut feedback=Feedback::new(&parsed,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:500,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd1,value:0xf2}]}}).unwrap();
        let scenes=SceneScheduler::new(actor.clone(),controller,8,500).unwrap();Self{actor,controller,clock,auth,path,document,feedback,scenes}
    }
    async fn advance(&self,waves:&mut Waves,query:bool)->Option<Wave>{waves.advance(&self.actor,self.controller,&self.document,&self.scenes,&self.feedback,20000,20,false,query).await.unwrap()}
    async fn write(&self,wave:Wave,frames:Arc<Mutex<Vec<Vec<u8>>>>)->bool {
        let phase=Phase::new(self.actor.task_generation().await.unwrap(),wave.batches,wave.deadline);
        let mut wire=hb_transport::Connection::for_simulation(Wire(frames),1);
        loop {
            if let Some(complete)=phase.completion(&self.actor,Some(self.controller)).await {return complete;}
            for _ in 0..4 {if self.clock.load(Ordering::SeqCst)>=phase.deadline(){break;}let Some(ticket)=self.actor.dispatch().await.unwrap() else{break;};crate::transport_owner::send_ticket(&ticket,&mut wire,||self.clock.load(Ordering::SeqCst)).unwrap();self.actor.finish_write(ticket.command.id,true).await.unwrap();self.clock.fetch_add(21,Ordering::SeqCst);}
        }
    }
    fn count(&self)->i64{rusqlite::Connection::open(&self.path).unwrap().query_row("SELECT count(*) FROM command_journal",[],|r|r.get(0)).unwrap()}
    async fn close(self){self.actor.shutdown().await.unwrap();for s in ["","-wal","-shm"]{let _=std::fs::remove_file(format!("{}{s}",self.path.display()));}let _=std::fs::remove_file(self.path.with_extension("runtime.lock"));}
}
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);
impl Read for Wire{fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for Wire{fn write(&mut self,b:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().push(b.to_vec());Ok(b.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
#[tokio::test]
async fn all168_actual_admission_finite_waves_keep_ttl_spacing_and_release_query_boundary(){
    let f=Fixture::new().await;let mut waves=Waves::default();let frames=Arc::new(Mutex::new(vec![]));let mut sizes=vec![];
    for _ in 0..80 {
        if let Some(wave)=f.advance(&mut waves,false).await {let n=wave.batches.iter().map(Vec::len).sum::<usize>();assert!((1..=16).contains(&n));sizes.push(n);assert!(f.write(wave,frames.clone()).await);waves.phase_ended(true);
            let before=f.count();assert!(f.advance(&mut waves,true).await.is_none());assert_eq!(before,f.count());assert!(f.advance(&mut waves,true).await.is_none());assert_eq!(before,f.count(),"ordinary query opportunity cannot admit next wave");
        }
        if waves.outcome.is_some(){break;}
    }
    let outcome=waves.outcome.as_ref().unwrap();assert_eq!(outcome.kind,Kind::Synced,"{outcome:?}");assert_eq!(outcome.admitted,168);assert_eq!(outcome.targets,168);assert_eq!(sizes.len(),11);
    let captured=frames.lock().unwrap().clone();assert_eq!(captured.len(),168);let mut unique=std::collections::BTreeSet::new();for b in captured.iter(){assert_eq!([b[0],b[3],b[4]],[0xae,0xf2,0xee]);assert!(unique.insert((b[1],b[2])));}
    let sql=rusqlite::Connection::open(&f.path).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written' AND updated_ms<deadline_ms",[],|r|r.get::<_,i64>(0)).unwrap(),168);drop(sql);
    for _ in 0..4 {f.clock.fetch_add(251,Ordering::SeqCst);assert!(f.advance(&mut waves,false).await.is_none());}assert_eq!(f.count(),168,"stable endpoints cannot replay due to regrouping");
    eprintln!("LED168 waves={sizes:?}, written=168, final_ms={}",f.clock.load(Ordering::SeqCst));f.close().await;
}
#[tokio::test]
async fn unadmitted_wave_metadata_drops_at_stop_revoke_and_original_feedback_expiry(){
    for boundary in ["stop","revoke","expiry"] {
        let f=Fixture::new().await;let mut waves=Waves::default();let frames=Arc::new(Mutex::new(vec![]));let first=f.advance(&mut waves,false).await.unwrap();assert!(f.write(first,frames).await);waves.phase_ended(true);f.advance(&mut waves,true).await;assert_eq!(f.count(),16);
        match boundary{"stop"=>f.actor.stop_now(),"revoke"=>f.auth.store(false,Ordering::Release),_=>f.clock.store(20500,Ordering::SeqCst)};
        assert!(f.advance(&mut waves,false).await.is_none());assert!(waves.cycle.is_none());assert_eq!(waves.outcome.as_ref().unwrap().kind,Kind::Aborted);assert_eq!(f.count(),16);f.close().await;
    }
}
#[tokio::test]
async fn cancelled_first_wave_is_unresolved_never_replayed_and_new_metadata_remains_bounded(){
    let f=Fixture::new().await;let mut waves=Waves::default();let frames=Arc::new(Mutex::new(vec![]));
    let first=f.advance(&mut waves,false).await.unwrap();let phase=Phase::new(f.actor.task_generation().await.unwrap(),first.batches,first.deadline);
    f.clock.store(first.deadline,Ordering::SeqCst);assert_eq!(phase.completion(&f.actor,Some(f.controller)).await,Some(false));waves.phase_ended(false);
    for _ in 0..80 {if let Some(wave)=f.advance(&mut waves,false).await {let complete=f.write(wave,frames.clone()).await;waves.phase_ended(complete);}
        if waves.outcome.is_some(){break;}}
    assert_eq!(waves.outcome.as_ref().unwrap().kind,Kind::Unresolved);assert_eq!(f.count(),168);assert_eq!(frames.lock().unwrap().len(),152);
    let sql=rusqlite::Connection::open(&f.path).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='cancelled' AND cancellation_reason='RunCancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),16);drop(sql);
    for _ in 0..4 {f.clock.fetch_add(251,Ordering::SeqCst);assert!(f.advance(&mut waves,false).await.is_none());}assert_eq!(f.count(),168);f.close().await;
}
#[tokio::test]
async fn feedback_change_prepares_remaining_targets_from_new_observation_and_cannot_claim_synced(){
    let mut f=Fixture::new().await;let mut waves=Waves::default();let first=f.advance(&mut waves,false).await.unwrap();assert!(f.write(first,Arc::new(Mutex::new(vec![]))).await);waves.phase_ended(true);f.advance(&mut waves,true).await;
    f.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:f.clock.load(Ordering::SeqCst),result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd1,value:0xf1}]}}).unwrap();
    for _ in 0..40 {assert!(f.advance(&mut waves,false).await.is_none());if waves.outcome.is_some(){break;}}
    assert_eq!(waves.outcome.as_ref().unwrap().kind,Kind::Changed);assert_eq!(f.count(),16,"remaining metadata must not carry pre-evaluated old OFF proof");f.close().await;
}
#[tokio::test]
async fn shared_snapshot_limit_still_rejects_duplicates_and_over_capacity(){
    let f=Fixture::new().await;let generation=f.actor.task_generation().await.unwrap();let id=DeviceId::new("light").unwrap();
    let duplicate=crate::management_devices::snapshot(&f.actor,f.controller,generation.clone(),vec![id.clone(),id.clone()],&f.feedback).await.unwrap_err();assert!(duplicate.contains("duplicate device"));
    let oversized=crate::management_devices::snapshot(&f.actor,f.controller,generation,vec![id;crate::web_auth::MAX_DEVICE_SCOPE+1],&f.feedback).await.unwrap_err();assert!(oversized.contains("query limit"));f.close().await;
}
