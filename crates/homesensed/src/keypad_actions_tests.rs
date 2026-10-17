use super::*;
use crate::{admission::Options,feedback::Feedback,web_auth::{Grant,Role,Tokens}};
use homesense_core::{Command,ExecutionContext,LevelTransition,Percent};
use homesense_hap::requests::{Batch,Intent};
use homesense_storage::{Store,variables::Update};
use std::{path::PathBuf,sync::{Arc,atomic::{AtomicU64,Ordering}}};

const SECRET:&str="synthetic-unified-keypad-token-0123456789";
struct Fixture { path:PathBuf,actor:Admission,config:Configuration,doc:Document,controller:Uuid,tokens:Arc<Tokens>,credential:Uuid,time:&'static AtomicU64,feedback:Feedback }
impl Fixture {
    async fn new(cue:bool,scope:impl FnOnce(&mut Grant))->Self {
        Self::with_motor(cue,false,scope).await
    }
    async fn with_motor(cue:bool,motor:bool,scope:impl FnOnce(&mut Grant))->Self {
        let mut config:Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/devices.json")).unwrap();
        config["keypads"][0]["keys"][2]["action"]=json!({"kind":"scene_toggle","scene":"meeting","review_evidence":"synthetic second origin key"});
        if cue {config["keypads"][0]["keys"][0]["local_indicator"]=json!({"kind":"scene_selection","group":"reading-group","value":1,"selected_color":"red","unselected_color":"green","review_evidence":"synthetic exact On/Off source review"});}
        if motor {
            config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"motor-module","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            config["devices"].as_array_mut().unwrap().push(json!({"id":"curtain","name":"Synthetic motor","module_id":"motor-module","channel":1,"enabled":true,
                "motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic stop scope","verified_at_unix_ms":1},
                "curtain_state":{"open_travel_ms":10000,"close_travel_ms":10000,"stop_led_idle_ms":1,"evidence":"synthetic direction and LED policy","reviewed_at_unix_ms":1,
                    "leds":{"open":["legacy-key-2"],"close":["legacy-key-3"],"stop":["legacy-key-4"]}}}));
            for (index,action) in [(1,"open"),(2,"close"),(3,"stop")] {
                config["keypads"][0]["keys"][index]["action"]=json!({"kind":"motor","device":"curtain","action":action,
                    "press_behavior":if action=="stop"{"absolute"}else{"repeat_to_stop"},"review_evidence":"synthetic native key mapping"});
            }
            // Twelve independent, explicitly reviewed eight-output modules.
            for n in 0..12 {
                config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":format!("away-{n}"),"address":format!("{:02X}",0x50+n),"profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
                for channel in 1..=8 {config["devices"].as_array_mut().unwrap().push(json!({"id":format!("away-{n}:{channel}"),"name":"Synthetic Away output","module_id":format!("away-{n}"),"channel":channel,"enabled":true}));}
            }
            config["keypads"][0]["keys"][5]["action"]=json!({"kind":"away_off","indicator_reset":"green","review_evidence":"synthetic exact origin reset"});
        }
        let bytes=serde_json::to_vec(&config).unwrap();
        let mut rules:Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/automation.json")).unwrap();
        // This fixture starts from a known recorded selection. Physical-pattern
        // inference and native-unknown execution have separate regressions.
        rules["scene_switch_groups"][0]["reconciliation"]["enabled"]=json!(false);
        if motor {rules["away_off"]=json!({"enabled":true,"review_evidence":"synthetic twelve complete lighting modules","active_execution_policy":"cancel_affected_lighting",
            "modules":(0..12).map(|n|json!({"module_id":format!("away-{n}"),"devices":(1..=8).map(|channel|format!("away-{n}:{channel}")).collect::<Vec<_>>(),"review_evidence":"synthetic full eight outputs"})).collect::<Vec<_>>()});}
        let rules=serde_json::to_vec(&rules).unwrap();
        let path=std::env::temp_dir().join(format!("keypad-unified-{}.sqlite",Uuid::new_v4()));
        let mut store=Store::open(&path).unwrap();let bundle=store.publish_automation(None,&bytes,&rules,ExecutionContext::Simulation,&[]).unwrap();
        store.accept_queue_batch_with_variables("seed",&[],&[],0,None,&[Update{revision:1,name:"selection".into(),expected_version:0,value:Some(homesense_core::rule::Value::Integer(0))}]).unwrap();drop(store);
        let config=bundle.configuration;let doc=bundle.automation.unwrap();let controller=Uuid::new_v4();let credential=Uuid::new_v4();let tokens=Arc::new(Tokens::default());
        let mut grant=Grant{controller,role:Role::Member,devices:config.devices().keys().map(|id|id.as_str().into()).collect(),scenes:doc.timed_scenes.keys().cloned().collect()};scope(&mut grant);tokens.insert(credential,SECRET,grant).unwrap();
        let time=Box::leak(Box::new(AtomicU64::new(100)));
        let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(20),..Default::default()},move |_|true,time).await.unwrap();actor.synchronized(1,1).await.unwrap();
        let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
        Self{path,actor,config,doc,controller,tokens,credential,time,feedback}
    }
    fn request(&self)->Request {Request{revision:"1".into(),keypad:"panel".into(),key:1,request_id:Uuid::new_v4().to_string(),gesture:Gesture::Activate}}
    fn source(&self,input:&Request)->Source {Source::capture(self.tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&Action::KeypadAction{controller:self.controller,request:input.clone()},None).unwrap()}
    async fn prepare(&self,input:&Request)->Result<Prepared,Rejection> {prepare(&self.actor,&self.actor.task_generation().await.unwrap(),&self.doc,self.controller,input,&self.source(input)).await}
    fn batch(&self,on:bool,own:bool,linked:bool)->Batch {
        let mut intents=vec![Intent{device:DeviceId::new("fixture-light").unwrap(),command:Command::SetLevel{level:Percent::new(if on{40}else{0}).unwrap(),transition:LevelTransition::Immediate}}];
        if own {intents.push(Intent{device:DeviceId::new("legacy-key-1").unwrap(),command:Command::PanelLed(on)});}
        if linked {intents.push(Intent{device:DeviceId::new("legacy-key-2").unwrap(),command:Command::PanelLed(on)});}
        Batch{controller:self.controller,intents,subscriptions:vec![]}
    }
    async fn submit(&self,source:Option<Source>,batch:Batch)->Result<Vec<u64>,String> {
        let actor=self.actor.with_source(source);
        let nodes=batch.intents.into_iter().map(|intent|homesense_core::rule::Node{source:"synthetic reviewed scene phase".into(),
            action:homesense_core::rule::Action::Command{device:intent.device,command:intent.command}}).collect::<Vec<_>>();
        crate::rule_runtime::PreparedRule::prepare(&actor,self.controller,vec![],&nodes,homesense_core::rule::Budget::default()).await?.commit(&actor).await
    }
    async fn dispatch(&self)->crate::admission::DispatchTicket {self.time.fetch_add(21,Ordering::SeqCst);self.actor.dispatch().await.unwrap().unwrap()}
    async fn finish(&self,ticket:&crate::admission::DispatchTicket){self.actor.finish_write(ticket.command.id,true).await.unwrap();}
    async fn close(self){self.actor.shutdown().await.unwrap();for suffix in ["","-wal","-shm"]{let _=std::fs::remove_file(format!("{}{suffix}",self.path.display()));}let _=std::fs::remove_file(self.path.with_extension("runtime.lock"));}
}

