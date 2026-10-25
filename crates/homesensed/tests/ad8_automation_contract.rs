//! AD8 automation normalizes every brightness target to a direct percentage.
//! Generated fixtures only: no field configuration or non-loopback connections.
use hap::server::WriteStatus;
use homesense_core::{Command,DeviceId,ExecutionContext,LevelTransition,Percent};
use homesense_hap::requests::{Batch,Intent};
use homesensed::admission::{Admission,Options};
use serde_json::{Value,json};
use std::{io::{Read,Write},sync::{Arc,Mutex},time::Duration};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
use uuid::Uuid;
fn devices(endpoint:&str,profile:&str,combined:bool,live:bool)->Vec<u8> {
    let verification=if live{json!({"status":"verified","verified_at_unix_ms":1,"evidence":"Generated synthetic loopback contract"})}else{json!({"status":"simulated"})};
    let mut light=json!({"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true});
    if combined{light["supply"]=json!({"module_id":"relay","channel":1});light["default_on_level"]=json!(100);}
    serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":endpoint,"modules":[{"id":"dimmer","address":"11","profile":profile,"verification":verification.clone()},{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":verification}]}],"devices":[light,{"id":"relay","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true}]})).unwrap()
}
fn batch(controller:Uuid,device:&str,level:u8,transition:LevelTransition)->Batch {
    Batch{controller,intents:vec![Intent{device:DeviceId::new(device).unwrap(),command:Command::SetLevel{level:Percent::new(level).unwrap(),transition}}],subscriptions:vec![]}
}
struct Wire(Arc<Mutex<Vec<[u8;5]>>>);
impl Read for Wire {fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for Wire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let(frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());self.0.lock().unwrap().extend_from_slice(frames);Ok(bytes.len())}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
#[tokio::test]
async fn ad8_automation_extremes_are_direct_and_intermediate_targets_are_direct() {
    for (level, transition, command) in [(0, LevelTransition::Ramp, 1), (100, LevelTransition::Ramp, 1), (10, LevelTransition::Immediate, 1), (10, LevelTransition::Ramp, 1), (1, LevelTransition::Ramp, 1), (99, LevelTransition::Ramp, 1)] {
        let directory = std::env::temp_dir().join(format!("ad8-endpoint-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(
            None, &devices("127.0.0.1:1", "ad8_din_rs485_v2_2_1", false, false),
            ExecutionContext::Simulation, &[],
        ).unwrap();
        let controller = Uuid::new_v4();
        let actor = Admission::open(path.clone(), Options {
            context: ExecutionContext::Simulation, ..Default::default()
        }, move |id| id == controller).await.unwrap();
        let generation = actor.task_generation().await.unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut owner = homesensed::transport_owner::TransportOwner::new(actor.clone(), [(
            "bus".into(), hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
        )].into());
        let ids = actor.submit_rule_update(
            generation, batch(controller, "light", level, transition), vec![],
        ).await.unwrap();
        let sent = owner.step().await.unwrap().unwrap();
        sent.result.unwrap();
        assert_eq!(sent.command_id, ids[0]);
        assert_eq!(*bytes.lock().unwrap(), vec![[0xae, 0x11, command, level, 0xee]],
            "AD8 {level}% must use the selected physical command, with every transition normalized to direct");
        drop(owner);
        actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
#[tokio::test]
async fn legacy_automation_ramp_is_normalized_to_direct_wire_while_manual_raw_ramp_stays_rejected() {
    for profile in ["ad8_din_rs485_v2_2_1","gt_d8_rl_v2_2_6_2"] {
        let directory=std::env::temp_dir().join(format!("automation-ramp-{}",Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(None,&devices("127.0.0.1:1",profile,false,false),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let actor=Admission::open(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();let generation=actor.task_generation().await.unwrap();
        let bytes=Arc::new(Mutex::new(Vec::new()));let mut owner=homesensed::transport_owner::TransportOwner::new(actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(Wire(bytes.clone()),1))].into());
        assert_eq!(actor.submit_for_revision(1,batch(controller,"light",37,LevelTransition::Ramp)).await,Err(WriteStatus::InvalidValueInRequest),"manual adapter cannot request a native transition");
        let ids=actor.submit_rule_update(generation.clone(),batch(controller,"light",37,LevelTransition::Ramp),vec![]).await.expect("legacy configured target remains admissible and normalizes before journaling");assert_eq!(ids.len(),1);
        let sent=owner.step().await.unwrap().unwrap();sent.result.unwrap();assert_eq!(sent.command_id,ids[0]);
        assert_eq!(*bytes.lock().unwrap(),vec![[0xae,0x11,1,37,0xee]],"legacy Ramp is normalized to a direct target, including on GT-D8");
        assert_eq!(actor.submit_rule_update(generation,batch(controller,"relay",37,LevelTransition::Ramp),vec![]).await,Err(WriteStatus::InvalidValueInRequest),"unsupported module kind must still reject");
        let db=rusqlite::Connection::open(&path).unwrap();assert_eq!(db.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);assert_eq!(db.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),1);drop(db);
        drop(owner);actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
    }
}
struct ReplyWire {bytes:Arc<Mutex<Vec<[u8;5]>>>,read:usize}
impl Read for ReplyWire {
    fn read(&mut self,out:&mut[u8])->std::io::Result<usize>{
        let bytes=self.bytes.lock().unwrap();let Some(frame)=bytes.get(self.read)else{return Err(std::io::ErrorKind::WouldBlock.into());};
        assert!(out.len()>=5);out[..5].copy_from_slice(&[0xae,frame[1],0xd0+frame[2],frame[3],0xee]);self.read+=1;Ok(5)
    }
}
impl Write for ReplyWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{Wire(self.bytes.clone()).write(bytes)}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
#[tokio::test]
async fn combined_logical_immediate_expands_to_direct_level_and_raw_logical_ramp_is_rejected() {
    let directory=std::env::temp_dir().join(format!("automation-combined-ramp-{}",Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
    homesense_storage::Store::open(&path).unwrap().publish_configuration(None,&devices("127.0.0.1:1","ad8_din_rs485_v2_2_1",true,false),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let actor=Admission::open(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();let generation=actor.task_generation().await.unwrap();
    let bytes=Arc::new(Mutex::new(Vec::new()));let mut owner=homesensed::transport_owner::TransportOwner::new(actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(ReplyWire{bytes:bytes.clone(),read:0},1))].into());
    assert_eq!(actor.submit_rule_update(generation.clone(),batch(controller,"light",37,LevelTransition::Ramp),vec![]).await,Err(WriteStatus::InvalidValueInRequest),"combined logical input must remain normalized Immediate");
    let ids=actor.submit_rule_update(generation,batch(controller,"light",37,LevelTransition::Immediate),vec![]).await.unwrap();assert_eq!(ids.len(),2);
    let sent=owner.step().await.unwrap().unwrap();sent.result.unwrap();assert_eq!(sent.completed.iter().map(|(id,_)|*id).collect::<Vec<_>>(),ids[..1]);assert_eq!(sent.physical_frames,1);
    assert!(owner.step().await.unwrap().is_none(),"supply module must wait for the dimmer's actual reply");
    let config=homesense_config::Configuration::parse(&devices("127.0.0.1:1","ad8_din_rs485_v2_2_1",true,false),ExecutionContext::Simulation).unwrap();
    let mut feedback=homesensed::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    feedback.apply(&owner.receive("bus").await.unwrap()).unwrap();
    feedback.reconcile_control_phases(&actor,controller).await.unwrap();
    let supply=owner.step().await.unwrap().expect("postwrite dimmer feedback releases supply");supply.result.unwrap();assert_eq!(supply.completed.iter().map(|(id,_)|*id).collect::<Vec<_>>(),ids[1..]);assert_eq!(supply.physical_frames,1);
    tokio::time::sleep(Duration::from_millis(3)).await;
    feedback.apply(&owner.receive("bus").await.unwrap()).unwrap();
    for _ in 0..2 {feedback.reconcile_control_phases(&actor,controller).await.unwrap();}
    let store=homesense_storage::Store::open(&path).unwrap();for id in &ids{assert_eq!(store.command_status(actor.boot_id(),*id).unwrap(),Some(homesense_storage::journal::Status::Written));}drop(store);assert!(owner.step().await.unwrap().is_none());
    assert_eq!(*bytes.lock().unwrap(),vec![[0xae,0x11,1,37,0xee],[0xae,3,1,0xf1,0xee]],"existing dimmer-before-supply expansion unchanged");
    drop(owner);actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn live_validated_ad8_startup_and_deferred_timeline_write_direct_targets_once() {
    let directory=std::env::temp_dir().join(format!("automation-live-ad8-ramp-{}",Uuid::new_v4()));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bytes=devices(&listener.local_addr().unwrap().to_string(),"ad8_din_rs485_v2_2_1",false,true);
    let rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"boot","enabled":true,"review_evidence":"Generated loopback","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"command","device":"light","command":{"kind":"level","value":25,"ramp":true}}},{"source":"fixture","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"Generated loopback","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"light","command":{"kind":"level","value":60,"ramp":true}}]}]});
    let config=homesense_config::Configuration::parse(&bytes,ExecutionContext::Live).unwrap();
    homesensed::configuration_publish::validate_automation(&config,&serde_json::to_vec(&rules).unwrap(),ExecutionContext::Live).expect("exact native Ramp document must pass the unchanged validation gate");
    let mut invalid:Value=rules.clone();invalid["rules"][0]["actions"][0]["action"]["command"]["ramp"]=json!(false);
    assert!(homesensed::configuration_publish::validate_automation(&config,&serde_json::to_vec(&invalid).unwrap(),ExecutionContext::Live).is_ok(),"direct AD8 intermediate targets validate");
    let frames=Arc::new(Mutex::new(Vec::new()));let received=frames.clone();
    let server=tokio::spawn(async move{
        let (mut socket,peer)=listener.accept().await.unwrap();assert!(peer.ip().is_loopback());let mut levels=[30u8;8];
        loop{
            let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
            if frame[1]==3 {assert!((0xa1..=0xa8).contains(&frame[2]),"unsupported switch operation {frame:?}");assert_eq!(frame[3],0xf2);socket.write_all(&[0xae,3,0xd0+(frame[2]-0xa0),0xf2,0xee]).await.unwrap();continue;}
            assert_eq!((frame[0],frame[1],frame[4]),(0xae,0x11,0xee));
            if frame[2]==0xa0 {assert_eq!(frame[3],0xf2);let replies=levels.iter().enumerate().flat_map(|(index,level)|[0xae,0x11,0xd1+index as u8,*level,0xee]).collect::<Vec<_>>();socket.write_all(&replies).await.unwrap();}
            else {assert_eq!(frame[2],1);assert!(matches!(frame[3],25|60));received.lock().unwrap().push(frame);levels[0]=frame[3];socket.write_all(&[0xae,0x11,0xd1,levels[0],0xee]).await.unwrap();}
        }
    });
    let result=homesensed::automation_tcp::serve(&bytes,&serde_json::to_vec(&rules).unwrap(),&directory,Duration::from_millis(1400),true).await;
    assert!(result.is_ok(),"validated AD8 startup/timed rules failed runtime admission: {result:?}");
    server.await.unwrap();assert_eq!(*frames.lock().unwrap(),vec![[0xae,0x11,1,25,0xee],[0xae,0x11,1,60,0xee]],"both configured targets must reach direct wire once in source order");
    let db=rusqlite::Connection::open(directory.join("automation-tcp.sqlite")).unwrap();assert_eq!(db.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),2);assert_eq!(db.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='boot'",[],|r|r.get::<_,i64>(0)).unwrap(),1);assert_eq!(db.query_row("SELECT count(*) FROM scene_run WHERE definition='later' AND status='commands_written'",[],|r|r.get::<_,i64>(0)).unwrap(),1);drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
