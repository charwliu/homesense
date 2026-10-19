//! Real synthetic bus, explicit stop, and logical multi-day lifetime advancement.
use super::*;
use crate::lifecycle::{RunControl,RunMode};
use super::fault_tests::free_address;
use serde_json::{Value,json};
use std::sync::{Arc,Mutex,atomic::{AtomicU8,Ordering}};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
async fn http(address:std::net::SocketAddr,path:&str,body:Option<Value>)->(u16,Value){
    super::fault_tests::http(address,path,body).await.expect("synthetic management response")
}
struct Bus {
    devices:Value,
    frames:Arc<Mutex<Vec<[u8;5]>>>,
    phase:Arc<AtomicU8>,
    stop:tokio::sync::watch::Sender<bool>,
    task:tokio::task::JoinHandle<()>,
}
impl Bus {
    async fn new(motor:bool)->Self {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut devices:Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        devices["gateways"][0]["endpoint"]=listener.local_addr().unwrap().to_string().into();
        let verified=json!({"status":"verified","evidence":"synthetic lifecycle fixture","verified_at_unix_ms":1});devices["gateways"][0]["modules"][0]["verification"]=verified.clone();
        if motor {
            devices["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"address":"22","profile":"c4_din_pro_v2_7_7","verification":verified}));
            devices["devices"].as_array_mut().unwrap().push(json!({"id":"motor","name":"Synthetic motor","gateway":"simulation","address":"22","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}));
        }
        let frames=Arc::new(Mutex::new(vec![]));let phase=Arc::new(AtomicU8::new(0));let level=Arc::new(AtomicU8::new(0));
        let captured=frames.clone();let state=phase.clone();let (stop,mut stopping)=tokio::sync::watch::channel(false);
        let task=tokio::spawn(async move {let mut workers=tokio::task::JoinSet::new();loop {tokio::select! {
            _=stopping.changed()=>break,
            result=workers.join_next(),if !workers.is_empty()=>{result.unwrap().unwrap();},
            accepted=listener.accept()=>{let (mut socket,_)=accepted.unwrap();let captured=captured.clone();let state=state.clone();let level=level.clone();workers.spawn(async move {loop {
                let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
                assert_eq!(frame[0],0xae);assert_eq!(frame[4],0xee);captured.lock().unwrap().push(frame);
                if frame[1]==0x22 {assert!(motor);assert_eq!(frame[2],1);assert!([0xf0,0xf1,0xf2].contains(&frame[3]));continue;}
                assert_eq!(frame[1],0x15);
                let response=if frame[2]==0xa0 {
                    assert_eq!(frame[3],0xf2);let phase=state.load(Ordering::Acquire);if phase==1{break;}
                    if phase==4{continue;}
                    (1..=8).filter(|channel|phase!=2 || *channel!=8).flat_map(|channel|[0xae,0x15,0xd0+channel,level.load(Ordering::Acquire),0xee]).collect::<Vec<_>>()
                }else{assert_eq!(frame[2],1);assert!(frame[3]<=100);level.store(frame[3],Ordering::Release);vec![0xae,0x15,0xd1,frame[3],0xee]};
                if socket.write_all(&response).await.is_err(){break;}
            }});},
        }}workers.abort_all();while let Some(result)=workers.join_next().await{if let Err(error)=result{assert!(error.is_cancelled(),"{error}");}}});
        Self{devices,frames,phase,stop,task}
    }
    fn levels(&self)->Vec<u8>{self.frames.lock().unwrap().iter().filter(|frame|frame[1]==0x15 && frame[2]==1).map(|frame|frame[3]).collect()}
    async fn shutdown(self){self.stop.send_replace(true);self.task.await.unwrap();}
}
// Fault injection needs an actual scheduled read. A test-only read consumer
// supplies it; idle sweeps retain the production 300s floor. No extra control
// is executed: the observed scene below is never invoked.
fn rules(revision:u64,boot:bool)->Vec<u8>{serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":revision,"variables":[],"rules":if boot{json!([{"id":"boot","enabled":true,"review_evidence":"synthetic","source":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"synthetic","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":25,"ramp":false}}}]}])}else{json!([])},"timed_scenes":[{"id":"fault-observation","enabled":true,"review_evidence":"Synthetic read consumer only; never invoked","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":25,"ramp":false}}]}],"scene_tracking":[{"scene":"fault-observation","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic fault observation","max_age_ms":1000,"review_evidence":"Explicit bounded read consumer triggers fault detection independently of the 300s idle sweep","members":[{"id":"light","device":"simulation:light","predicate":{"kind":"power","on":true},"review_evidence":"Synthetic single-port observation"}]}]})).unwrap()}
async fn ready(web:std::net::SocketAddr,revision:&str,service:&tokio::task::JoinHandle<Result<(),String>>){tokio::time::timeout(Duration::from_secs(12),async{loop{
    assert!(!service.is_finished());if tokio::net::TcpStream::connect(web).await.is_ok(){let (code,state)=http(web,"/api/v1/diagnostics",None).await;if code==200 && state["revision"]==revision && state["ready"]==true{break;}}
    tokio::time::sleep(Duration::from_millis(10)).await;
}}).await.unwrap();}
async fn unavailable(web:std::net::SocketAddr,service:&tokio::task::JoinHandle<Result<(),String>>){tokio::time::timeout(Duration::from_secs(6),async{loop{
    assert!(!service.is_finished());let (code,state)=http(web,"/api/v1/diagnostics",None).await;if code==200 && state["ready"]==false{break;}
    tokio::time::sleep(Duration::from_millis(10)).await;
}}).await.unwrap();}
#[tokio::test]
async fn forever_survives_multi_day_advance_fault_partial_recovery_and_online_publication_without_replay(){
    let root=std::env::temp_dir().join(format!("forever-runtime-{}",Uuid::new_v4()));let bus=Bus::new(false).await;
    let web=free_address();let (stop,receive)=tokio::sync::watch::channel(false);let (publisher,requests)=crate::automation_publication::channel();
    let service={let input=serde_json::to_vec(&bus.devices).unwrap();let directory=root.clone();let publisher=publisher.clone();tokio::spawn(async move {serve_inner_running(&input,&rules(1,true),&directory,RunControl::new(RunMode::Forever,receive),true,3,Some(requests),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),None,None,None).await})};
    ready(web,"1",&service).await;
    tokio::time::timeout(Duration::from_secs(3),async{while bus.levels()!=vec![25]{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    bus.phase.store(1,Ordering::Release);unavailable(web,&service).await;
    let (_,receipt)=http(web,"/api/v1/device-commands",Some(json!({"device":"simulation:light","revision":"1","request_id":"offline","command":{"action":"brightness","level":88}}))).await;
    assert_eq!(receipt["outcome"],"unknown");assert_eq!(receipt["retry"],false);
    bus.phase.store(2,Ordering::Release);
    let before=bus.frames.lock().unwrap().len();tokio::time::timeout(Duration::from_secs(6),async{while bus.frames.lock().unwrap().len()==before{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    tokio::time::pause();tokio::time::advance(Duration::from_secs(86400*30)).await;tokio::time::resume();
    assert!(!service.is_finished(),"forever must not synthesize a finite deadline");unavailable(web,&service).await;assert_eq!(bus.levels(),vec![25]);
    bus.phase.store(3,Ordering::Release);ready(web,"1",&service).await;
    let (_,receipt)=http(web,"/api/v1/device-commands",Some(json!({"device":"simulation:light","revision":"1","request_id":"fresh","command":{"action":"brightness","level":30}}))).await;assert_eq!(receipt["outcome"],"accepted","{receipt}");
    tokio::time::timeout(Duration::from_secs(3),async{while bus.levels()!=vec![25,30]{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    let mut candidate=bus.devices.clone();candidate["revision"]=2.into();assert_eq!(publisher.publish(serde_json::to_vec(&candidate).unwrap(),rules(2,false),1).await.unwrap(),2);ready(web,"2",&service).await;
    stop.send_replace(true);tokio::time::timeout(Duration::from_secs(3),service).await.unwrap().unwrap().unwrap();assert!(tokio::net::TcpStream::connect(web).await.is_err());assert_eq!(bus.levels(),vec![25,30]);
    let db=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();let count:i64=db.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|row|row.get(0)).unwrap();assert_eq!(count,2);let pending:i64=db.query_row("SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",[],|row|row.get(0)).unwrap();assert_eq!(pending,0);drop(db);bus.shutdown().await;std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn stop_during_initial_synchronization_finishes_without_startup_or_control(){
    let root=std::env::temp_dir().join(format!("forever-initial-stop-{}",Uuid::new_v4()));let bus=Bus::new(false).await;bus.phase.store(4,Ordering::Release);let (stop,receive)=tokio::sync::watch::channel(false);
    let service={let input=serde_json::to_vec(&bus.devices).unwrap();let directory=root.clone();tokio::spawn(async move {serve_running(&input,&rules(1,true),&directory,RunControl::new(RunMode::Forever,receive),true,0,Duration::from_millis(20),Endpoints::default()).await})};
    tokio::time::timeout(Duration::from_secs(3),async{while bus.frames.lock().unwrap().is_empty(){tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(3),service).await.unwrap().unwrap().unwrap();assert!(bus.levels().is_empty());bus.shutdown().await;std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn explicit_stop_after_accepted_publication_drains_motor_stop_and_commits_before_exit(){
    let root=std::env::temp_dir().join(format!("forever-publication-stop-{}",Uuid::new_v4()));let bus=Bus::new(true).await;let web=free_address();let (stop,receive)=tokio::sync::watch::channel(false);let (publisher,requests)=crate::automation_publication::channel();let mut control=RunControl::new(RunMode::Forever,receive);control.stop_after_barrier=Some(stop.clone());control.queue_stop_before_barrier=Some(homesense_core::DeviceId::new("motor").unwrap());
    let service={let input=serde_json::to_vec(&bus.devices).unwrap();let directory=root.clone();let publisher=publisher.clone();tokio::spawn(async move {serve_inner_running(&input,&rules(1,false),&directory,control,true,0,Some(requests),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),None,None,None).await})};
    ready(web,"1",&service).await;let (_,receipt)=http(web,"/api/v1/device-commands",Some(json!({"device":"motor","revision":"1","request_id":"open","command":{"action":"open"}}))).await;assert_eq!(receipt["outcome"],"accepted","{receipt}");
    tokio::time::timeout(Duration::from_secs(3),async{while !bus.frames.lock().unwrap().contains(&[0xae,0x22,1,0xf1,0xee]){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    let mut candidate=bus.devices.clone();candidate["revision"]=2.into();let error=publisher.publish(serde_json::to_vec(&candidate).unwrap(),rules(2,false),1).await.unwrap_err();assert!(error.contains("committed revision 2") && error.contains("shutdown"),"{error}");
    tokio::time::timeout(Duration::from_secs(3),service).await.unwrap().unwrap().unwrap();assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    let motor:Vec<_>=bus.frames.lock().unwrap().iter().filter(|frame|frame[1]==0x22).copied().collect();assert_eq!(motor,vec![[0xae,0x22,1,0xf1,0xee],[0xae,0x22,1,0xf0,0xee]],"accepted STOP must finish before shutdown, with no replay");
    bus.shutdown().await;std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn forever_retries_initial_unavailability_without_startup_until_complete_feedback(){
    let root=std::env::temp_dir().join(format!("forever-initial-retry-{}",Uuid::new_v4()));let bus=Bus::new(false).await;bus.phase.store(1,Ordering::Release);let web=free_address();let (stop,receive)=tokio::sync::watch::channel(false);
    let service={let input=serde_json::to_vec(&bus.devices).unwrap();let directory=root.clone();tokio::spawn(async move {serve_running(&input,&rules(1,true),&directory,RunControl::new(RunMode::Forever,receive),true,3,Duration::from_millis(20),Endpoints{management:Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),..Default::default()}).await})};
    tokio::time::timeout(Duration::from_secs(5),async{while bus.frames.lock().unwrap().len()<3{assert!(!service.is_finished());tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    assert!(bus.levels().is_empty());tokio::time::pause();tokio::time::advance(Duration::from_secs(86400*2)).await;tokio::time::resume();assert!(!service.is_finished());assert!(bus.levels().is_empty());
    bus.phase.store(0,Ordering::Release);ready(web,"1",&service).await;
    tokio::time::timeout(Duration::from_secs(3),async{while bus.levels()!=vec![25]{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    stop.send_replace(true);tokio::time::timeout(Duration::from_secs(3),service).await.unwrap().unwrap().unwrap();assert_eq!(bus.levels(),vec![25]);bus.shutdown().await;std::fs::remove_dir_all(root).unwrap();
}