#[derive(Clone)]
struct Wire { bytes:WireBytes,fail_after:Option<usize> }
type WireBytes=Arc<std::sync::Mutex<Vec<Vec<u8>>>>;
impl std::io::Read for Wire {fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl std::io::Write for Wire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let mut log=self.bytes.lock().unwrap();if self.fail_after==Some(log.len()){return Err(std::io::ErrorKind::PermissionDenied.into());}log.push(bytes.to_vec());Ok(bytes.len())}
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
fn owner(f:&Fixture,fail_after:Option<usize>)->(crate::transport_owner::TransportOwner<Wire>,WireBytes) {
    let bytes=Arc::new(std::sync::Mutex::new(vec![]));let wire=Wire{bytes:bytes.clone(),fail_after};
    (crate::transport_owner::TransportOwner::new(f.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(wire,1))].into()),bytes)
}
async fn press_motor(f:&Fixture,key:u8)->Vec<u64> {
    let input=Request{key,..f.request()};let prepared=f.prepare(&input).await.unwrap();let actor=f.actor.with_source(prepared.source);
    match prepared.action {
        Action::DeviceCommand{device,action:crate::management_devices::DeviceAction::KeypadPress{keypad,key},..}=>actor.submit_keypad_press_for_revision(1,f.controller,device,keypad,key,f.time.load(Ordering::SeqCst)+2000).await.unwrap(),
        Action::DeviceCommand{device,action:crate::management_devices::DeviceAction::Stop,..}=>actor.submit_motor_for_revision(1,f.controller,device,homesense_core::MotorAction::Stop).await.unwrap(),
        _=>panic!("unexpected synthetic motor mapping"),
    }
}

