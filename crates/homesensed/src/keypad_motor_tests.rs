//! Deterministic actor-level card presses. No socket, hardware or field IDs.
use crate::{admission::{Admission,Options}, automation_control::Action, management_devices::DeviceAction,
    source_auth::Source, web_auth::{Grant,Role,Tokens}};
use homesense_core::{Command,DeviceId,ExecutionContext,MotorAction,MotorMotion};
use homesense_storage::Store;
use serde_json::{Value,json};
use std::{path::PathBuf,sync::{Arc,atomic::{AtomicU64,Ordering}}};
use uuid::Uuid;
const SECRET:&str="synthetic-keypad-card-source-0123456789";
fn definition()->Value {
    json!({"schema_version":2,"revision":1,"gateways":[
        {"id":"motor-bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]},
        {"id":"panel-bus","endpoint":"127.0.0.1:2","modules":[{"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],
        "devices":[{"id":"curtain","name":"Synthetic motor","module_id":"motor-module","channel":1,"enabled":true,
          "motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},
          "curtain_state":{"open_travel_ms":10000,"close_travel_ms":10000,"stop_led_idle_ms":1,"evidence":"synthetic","reviewed_at_unix_ms":1}},
          {"id":"other-curtain","name":"Other motor","module_id":"motor-module","channel":2,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}],
        "keypads":[{"id":"panel","name":"Synthetic panel","module_id":"panel-module","enabled":true,
          "keys":[{"number":1,"label":"Open","led_endpoint":{"id":"key-open","name":"Open LED","enabled":true},"action":{"kind":"motor","device":"curtain","action":"open","press_behavior":"repeat_to_stop","review_evidence":"synthetic native card only"}},
          {"number":2,"label":"Close","led_endpoint":{"id":"key-close","name":"Close LED","enabled":true},"action":{"kind":"motor","device":"curtain","action":"close","press_behavior":"repeat_to_stop","review_evidence":"synthetic native card only"}},
          {"number":3,"label":"Stop","led_endpoint":{"id":"key-stop","name":"Stop LED","enabled":true},"action":{"kind":"motor","device":"curtain","action":"stop","review_evidence":"synthetic absolute stop"}}]}]})
}
struct Fixture {actor:Admission,path:PathBuf,tokens:Arc<Tokens>,credential:Uuid,controller:Uuid,time:&'static AtomicU64,config:homesense_config::Configuration}
impl Fixture {
    async fn new(mutator:impl FnOnce(&mut Value),devices:&[&str])->Self {
        let path=std::env::temp_dir().join(format!("keypad-card-{}.sqlite",Uuid::new_v4()));
        let mut v=definition();mutator(&mut v);let bytes=serde_json::to_vec(&v).unwrap();
        let config=homesense_config::Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
        Store::open(&path).unwrap().publish_configuration(None,&bytes,ExecutionContext::Simulation,&[]).unwrap();
        let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();let controller=Uuid::new_v4();
        tokens.insert(credential,SECRET,Grant{controller,role:Role::Member,devices:devices.iter().map(|s|(*s).into()).collect(),scenes:Default::default()}).unwrap();
        let time=Box::leak(Box::new(AtomicU64::new(100)));
        // An independent principal owner remains live after exact source revoke.
        let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(20),..Default::default()},move |_|true,time).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        Self{actor,path,tokens,credential,controller,time,config}
    }
    fn scoped(&self,controller:Uuid,key:u8)->Admission {
        let lease=self.tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap();
        let action=Action::DeviceCommand{controller,revision:1,device:DeviceId::new("curtain").unwrap(),action:DeviceAction::KeypadPress{keypad:"panel".into(),key}};
        self.actor.with_source(Source::capture(lease,&action,None))
    }
    async fn press(&self,key:u8)->Result<Vec<u64>,hap::server::WriteStatus> {
        self.scoped(self.controller,key).submit_keypad_press_for_revision(1,self.controller,DeviceId::new("curtain").unwrap(),"panel".into(),key,100000).await
    }
    fn advance(&self,ms:u64) {self.time.fetch_add(ms,Ordering::SeqCst);}
    async fn write(&self,expected:MotorMotion) {
        self.advance(21);let ticket=self.actor.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.command,Command::Motor(expected));
        self.actor.finish_write(ticket.command.id,true).await.unwrap();
    }
    async fn close(self) {self.actor.shutdown().await.unwrap();for suffix in ["","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",self.path.display()));}let _=std::fs::remove_file(self.path.with_extension("runtime.lock"));}
}
const SCOPE:&[&str]=&["curtain","key-open","key-close","key-stop"];
#[tokio::test]
async fn keypad_card_atomic_written_direction_repeat_stop_reverse_pending_and_endpoint() {
    let f=Fixture::new(|_|{},SCOPE).await;
    f.press(1).await.unwrap();
    assert_eq!(f.press(1).await,Err(hap::server::WriteStatus::ResourceBusy),"Accepted is not Written");
    f.advance(2);let writing=f.actor.dispatch().await.unwrap().unwrap();
    assert_eq!(f.press(1).await,Err(hap::server::WriteStatus::ResourceBusy),"Writing is not Written");
    f.actor.finish_write(writing.command.id,true).await.unwrap();
    f.press(1).await.unwrap();f.write(MotorMotion::Stop).await;
    f.press(1).await.unwrap();f.write(MotorMotion::Down).await;
    f.press(2).await.unwrap();f.write(MotorMotion::Up).await;
    f.press(2).await.unwrap();f.write(MotorMotion::Stop).await;
    f.press(2).await.unwrap();f.write(MotorMotion::Up).await;
    f.advance(10001); // estimated closed endpoint is Stopped, never a repeat STOP.
    f.press(2).await.unwrap();f.write(MotorMotion::Up).await;
    // Independent absolute paths keep Open absolute while already Opening.
    f.actor.submit_motor(f.controller,DeviceId::new("curtain").unwrap(),MotorAction::Open).await.unwrap();f.write(MotorMotion::Down).await;
    f.actor.submit_motor(f.controller,DeviceId::new("curtain").unwrap(),MotorAction::Open).await.unwrap();f.write(MotorMotion::Down).await;
    f.close().await;
}
#[tokio::test]
async fn keypad_card_exact_key_and_target_scope_no_parent_scope_and_revocation() {
    for devices in [vec!["curtain"],vec!["curtain","key-open"]] {
        let f=Fixture::new(|_|{},&devices).await;
        assert!(!devices.contains(&"panel"));
        if devices.len()==1 {assert_eq!(f.press(1).await,Err(hap::server::WriteStatus::InsufficientPrivileges));}
        else {
            f.press(1).await.unwrap();
            assert_eq!(f.press(2).await,Err(hap::server::WriteStatus::InsufficientPrivileges),"exact sub-endpoint, not just shared motor");
            f.tokens.revoke(f.credential).unwrap();
            assert!(f.actor.dispatch().await.unwrap().is_none(),"revoked source cannot write despite live same controller");
        }
        f.close().await;
    }
}
#[tokio::test]
async fn keypad_card_resolves_current_mapping_revision_and_unknown_without_led_guessing() {
    use hap::server::WriteStatus as W;
    for mode in [0,1,2,3] {
        let f=Fixture::new(|v|match mode {
            0=>{for field in ["open_travel_ms","close_travel_ms"] {v["devices"][0]["curtain_state"].as_object_mut().unwrap().remove(field);}},
            1=>v["keypads"][0]["keys"][0]["action"]["press_behavior"]=json!("absolute"),
            2=>v["keypads"][0]["keys"][0]["led_endpoint"]["enabled"]=json!(false),
            _=>v["keypads"][0]["enabled"]=json!(false),
        },SCOPE).await;
        if mode==0 {
            f.press(1).await.unwrap();f.write(MotorMotion::Down).await;
            f.press(1).await.unwrap();f.write(MotorMotion::Stop).await;
        }else {assert_eq!(f.press(1).await,Err(W::InvalidValueInRequest));}
        f.close().await;
    }
    let f=Fixture::new(|_|{},SCOPE).await;
    let scoped=f.scoped(f.controller,1);
    for (rev,device,parent,key) in [(2,"curtain","panel",1),(1,"other-curtain","panel",1),(1,"curtain","missing",1),(1,"curtain","panel",3),(1,"curtain","panel",7)] {
        assert!(scoped.submit_keypad_press_for_revision(rev,f.controller,DeviceId::new(device).unwrap(),parent.into(),key,100000).await.is_err());
    }
    assert_eq!(f.actor.submit_keypad_press_for_revision(1,f.controller,DeviceId::new("curtain").unwrap(),"panel".into(),1,100000).await,Err(W::InsufficientPrivileges));
    f.press(1).await.unwrap();f.write(MotorMotion::Down).await;f.close().await;
}
#[tokio::test]
async fn keypad_card_uses_shared_other_principal_trajectory_and_restart_discards_it() {
    let f=Fixture::new(|_|{},SCOPE).await;
    let other=Uuid::new_v4();
    f.actor.submit_motor(other,DeviceId::new("curtain").unwrap(),MotorAction::Open).await.unwrap();f.write(MotorMotion::Down).await;
    f.press(1).await.unwrap();f.write(MotorMotion::Stop).await;
    f.actor.submit_motor(other,DeviceId::new("curtain").unwrap(),MotorAction::Close).await.unwrap();f.write(MotorMotion::Up).await;
    f.press(2).await.unwrap();f.write(MotorMotion::Stop).await;
    f.actor.shutdown().await.unwrap();
    let actor=Admission::open_with_test_time(f.path.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(20),..Default::default()},move |_|true,f.time).await.unwrap();actor.synchronized(1,1).await.unwrap();
    let f=Fixture{actor,..f};f.press(1).await.unwrap();f.write(MotorMotion::Down).await;f.close().await;
}
#[tokio::test]
async fn keypad_card_parent_and_target_wires_both_gate_without_unrelated_bus_blocking() {
    use homesense_core::gateway_scope::{WireScope,Port,ReadRequirement};
    let f=Fixture::new(|_|{},SCOPE).await;
    let initial=f.config.gateways().iter().map(|(gateway,object)|crate::gateway_runtime::Initial{
        wire:WireScope::from_persisted(gateway.clone(),1).unwrap(),samples:object.modules().values().filter(|m|matches!(m.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer)).flat_map(|m|(1..=m.channels()).map(move|channel|crate::gateway_runtime::Sample{requirement:ReadRequirement{port:Port{address:m.address().get(),channel},before_count:0,sent_at_ms:0,deadline_ms:100},count:1,at:0})).collect()}).collect();
    f.actor.activate_gateways(initial).await.unwrap();
    f.press(1).await.unwrap();
    f.actor.gateway_fault(WireScope::from_persisted("panel-bus".into(),1).unwrap()).await.unwrap();
    assert!(f.actor.dispatch().await.unwrap().is_none(),"parent bus loss cancels already accepted press on target bus");
    assert!(f.press(1).await.is_err(),"offline parent cannot drive ready motor");
    f.actor.submit_motor(f.controller,DeviceId::new("curtain").unwrap(),MotorAction::Stop).await.unwrap();f.write(MotorMotion::Stop).await;
    f.close().await;
}
#[tokio::test]
async fn keypad_card_revoked_old_written_source_cannot_turn_new_valid_source_into_stop() {
    let f=Fixture::new(|_|{},SCOPE).await;
    let old_credential=Uuid::new_v4();let old_controller=Uuid::new_v4();
    const OLD:&str="synthetic-old-motor-source-0123456789";
    f.tokens.insert(old_credential,OLD,Grant{controller:old_controller,role:Role::Member,devices:["curtain".into()].into(),scenes:Default::default()}).unwrap();
    let action=Action::DeviceCommand{controller:old_controller,revision:1,device:DeviceId::new("curtain").unwrap(),action:DeviceAction::Open};
    let lease=f.tokens.module_off_lease(Some(&format!("Bearer {OLD}")),None,None).unwrap();
    let old=f.actor.with_source(Source::capture(lease,&action,None));
    old.submit_motor_for_revision(1,old_controller,DeviceId::new("curtain").unwrap(),MotorAction::Open).await.unwrap();f.write(MotorMotion::Down).await;
    // Let the owner wait idle after its last audit; controller authorization
    // remains true through the independent owner, only this exact source goes.
    f.actor.task_generation().await.unwrap();
    f.tokens.revoke(old_credential).unwrap();
    f.press(1).await.unwrap();f.write(MotorMotion::Down).await;
    f.close().await;
}
#[test]
fn keypad_source_dependency_fence_validates_both_generations_without_recursive_lock() {
    let tokens=Arc::new(Tokens::default());let controller=Uuid::new_v4();let a=Uuid::new_v4();let b=Uuid::new_v4();
    const OTHER:&str="synthetic-source-dependency-0123456789";
    let grant=Grant{controller,role:Role::Member,devices:["curtain".into()].into(),scenes:Default::default()};
    tokens.insert(a,SECRET,grant.clone()).unwrap();tokens.insert(b,OTHER,grant).unwrap();
    let action=Action::DeviceCommand{controller,revision:1,device:DeviceId::new("curtain").unwrap(),action:DeviceAction::Open};
    let source=|secret:&str|Source::capture(tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap(),&action,None).unwrap();
    let fresh=source(SECRET);let old=source(OTHER);
    assert_eq!(crate::source_auth::commit_with_dependency(Some(&fresh),Some(&old),||7),Some(7));
    tokens.revoke(b).unwrap();let mut written=false;
    assert_eq!(crate::source_auth::commit_with_dependency(Some(&fresh),Some(&old),||{written=true;7}),None);
    assert!(!written);assert!(fresh.valid());
}
