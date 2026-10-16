//! Offline native-key authorization/atomic batch/green dependency boundaries.
use crate::{admission::{Admission,Options},away_auth::{Authorization,KeypadCommand},web_auth::{Grant,Role,Tokens},transport_owner::send_ticket};
use homesense_core::{Command,DeviceId,ExecutionContext,gateway_scope::{WireScope,Port,ReadRequirement}};
use homesense_storage::{Store,journal::Status};
use serde_json::{Value,json};
use std::{collections::BTreeSet,path::PathBuf,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},io::{Read,Write}};
use uuid::Uuid;
const SECRET:&str="synthetic-keypad-away-source-0123456789";
fn documents(reset:bool,supply:bool)->(Value,Value,BTreeSet<String>) {
    let mut modules_a=vec![];let mut modules_b=vec![];let mut devices=vec![];let mut reviews=vec![];let mut scope=BTreeSet::new();
    for n in 0..12 {
        let name=format!("output-{n}");let relay=n<7;let address=if relay{0x20+n}else{0x40+n-7};
        let module=json!({"id":name,"address":format!("{address:02X}"),"profile":if relay{"r8i8_din_pro_v2_7_7"}else{"ad8_din_rs485_v2_2_1"},"verification":{"status":"simulated"}});
        if relay||supply {modules_a.push(module);}else{modules_b.push(module);}
        let mut ids=vec![];
        for channel in 1..=8 {let id=format!("{name}:{channel}");scope.insert(id.clone());ids.push(id.clone());devices.push(json!({"id":id,"name":"Synthetic output","module_id":name,"channel":channel,"enabled":true,"catalog_visible":n==0}));}
        reviews.push(json!({"module_id":name,"devices":ids,"review_evidence":"synthetic full8 reviewed"}));
    }
    modules_a.push(json!({"id":"panel-module","address":"6A","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
    let mut action=json!({"kind":"away_off","review_evidence":"synthetic native key only"});if reset {action["indicator_reset"]=json!("green");}
    let mut gateways=vec![json!({"id":"a","endpoint":"127.0.0.1:1","modules":modules_a})];if !modules_b.is_empty(){gateways.push(json!({"id":"b","endpoint":"127.0.0.1:2","modules":modules_b}));}
    if supply {devices[56]["supply"]=json!({"module_id":"output-0","channel":1});devices[56]["default_on_level"]=json!(100);}
    let config=json!({"schema_version":2,"revision":1,"gateways":gateways,"devices":devices,"keypads":[{"id":"panel","name":"Synthetic entry","module_id":"panel-module","enabled":true,"keys":[{"number":6,"label":"Away","led_endpoint":{"id":"key6","name":"Synthetic reset","enabled":true},"action":action}]}]});
    let rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"review_evidence":"synthetic explicit96","active_execution_policy":"cancel_affected_lighting","modules":reviews}});
    scope.insert("key6".into());(config,rules,scope)
}
struct F {actor:Admission,path:PathBuf,tokens:Arc<Tokens>,credential:Uuid,controller:Uuid,time:&'static AtomicU64,config:homesense_config::Configuration,rules:homesense_config::rule_document::Document}
impl F {
    async fn new(reset:bool,supply:bool,scope_change:impl FnOnce(&mut BTreeSet<String>))->Self {
        let(v,r,mut scope)=documents(reset,supply);scope_change(&mut scope);let bytes=serde_json::to_vec(&v).unwrap();let rules=serde_json::to_vec(&r).unwrap();
        let path=std::env::temp_dir().join(format!("keypad-away-{}.sqlite",Uuid::new_v4()));let bundle=Store::open(&path).unwrap().publish_automation(None,&bytes,&rules,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let credential=Uuid::new_v4();let tokens=Arc::new(Tokens::default());tokens.insert(credential,SECRET,Grant{controller,role:Role::Member,devices:scope,scenes:BTreeSet::new()}).unwrap();
        let time=Box::leak(Box::new(AtomicU64::new(100)));let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move |_|true,time).await.unwrap();actor.synchronized(1,1).await.unwrap();
        Self{actor,path,tokens,credential,controller,time,config:bundle.configuration,rules:bundle.automation.unwrap()}
    }
    fn lease(&self,keypad:bool)->Authorization {
        let lease=self.tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap();
        if keypad {Authorization::keypad(lease,&KeypadCommand::KeypadPress{keypad:"panel".into(),key:6}).unwrap()}else{lease.into()}
    }
    fn step(&self){self.time.fetch_add(21,Ordering::SeqCst);}
    async fn local(&self){
        let initial=self.config.gateways().iter().map(|(gateway,g)|crate::gateway_runtime::Initial{wire:WireScope::from_persisted(gateway.clone(),1).unwrap(),samples:g.modules().values().filter(|m|matches!(m.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer)).flat_map(|m|(1..=8).map(move|channel|crate::gateway_runtime::Sample{requirement:ReadRequirement{port:Port{address:m.address().get(),channel},before_count:0,sent_at_ms:0,deadline_ms:100},count:1,at:0})).collect()}).collect();self.actor.activate_gateways(initial).await.unwrap();
    }
    async fn close(self){self.actor.shutdown().await.unwrap();for suffix in ["","-wal","-shm"]{let _=std::fs::remove_file(format!("{}{suffix}",self.path.display()));}let _=std::fs::remove_file(self.path.with_extension("runtime.lock"));}
}
#[tokio::test]
async fn keypad_away_cross_bus_reset_waits_for_every_written_module_and_direct_stays12() {
    let f=F::new(true,false,|_|{}).await;f.local().await;
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();assert_eq!(ids.len(),13);
    for _ in 0..7 {f.step();let t=f.actor.dispatch_available(["a".into()].into()).await.unwrap().unwrap();assert_eq!(t.command.command,Command::ModuleAllOff);f.actor.finish_write(t.command.id,true).await.unwrap();}
    f.step();assert!(f.actor.dispatch_available(["a".into()].into()).await.unwrap().is_none(),"green on available A cannot overtake five unWritten B modules");
    assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap(),Some(Status::Accepted));
    for _ in 0..5 {f.step();let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::ModuleAllOff);f.actor.finish_write(t.command.id,true).await.unwrap();}
    f.step();let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::PanelLed(false));assert_eq!(t.command.device_id,DeviceId::new("key6").unwrap());let bytes=Arc::new(Mutex::new(vec![]));let mut connection=hb_transport::Connection::for_simulation(Bytes(bytes.clone()),1);send_ticket(&t,&mut connection,||f.time.load(Ordering::SeqCst)).unwrap();assert_eq!(bytes.lock().unwrap().as_slice(),&[0xae,0x6a,6,0xf2,0xee]);f.actor.finish_write(t.command.id,true).await.unwrap();
    assert_eq!(f.actor.submit_away_off(1,f.lease(false),true).await.unwrap().len(),12);
    f.close().await;
}
#[tokio::test]
async fn keypad_away_partial_write_never_resets_and_restart_replays_nothing() {
    let f=F::new(true,false,|_|{}).await;let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();
    for _ in 0..4 {f.step();let t=f.actor.dispatch().await.unwrap().unwrap();f.actor.finish_write(t.command.id,true).await.unwrap();}
    f.step();let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::ModuleAllOff);f.actor.finish_write(t.command.id,false).await.unwrap();
    let status=Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap();assert_ne!(status,Some(Status::Written));assert_ne!(status,Some(Status::Observed));
    f.actor.shutdown().await.unwrap();let actor=Admission::open_with_test_time(f.path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move |_|true,f.time).await.unwrap();actor.synchronized(1,1).await.unwrap();assert!(actor.dispatch().await.unwrap().is_none());let f=F{actor,..f};f.close().await;
}
#[tokio::test]
async fn keypad_away_exact_key_full_scope_hidden_supply_and_no_parent_grant() {
    for missing in ["key6","output-11:8"] {
        let f=F::new(true,false,|scope|{scope.remove(missing);}).await;let lease=f.lease(true);
        assert!(f.actor.submit_away_off(1,lease,true).await.is_err());assert!(f.actor.dispatch().await.unwrap().is_none());f.close().await;
    }
    let f=F::new(true,true,|scope|{scope.remove("output-0:1");}).await;
    let grant=f.tokens.authenticate(&format!("Bearer {SECRET}")).unwrap();assert!(!grant.devices.contains("panel"));
    assert!(crate::away_off::catalog_scope(&f.config,&f.rules,&[]).unwrap().permitted(&grant));
    assert_eq!(f.actor.submit_away_off(1,f.lease(true),true).await.unwrap().len(),13);f.close().await;
    let f=F::new(true,true,|scope|{scope.remove("output-7:1");}).await;
    let grant=f.tokens.authenticate(&format!("Bearer {SECRET}")).unwrap();assert!(!crate::away_off::catalog_scope(&f.config,&f.rules,&[]).unwrap().permitted(&grant));
    assert!(f.actor.submit_away_off(1,f.lease(true),true).await.is_err());f.close().await;
}
#[tokio::test]
async fn keypad_away_parent_fault_cancels_full_decision_without_raising_authority() {
    let f=F::new(true,false,|_|{}).await;f.local().await;
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();
    f.actor.gateway_fault(WireScope::from_persisted("a".into(),1).unwrap()).await.unwrap();
    assert!(f.actor.dispatch_available(["b".into()].into()).await.unwrap().is_none(),"B children retain A key dependency");
    for id in ids {assert_ne!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),id).unwrap(),Some(Status::Written));}
    f.close().await;
}
struct Bytes(Arc<Mutex<Vec<u8>>>);
impl Read for Bytes{fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for Bytes{fn write(&mut self,b:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().extend_from_slice(b);Ok(b.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
#[tokio::test]
async fn keypad_away_revoke_after_ticket_before_actual_write_sends_zero_bytes() {
    let f=F::new(true,false,|_|{}).await;f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();f.step();let ticket=f.actor.dispatch().await.unwrap().unwrap();
    f.tokens.revoke(f.credential).unwrap();let written=Arc::new(Mutex::new(vec![]));let mut connection=hb_transport::Connection::for_simulation(Bytes(written.clone()),1);
    let result=send_ticket(&ticket,&mut connection,||f.time.load(Ordering::SeqCst));assert!(result.is_err());assert!(written.lock().unwrap().is_empty());f.actor.finish_write(ticket.command.id,false).await.unwrap();f.close().await;
}
#[test]
fn keypad_away_command_dto_is_strict_and_legacy_command_is_not_injected() {
    for value in [json!({"action":"open","keypad":"panel","key":6}),json!({"action":"keypad_press","keypad":"panel","key":6,"scope":["anything"]})] {assert!(serde_json::from_value::<KeypadCommand>(value).is_err());}
    let(v,mut rules,_)=documents(true,false);let config=homesense_config::Configuration::parse(&serde_json::to_vec(&v).unwrap(),ExecutionContext::Simulation).unwrap();
    assert!(homesense_config::rule_document::Document::parse(&serde_json::to_vec(&rules).unwrap(),&config).is_ok());rules["away_off"]["enabled"]=json!(false);
    assert!(homesense_config::rule_document::Document::parse(&serde_json::to_vec(&rules).unwrap(),&config).is_err());
}
#[tokio::test]
async fn denied_keypad_away_does_not_cancel_preexisting_authorized_work() {
    let f=F::new(true,false,|scope|{scope.remove("output-11:8");}).await;
    let old=f.actor.submit(homesense_hap::requests::Batch{controller:f.controller,intents:vec![homesense_hap::requests::Intent{device:DeviceId::new("output-0:1").unwrap(),command:Command::Switch(true)}],subscriptions:vec![]}).await.unwrap();
    assert!(f.actor.submit_away_off(1,f.lease(true),true).await.is_err());f.step();let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.id,old[0]);assert_eq!(t.command.command,Command::Switch(true));f.actor.finish_write(t.command.id,true).await.unwrap();f.close().await;
}