#[tokio::test]
async fn real_motor_owner_final_repeat_stop_precedes_green_source_cue() {
    let f=Fixture::with_motor(false,true,|_|{}).await;let (mut owner,bytes)=owner(&f,None);
    let first=press_motor(&f,2).await;assert_eq!(first.len(),2);
    for _ in 0..2 {f.time.fetch_add(21,Ordering::SeqCst);owner.step().await.unwrap().unwrap().result.unwrap();}
    let repeat=press_motor(&f,2).await;assert_eq!(repeat.len(),2);
    f.time.fetch_add(21,Ordering::SeqCst);let stopped=owner.step().await.unwrap().unwrap();assert_eq!(stopped.command_id,repeat[0]);stopped.result.unwrap();
    f.time.fetch_add(21,Ordering::SeqCst);owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(bytes.lock().unwrap().as_slice(),&[vec![0xae,0x22,1,0xf1,0xee],vec![0xae,0x41,2,0xf1,0xee],vec![0xae,0x22,1,0xf0,0xee],vec![0xae,0x41,2,0xf2,0xee]]);
    f.close().await;
}

#[tokio::test]
async fn explicit_stop_written_before_zero_byte_cue_io_failure_or_queued_cue_expiry() {
    for expiry in [false,true] {let f=Fixture::with_motor(false,true,|_|{}).await;let (mut owner,bytes)=owner(&f,if expiry{None}else{Some(1)});
        let ids=press_motor(&f,4).await;f.time.fetch_add(21,Ordering::SeqCst);let stop=owner.step().await.unwrap().unwrap();assert_eq!(stop.command_id,ids[0]);stop.result.unwrap();
        f.time.fetch_add(if expiry{2001}else{21},Ordering::SeqCst);
        if expiry {assert!(owner.step().await.unwrap().is_none());}else{let cue=owner.step().await.unwrap().unwrap();assert!(cue.result.is_err());assert!(!cue.projection_cancelled,"raw physical IO error is not a known authority cancellation");}
        assert_eq!(bytes.lock().unwrap().as_slice(),&[vec![0xae,0x22,1,0xf0,0xee]]);
        assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[0]).unwrap(),Some(homesense_storage::journal::Status::Written));f.close().await;
    }
}

#[tokio::test]
async fn unified_away_origin_green_does_not_wait_for_feedback_and_legacy_reset_still_does() {
    for contextual in [false,true] {
        let f=Fixture::with_motor(false,true,|_|{}).await;let (mut owner,bytes)=owner(&f,None);
        let input=Request{key:6,..f.request()};let prepared=f.prepare(&input).await.unwrap();
        let Action::AwayOff{lease,..}=prepared.action else{panic!("reviewed Away mapping missing")};
        let actor=f.actor.with_source(if contextual{prepared.source}else{None});let ids=actor.submit_away_off(1,lease,true).await.unwrap();assert_eq!(ids.len(),13);
        for _ in 0..13 {f.time.fetch_add(21,Ordering::SeqCst);let Some(outcome)=owner.step().await.unwrap() else{break;};outcome.result.unwrap();}
        let sent=bytes.lock().unwrap().iter().flatten().copied().collect::<Vec<_>>();
        assert_eq!(sent.len(),if contextual{65}else{60});
        if contextual {assert_eq!(&sent[60..],&[0xae,0x41,6,0xf2,0xee]);
            assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap(),Some(homesense_storage::journal::Status::Written));
        } else {assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap(),Some(homesense_storage::journal::Status::Accepted));}
        assert!(f.feedback.snapshot().devices.values().all(|state|state.observed.is_none()),"no fake feedback supplied to open the legacy gate");
        f.close().await;
    }
}

#[tokio::test]
async fn successful_cross_key_and_ordinary_led_writes_retire_old_cue_without_raw_cursor_or_actuator_cancellation() {
    for linked in [false,true] {
        let mut f=Fixture::new(false,|_|{}).await;let (mut owner,bytes)=owner(&f,None);
        let first=f.prepare(&f.request()).await.unwrap();let old=first.source.clone().unwrap();let ids=f.submit(Some(old.clone()),f.batch(true,true,false)).await.unwrap();
        for _ in 0..2 {f.time.fetch_add(21,Ordering::SeqCst);owner.step().await.unwrap().unwrap().result.unwrap();}
        let prior_reply=first.context.finish(Reply::CommandsAccepted(ids)).unwrap();assert_eq!(prior_reply.indication.color,Some("red"));
        let mut intents=vec![Intent{device:DeviceId::new("legacy-key-1").unwrap(),command:Command::PanelLed(false)}];
        let source=if linked {
            intents.insert(0,Intent{device:DeviceId::new("legacy-key-3").unwrap(),command:Command::PanelLed(true)});
            intents.insert(0,Intent{device:DeviceId::new("fixture-light").unwrap(),command:Command::SetLevel{level:Percent::new(0).unwrap(),transition:LevelTransition::Immediate}});
            f.prepare(&Request{key:3,..f.request()}).await.unwrap().source
        }else{None};
        f.submit(source,Batch{controller:f.controller,intents,subscriptions:vec![]}).await.unwrap();
        if linked {
            for _ in 0..2 {f.time.fetch_add(21,Ordering::SeqCst);owner.step().await.unwrap().unwrap().result.unwrap();}
            f.time.fetch_add(21,Ordering::SeqCst);assert!(owner.step().await.unwrap().is_none(),"linked green cannot retire an old cue before target feedback");
            let at=f.time.fetch_add(2,Ordering::SeqCst)+2;f.feedback.apply(&crate::transport_owner::GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:0}]}}).unwrap();
            crate::control_batch::tick(&f.actor,f.controller,&f.feedback).await.unwrap();
        }
        let generation=f.actor.task_generation().await.unwrap();let requested=f.config.devices().keys().cloned().collect::<Vec<_>>();
        let before=crate::management_events::read_scoped(&f.actor,f.controller,generation.clone(),requested.clone(),&f.feedback,None,"synthetic-cue-wss").await.unwrap();
        assert_eq!(before.snapshot.keypads[0]["keys"][0]["indication"]["color"],"red");
        f.time.fetch_add(21,Ordering::SeqCst);owner.step().await.unwrap().unwrap().result.unwrap();
        let after=crate::management_events::read_scoped(&f.actor,f.controller,generation,requested,&f.feedback,Some(before.cursor.clone()),"synthetic-cue-wss").await.unwrap();
        assert_eq!(after.cursor,before.cursor,"LED software lifecycle must not invent physical feedback events");
        assert_ne!(after.snapshot.keypads,before.snapshot.keypads,"WSS compares this projection independently of raw cursor");
        let tombstone=&after.snapshot.keypads[0]["keys"][0]["indication"];
        assert_eq!(tombstone["status"],"retired");assert!(tombstone["color"].is_null());
        assert_eq!(tombstone["operation_sequence"],prior_reply.indication.operation_sequence);
        assert_eq!(after.snapshot.keypads[0]["keys"][0]["led"]["last_written"]["color"],"green");
        assert!(f.actor.with_source(Some(old.clone())).source_valid(),"retiring display cue must not cancel valid actuator/STOP authority");
        assert_eq!(indication(&old,None).unwrap().status,"retired","late response assembly retains terminal tombstone");
        assert_eq!(bytes.lock().unwrap().last().unwrap(),&[0xae,0x41,1,0xf2,0xee]);f.close().await;
    }
}

#[test]
fn exact_wire_fields_and_local_indicator_schema_are_strict() {
    let mut input=json!({"revision":"1","keypad":"panel","key":1,"request_id":"synthetic-1","gesture":"activate"});
    assert!(serde_json::from_value::<Request>(input.clone()).unwrap().validate().is_ok());
    for field in ["scene","target","color","endpoint_id","from_event"] {input[field]=json!("forbidden");assert!(serde_json::from_value::<Request>(input.clone()).is_err());input.as_object_mut().unwrap().remove(field);}
    for gesture in ["press","release","held","repeat"] {input["gesture"]=json!(gesture);assert!(serde_json::from_value::<Request>(input.clone()).is_err());}
    input["gesture"]=json!("activate");input["revision"]=json!("01");assert_eq!(serde_json::from_value::<Request>(input).unwrap().validate(),Err(Rejection::InvalidBody));
    let mut config:Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/devices.json")).unwrap();
    let valid=json!({"kind":"scene_selection","group":"reading-group","value":1,"selected_color":"red","unselected_color":"green","review_evidence":"synthetic exact original On/Off proof"});
    for mode in 0..5 {let mut cue=valid.clone();match mode {0=>{},1=>cue["group"]=json!("foreign"),2=>cue["value"]=json!(2),3=>cue["unselected_color"]=json!("red"),_=>cue["review_evidence"]=json!("")};config["keypads"][0]["keys"][0]["local_indicator"]=cue;
        let parsed=Configuration::parse(&serde_json::to_vec(&config).unwrap(),ExecutionContext::Simulation);
        let accepted=parsed.ok().is_some_and(|config|Document::parse(include_bytes!("../../../config/test/native-app-keypad/automation.json"),&config).is_ok());assert_eq!(accepted,mode==0);
    }
}

#[tokio::test]
async fn exact_source_mapping_scope_revision_and_revocation_precede_outputs() {
    for missing in ["legacy-key-1","fixture-light"] {
        let f=Fixture::new(true,|grant|{grant.devices.remove(missing);}).await;
        assert!(matches!(f.prepare(&f.request()).await,Err(Rejection::KeypadScopeDenied)));assert!(f.actor.dispatch().await.unwrap().is_none());f.close().await;
    }
    let f=Fixture::new(true,|_|{}).await;let input=f.request();let source=f.source(&input);
    assert!(!source.matches(&Action::KeypadAction{controller:f.controller,request:Request{key:2,..input.clone()}}));
    assert!(matches!(f.prepare(&Request{revision:"2".into(),..input.clone()}).await,Err(Rejection::StaleRevision)));
    assert!(matches!(f.prepare(&Request{keypad:"foreign".into(),..input.clone()}).await,Err(Rejection::KeypadUnavailable)));
    f.tokens.revoke(f.credential).unwrap();assert!(!source.valid());assert!(f.actor.dispatch().await.unwrap().is_none());f.close().await;
}

#[tokio::test]
async fn source_led_precedes_lights_and_linked_led_still_requires_exact_post_write_feedback() {
    let mut f=Fixture::new(false,|_|{}).await;let prepared=f.prepare(&f.request()).await.unwrap();let source=prepared.source.clone().unwrap();
    f.submit(Some(source.clone()),f.batch(true,true,true)).await.unwrap();
    let local=f.dispatch().await;assert_eq!(local.command.device_id.as_str(),"legacy-key-1");assert_eq!(local.command.command,Command::PanelLed(true));assert!(local.feedback_barrier.is_none());f.finish(&local).await;
    let light=f.dispatch().await;assert_eq!(light.command.device_id.as_str(),"fixture-light");f.finish(&light).await;
    f.time.fetch_add(2,Ordering::SeqCst);assert!(f.actor.dispatch().await.unwrap().is_none());
    let at=f.time.fetch_add(2,Ordering::SeqCst)+2;f.feedback.apply(&crate::transport_owner::GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:36}]}}).unwrap();
    crate::control_batch::tick(&f.actor,f.controller,&f.feedback).await.unwrap();assert!(f.actor.dispatch().await.unwrap().is_none(),"ramp intermediate cannot release linkedLED");
    let at=f.time.fetch_add(2,Ordering::SeqCst)+2;f.feedback.apply(&crate::transport_owner::GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:40}]}}).unwrap();
    crate::control_batch::tick(&f.actor,f.controller,&f.feedback).await.unwrap();let linked=f.dispatch().await;assert_eq!(linked.command.device_id.as_str(),"legacy-key-2");assert!(linked.feedback_barrier.is_some());f.finish(&linked).await;
    let receipt=prepared.context.finish(Reply::CommandsAccepted(vec![local.command.id,light.command.id,linked.command.id])).unwrap();assert_eq!(receipt.indication.color,Some("red"));assert!(!receipt.indication.observed);
    let views=crate::management_devices::snapshot(&f.actor,f.controller,f.actor.task_generation().await.unwrap(),f.config.devices().keys().cloned().collect(),&f.feedback).await.unwrap();assert!(views.keypads[0]["keys"][0]["led"]["observed"].is_null());assert_eq!(views.keypads[0]["keys"][0]["indication"]["status"],"written");f.close().await;
}

#[tokio::test]
async fn reviewed_missing_source_led_is_added_only_to_authenticated_origin_not_room_scene() {
    for contextual in [false,true] {let f=Fixture::new(true,|_|{}).await;let prepared=f.prepare(&f.request()).await.unwrap();
        let ids=f.submit(if contextual{prepared.source.clone()}else{None},f.batch(true,false,false)).await.unwrap();assert_eq!(ids.len(),if contextual{2}else{1});
        let first=f.dispatch().await;assert_eq!(first.command.device_id.as_str(),if contextual{"legacy-key-1"}else{"fixture-light"});f.finish(&first).await;
        if contextual {let light=f.dispatch().await;assert_eq!(light.command.device_id.as_str(),"fixture-light");f.finish(&light).await;}
        f.close().await;
    }
}

#[tokio::test]
async fn latest_successful_source_retires_older_cue_and_late_phase_without_replaying() {
    let f=Fixture::new(true,|_|{}).await;let first=f.prepare(&f.request()).await.unwrap();let old=first.source.clone().unwrap();
    f.submit(Some(old.clone()),f.batch(true,false,true)).await.unwrap();
    let next=f.prepare(&f.request()).await.unwrap();let latest=next.source.clone().unwrap();f.submit(Some(latest.clone()),f.batch(false,true,false)).await.unwrap();
    assert!(!old.indication_current());assert!(latest.indication_current());
    assert!(f.submit(Some(old),f.batch(true,true,true)).await.is_err());
    assert_eq!(first.context.finish(Reply::CommandsAccepted(vec![])).unwrap().indication.status,"superseded");
    let local=f.dispatch().await;assert_eq!(local.command.command,Command::PanelLed(false));f.finish(&local).await;let light=f.dispatch().await;assert!(matches!(light.command.command,Command::SetLevel{level,..} if level.get()==0));f.finish(&light).await;
    assert!(f.actor.dispatch().await.unwrap().is_none());f.close().await;
}

#[tokio::test]
async fn canonical_motor_stop_leds_retire_temporary_source_cue_without_resurrecting_late_http() {
    let f=Fixture::with_motor(false,true,|_|{}).await;
    let (mut wire_owner,bytes)=owner(&f,None);
    press_motor(&f,2).await;
    for _ in 0..2 {f.time.fetch_add(21,Ordering::SeqCst);wire_owner.step().await.unwrap().unwrap().result.unwrap();}
    let input=Request{key:2,..f.request()};
    let prepared=f.prepare(&input).await.unwrap();
    let source=prepared.source.clone().unwrap();
    let Action::DeviceCommand{device,action:crate::management_devices::DeviceAction::KeypadPress{keypad,key},..}=prepared.action else{panic!("reviewed repeat mapping missing")};
    let ids=f.actor.with_source(Some(source.clone())).submit_keypad_press_for_revision(1,f.controller,device,keypad,key,f.time.load(Ordering::SeqCst)+2000).await.unwrap();
    f.time.fetch_add(21,Ordering::SeqCst);
    let stop=wire_owner.step().await.unwrap().unwrap();
    assert_eq!(stop.command_id,ids[0]);stop.result.unwrap();
    f.time.fetch_add(21,Ordering::SeqCst);wire_owner.step().await.unwrap().unwrap().result.unwrap();
    let earlier_http=prepared.context.finish(Reply::CommandsAccepted(ids)).unwrap();
    assert_eq!(earlier_http.indication.color,Some("green"));
    // Advance the actual actor-owned canonical model, with no fabricated panel
    // or position feedback and without calling its LED helper directly.
    for _ in 0..16 {f.time.fetch_add(21,Ordering::SeqCst);if let Some(outcome)=wire_owner.step().await.unwrap(){outcome.result.unwrap();}}
    let sent=bytes.lock().unwrap().clone();
    assert_eq!(&sent[2..4],&[vec![0xae,0x22,1,0xf0,0xee],vec![0xae,0x41,2,0xf2,0xee]]);
    let view=crate::management_devices::snapshot(&f.actor,f.controller,f.actor.task_generation().await.unwrap(),f.config.devices().keys().cloned().collect(),&f.feedback).await.unwrap();
    let keys=view.keypads[0]["keys"].as_array().unwrap();
    for (index,color) in [(1,"red"),(2,"red"),(3,"green")] {assert_eq!(keys[index]["led"]["last_written"]["color"],color);}
    let tombstone=&keys[1]["indication"];
    assert_eq!(tombstone["status"],"retired");assert!(tombstone["color"].is_null());
    assert_eq!(tombstone["operation_sequence"],earlier_http.indication.operation_sequence);
    assert_eq!(indication(&source,None).unwrap().status,"retired");
    assert!(f.actor.with_source(Some(source)).source_valid(),"canonical LED overwrite only retires display authority");
    f.close().await;
}

#[test]
fn maximum_permission_scope_keeps_receipt_fingerprint_bounded_and_changes_with_grant() {
    let tokens=Arc::new(Tokens::default());let id=Uuid::new_v4();
    let mut grant=Grant{controller:Uuid::new_v4(),role:Role::Member,
        devices:(0..crate::web_auth::MAX_DEVICE_SCOPE).map(|n|format!("device-{n:04}-{}","x".repeat(240))).collect(),
        scenes:(0..4095).map(|n|format!("scene-{n:04}-{}","x".repeat(240))).collect()};
    tokens.insert(id,SECRET,grant.clone()).unwrap();
    let header=format!("Bearer {SECRET}");let lease=tokens.module_off_lease(Some(&header),None,None).unwrap();
    let request=Request{revision:u64::MAX.to_string(),keypad:"\"".repeat(250),key:6,request_id:"x".repeat(128),gesture:Gesture::Activate};
    request.validate().unwrap();
    assert_eq!(Request{keypad:"invalid\nkey".into(),..request.clone()}.validate(),Err(Rejection::InvalidBody));
    let first=fingerprint(&request,&lease);assert!(first.len()<=1024,"must fit the existing receipt identity bound");
    let removed=grant.devices.first().unwrap().clone();grant.devices.remove(&removed);
    tokens.insert(id,SECRET,grant).unwrap();
    let replacement=tokens.module_off_lease(Some(&header),None,None).unwrap();
    assert_eq!(replacement.credential_id(),lease.credential_id());assert_ne!(fingerprint(&request,&replacement),first);
}

#[tokio::test]
async fn cancelled_queued_source_cue_has_no_color_while_source_remains_current() {
    let f=Fixture::new(false,|_|{}).await;
    let prepared=f.prepare(&f.request()).await.unwrap();let source=prepared.source.unwrap();
    let ids=f.submit(Some(source.clone()),f.batch(true,true,false)).await.unwrap();
    f.time.fetch_add(2001,Ordering::SeqCst);
    assert!(f.actor.dispatch().await.unwrap().is_none(),"expired queue must produce no write ticket");
    let status=Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[0]).unwrap().unwrap();
    assert_eq!(status,homesense_storage::journal::Status::Cancelled);
    assert!(source.valid());assert!(source.indication_current(),"test must not turn cancellation into supersession");
    let state=indication(&source,Some(status)).unwrap();assert_eq!(state.status,"cancelled");assert_eq!(state.color,None);assert!(!state.observed);
    let snapshot=crate::management_devices::snapshot(&f.actor,f.controller,f.actor.task_generation().await.unwrap(),f.config.devices().keys().cloned().collect(),&f.feedback).await.unwrap();
    assert_eq!(snapshot.keypads[0]["keys"][0]["indication"]["status"],"cancelled");
    assert!(snapshot.keypads[0]["keys"][0]["indication"]["color"].is_null());
    f.close().await;
}
