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
        Self::new_documents(reset,supply,|_,_,scope|scope_change(scope)).await
    }
    async fn new_documents(reset:bool,supply:bool,change:impl FnOnce(&mut Value,&mut Value,&mut BTreeSet<String>))->Self {
        let(mut v,mut r,mut scope)=documents(reset,supply);change(&mut v,&mut r,&mut scope);let bytes=serde_json::to_vec(&v).unwrap();let rules=serde_json::to_vec(&r).unwrap();
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
async fn all_off_feedback(f:&F){
    f.step();let mut feedback=crate::feedback::Feedback::new(&f.config,ExecutionContext::Simulation,Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),1).unwrap();
    for(gateway,config)in f.config.gateways(){
        let frames=config.modules().values().filter(|m|matches!(m.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer)).flat_map(|m|(1..=m.channels()).map(move|ch|hb_protocol::RawFrame{address:m.address().get(),command:0xd0+ch,value:if m.kind()==homesense_core::ModuleKind::Switch{0xf2}else{0}})).collect();
        feedback.apply(&crate::transport_owner::GatewayRead{gateway_id:gateway.clone(),transport_epoch:1,received_at_ms:f.time.load(Ordering::SeqCst),result:hb_transport::Receive::Frames{transport_epoch:1,frames}}).unwrap();
    }
    crate::control_batch::tick(&f.actor,f.controller,&feedback).await.unwrap();
}
#[tokio::test]
async fn keypad_away_cross_bus_reset_waits_for_every_written_module_and_direct_stays12() {
    let f=F::new(true,false,|_|{}).await;f.local().await;
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();assert_eq!(ids.len(),13);
    for _ in 0..7 {f.step();let t=f.actor.dispatch_available(["a".into()].into()).await.unwrap().unwrap();assert_eq!(t.command.command,Command::ModuleAllOff);f.actor.finish_write(t.command.id,true).await.unwrap();}
    f.step();assert!(f.actor.dispatch_available(["a".into()].into()).await.unwrap().is_none(),"green on available A cannot overtake five unWritten B modules");
    assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap(),Some(Status::Accepted));
    for _ in 0..5 {f.step();let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::ModuleAllOff);f.actor.finish_write(t.command.id,true).await.unwrap();}
    f.step();assert!(f.actor.dispatch().await.unwrap().is_none(),"Written alone cannot release Away LED");all_off_feedback(&f).await;let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::PanelLed(false));assert_eq!(t.command.device_id,DeviceId::new("key6").unwrap());let bytes=Arc::new(Mutex::new(vec![]));let mut connection=hb_transport::Connection::for_simulation(Bytes(bytes.clone()),1);send_ticket(&t,&mut connection,||f.time.load(Ordering::SeqCst)).unwrap();assert_eq!(bytes.lock().unwrap().as_slice(),&[0xae,0x6a,6,0xf2,0xee]);f.actor.finish_write(t.command.id,true).await.unwrap();
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

struct QueryDelayWire {
    time:&'static AtomicU64,
    frames:Arc<Mutex<Vec<Vec<u8>>>>,
}
impl Read for QueryDelayWire {fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
impl Write for QueryDelayWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        self.frames.lock().unwrap().push(bytes.to_vec());
        if bytes.len()==5 && (0xa0..=0xa8).contains(&bytes[2]) {self.time.fetch_add(180,Ordering::SeqCst);}
        Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
#[tokio::test]
async fn polling_cannot_spend_an_admitted_away_cohorts_original_deadline() {
    let f=F::new(true,false,|_|{}).await;
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();assert_eq!(ids.len(),13);
    let frames=Arc::new(Mutex::new(vec![]));
    let wire=|name:&str| (name.to_owned(),hb_transport::Connection::for_simulation(QueryDelayWire {time:f.time,frames:frames.clone()},1));
    let mut owner=crate::transport_owner::TransportOwner::new(f.actor.clone(),[wire("a"),wire("b")].into());
    let (module,channel)={let (device,module)=f.config.binding(&DeviceId::new("output-0:1").unwrap()).unwrap();(module.clone(),device.channel)};
    let mut control=hb_transport::Connection::for_simulation(QueryDelayWire {time:f.time,frames:frames.clone()},1);
    let mut written=vec![];
    for _ in 0..12 {
        if owner.idle_for_refresh(f.controller,f.actor.task_generation().await.unwrap()).await {
            owner.query_status("a",&module,channel,ExecutionContext::Simulation,500).await.unwrap();
        }
        f.step();
        if let Some(ticket)=f.actor.dispatch().await.unwrap() {
            send_ticket(&ticket,&mut control,||f.time.load(Ordering::SeqCst)).unwrap();
            f.actor.finish_write(ticket.command.id,true).await.unwrap();written.push(ticket.command.id);
        }
    }
    assert!(f.actor.dispatch().await.unwrap().is_none(),"reset remains Accepted until real feedback");
    all_off_feedback(&f).await;f.step();let ticket=f.actor.dispatch().await.unwrap().unwrap();send_ticket(&ticket,&mut control,||f.time.load(Ordering::SeqCst)).unwrap();f.actor.finish_write(ticket.command.id,true).await.unwrap();written.push(ticket.command.id);
    let store=Store::open(&f.path).unwrap();
    let statuses=ids.iter().map(|id|store.command_status(f.actor.boot_id(),*id).unwrap()).collect::<Vec<_>>();
    assert_eq!(written,ids,"queued status reads must not consume the shared2s lease: {statuses:?}");
    assert!(statuses.iter().all(|s|*s==Some(Status::Written)));
    let captured=frames.lock().unwrap().clone();assert_eq!(captured.len(),13);assert!(captured[..12].iter().all(|f|f[2]==0));assert_eq!(captured[12],vec![0xae,0x6a,6,0xf2,0xee]);drop(captured);
    assert!(owner.idle_for_refresh(f.controller,f.actor.task_generation().await.unwrap()).await,"polling resumes only after the accepted cohort and its tail drain");
    drop(store);f.close().await;
}

type TimedFrames=Arc<Mutex<Vec<(u64,Vec<u8>)>>>;
struct LoopDelayWire {
    time:&'static AtomicU64,
    frames:TimedFrames,
    replies:std::collections::VecDeque<u8>,
}
impl Read for LoopDelayWire {
    fn read(&mut self,out:&mut[u8])->std::io::Result<usize>{
        if !self.replies.is_empty(){self.time.fetch_add(21,Ordering::SeqCst);let n=out.len().min(self.replies.len());for byte in &mut out[..n]{*byte=self.replies.pop_front().unwrap();}return Ok(n);}
        self.time.fetch_add(220,Ordering::SeqCst);Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl Write for LoopDelayWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        let(frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());
        for frame in frames{self.frames.lock().unwrap().push((self.time.load(Ordering::SeqCst),frame.to_vec()));
            if frame[2]==0{for ch in 1..=8{self.replies.extend([0xae,frame[1],0xd0+ch,if frame[1]<0x40{0xf2}else{0},0xee]);}}
        }
        self.time.fetch_add(21,Ordering::SeqCst);Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
#[tokio::test]
async fn complete_away_and_green_tail_make_progress_before_idle_reads_spend_the_cohort_deadline() {
    let f=F::new(true,false,|_|{}).await;
    let mut runtime=crate::automation_runtime::AutomationRuntime::load(f.actor.clone(),f.controller,1_800_000_000_000,1000,64,Default::default()).await.unwrap();
    let frames=Arc::new(Mutex::new(vec![]));let wire=|name:&str|(name.to_owned(),hb_transport::Connection::for_simulation(LoopDelayWire {time:f.time,frames:frames.clone(),replies:Default::default()},1));
    let transport=crate::transport_owner::TransportOwner::new(f.actor.clone(),[wire("a"),wire("b")].into());
    let feedback=crate::feedback::Feedback::new(&f.config,ExecutionContext::Simulation,Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),1).unwrap();
    let mut owner=crate::observed_transport::ObservedTransport::new(transport,feedback);
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();let generation=f.actor.task_generation().await.unwrap();
    let (stop,receive)=tokio::sync::watch::channel(false);
    let drive=runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000));
    let monitor=async {
        loop {
            let statuses=f.actor.command_statuses(f.controller,generation.clone(),ids.clone()).await.unwrap();
            if statuses.iter().all(|s|matches!(s,Status::Written|Status::Cancelled|Status::Failed|Status::Uncertain)) {stop.send_replace(true);break statuses;}
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    };
    let (result,statuses)=tokio::time::timeout(std::time::Duration::from_secs(4),async {tokio::join!(drive,monitor)}).await.unwrap();result.unwrap();
    assert!(statuses.iter().all(|s|*s==Status::Written),"shared2s lease exhausted by drive read/status work: {statuses:?}");
    let captured=frames.lock().unwrap().clone();assert_eq!(captured.len(),13);assert!(captured[..12].iter().all(|(_,frame)|frame[2]==0));assert_eq!(captured[12].1,vec![0xae,0x6a,6,0xf2,0xee]);drop(captured);
    f.close().await;
}

#[tokio::test]
async fn away_accepted_after_idle_snapshot_but_before_wire_slot_defers_the_new_query() {
    let f=F::new(true,false,|_|{}).await;let frames=Arc::new(Mutex::new(vec![]));
    let mut owner=crate::transport_owner::TransportOwner::new(f.actor.clone(),[("a".into(),hb_transport::Connection::for_simulation(QueryDelayWire {time:f.time,frames:frames.clone()},1))].into());
    let (module,channel)={let (device,module)=f.config.binding(&DeviceId::new("output-0:1").unwrap()).unwrap();(module.clone(),device.channel)};
    assert!(owner.idle_for_refresh(f.controller,f.actor.task_generation().await.unwrap()).await);
    let slot=f.actor.write_slot.clone().lock_owned().await;
    let query=owner.try_query_status_scope("a",&module,channel,ExecutionContext::Simulation,500,false);tokio::pin!(query);
    assert!(matches!(futures_util::poll!(query.as_mut()),std::task::Poll::Pending),"query is still waiting for its physical slot");
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();drop(slot);
    assert!(!query.await.unwrap(),"atomic actor reservation must observe the later accepted controls");
    assert!(frames.lock().unwrap().is_empty());
    assert!(ids.iter().all(|id|Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),*id).unwrap()==Some(Status::Accepted)));
    f.close().await;
}
#[tokio::test]
async fn unfit_reserved_query_rejects_away_before_acceptance_and_releases_without_replay() {
    let f=F::new(true,false,|_|{}).await;let frames=Arc::new(Mutex::new(vec![]));
    let mut owner=crate::transport_owner::TransportOwner::new(f.actor.clone(),[("a".into(),hb_transport::Connection::for_simulation(QueryDelayWire {time:f.time,frames:frames.clone()},1))].into());
    let (module,channel)={let (device,module)=f.config.binding(&DeviceId::new("output-0:1").unwrap()).unwrap();(module.clone(),device.channel)};
    assert!(owner.try_query_status_scope("a",&module,channel,ExecutionContext::Simulation,5000,false).await.unwrap());
    assert!(matches!(f.actor.submit_away_off(1,f.lease(true),true).await,Err(hap::server::WriteStatus::ResourceBusy)),"not accepted, not silently expired");
    let count:i64=rusqlite::Connection::open(&f.path).unwrap().query_row("SELECT count(*) FROM command_journal",[],|row|row.get(0)).unwrap();assert_eq!(count,0);
    owner.finish_status_read("a");
    let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();assert_eq!(ids.len(),13);
    assert_eq!(frames.lock().unwrap().len(),1,"only the original query, no automatic AllOff or retry");
    f.close().await;
}

#[derive(Default)]
struct HomeReadState {queries:usize,address:u8,channel:u8,reply_sent:bool,frames:Vec<(u64,Vec<u8>)>,query_gateway:String,reads:Vec<(u64,String,bool)>}
struct HomeReadWire {time:&'static AtomicU64,state:Arc<Mutex<HomeReadState>>,gateway:String}
impl Write for HomeReadWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        let mut state=self.state.lock().unwrap();let(frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());for frame in frames{state.frames.push((self.time.load(Ordering::SeqCst),frame.to_vec()));}
        if bytes.len()==5 && (0xa1..=0xa8).contains(&bytes[2]) {state.queries+=1;state.address=bytes[1];state.channel=bytes[2]-0xa0;state.query_gateway=self.gateway.clone();}
        self.time.fetch_add(21,Ordering::SeqCst);Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
impl Read for HomeReadWire {
    fn read(&mut self,bytes:&mut[u8])->std::io::Result<usize>{
        self.time.fetch_add(240,Ordering::SeqCst);let mut state=self.state.lock().unwrap();
        let reply=state.queries>=2 && !state.reply_sent;
        state.reads.push((self.time.load(Ordering::SeqCst),self.gateway.clone(),reply));
        if reply {state.reply_sent=true;bytes[..5].copy_from_slice(&[0xae,state.address,0xd0+state.channel,0xf2,0xee]);return Ok(5);}
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
#[tokio::test]
async fn three_led_home_after_an_existing_pending_read_keeps_its_original_command_deadlines() {
    let f=F::new_documents(true,false,|config,rules,scope|{
        for key in [1,3,5] {let id=format!("key{key}");scope.insert(id.clone());config["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({"number":key,"label":"Synthetic Home","led_endpoint":{"id":id,"name":"Synthetic Home LED","enabled":true}}));}
        rules["timed_scenes"]=json!([{"id":"home","enabled":true,"review_evidence":"synthetic3LED","max_lateness_ms":1000,"steps":([1,3,5].into_iter().map(|key|json!({"source":format!("LED{key}"),"offset_ms":0,"device":format!("key{key}"),"command":{"kind":"panel_led","on":false}})).collect::<Vec<_>>())}]);
    }).await;
    let state=Arc::new(Mutex::new(HomeReadState::default()));let wire=|name:&str|(name.to_owned(),hb_transport::Connection::for_simulation(HomeReadWire {time:f.time,state:state.clone(),gateway:name.into()},1));
    let transport=crate::transport_owner::TransportOwner::new(f.actor.clone(),[wire("a"),wire("b")].into());
    let feedback=crate::feedback::Feedback::new(&f.config,ExecutionContext::Simulation,Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),1).unwrap();
    let mut owner=crate::observed_transport::ObservedTransport::new(transport,feedback);
    owner.configure_output_refresh(&f.config,f.controller,f.actor.task_generation().await.unwrap(),ExecutionContext::Simulation,1000,5000).unwrap();f.time.fetch_add(45,Ordering::SeqCst);owner.refresh_outputs_if_due().await.unwrap();assert!(owner.output_query_pending());
    let mut runtime=crate::automation_runtime::AutomationRuntime::load(f.actor.clone(),f.controller,1_800_000_000_000,1000,64,Default::default()).await.unwrap();
    let accepted_at=f.time.load(Ordering::SeqCst);
    let ids=f.actor.submit_scene(f.actor.task_generation().await.unwrap(),homesense_hap::requests::Batch {controller:f.controller,intents:[1,3,5].into_iter().map(|key|homesense_hap::requests::Intent {device:DeviceId::new(format!("key{key}")).unwrap(),command:Command::PanelLed(false)}).collect(),subscriptions:vec![]},f.actor.manual_fence().await.unwrap()).await.unwrap();assert_eq!(ids.len(),3);
    let generation=f.actor.task_generation().await.unwrap();let (stop,receive)=tokio::sync::watch::channel(false);
    let drive=runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000));
    let monitor=async {loop {let statuses=f.actor.command_statuses(f.controller,generation.clone(),ids.clone()).await.unwrap();if statuses.iter().all(|s|matches!(s,Status::Written|Status::Cancelled|Status::Failed|Status::Uncertain)){stop.send_replace(true);break statuses;}tokio::time::sleep(std::time::Duration::from_millis(2)).await;}};
    let (result,statuses)=tokio::time::timeout(std::time::Duration::from_secs(4),async {tokio::join!(drive,monitor)}).await.unwrap();result.unwrap();let frames=state.lock().unwrap().frames.clone();assert!(statuses.iter().all(|s|*s==Status::Written),"existing read may wait only within the original shared command lease: {statuses:?}; accepted_at={accepted_at}; frames={frames:?}; read_routes={:?}",state.lock().unwrap().reads);
    let leds=frames.iter().filter(|(_,bytes)|bytes[1]==0x6a).collect::<Vec<_>>();assert_eq!(leds.len(),3);assert!(leds.iter().all(|(at,_)|*at<accepted_at+2000));assert_eq!(leds.iter().map(|(_,bytes)|bytes[2]).collect::<Vec<_>>(),[1,3,5]);
    f.close().await;
}

#[tokio::test]
async fn three_led_home_rejects_an_unfit_existing_read_before_any_command_is_accepted() {
    let f=F::new_documents(true,false,|config,_,scope|{
        for key in [1,3,5] {let id=format!("key{key}");scope.insert(id.clone());config["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({"number":key,"label":"Synthetic Home","led_endpoint":{"id":id,"name":"Synthetic Home LED","enabled":true}}));}
    }).await;
    let frames=Arc::new(Mutex::new(vec![]));let mut owner=crate::transport_owner::TransportOwner::new(f.actor.clone(),[("a".into(),hb_transport::Connection::for_simulation(QueryDelayWire {time:f.time,frames:frames.clone()},1))].into());
    let(_,module)=f.config.module("output-0").unwrap();
    owner.query_status("a",module,homesense_core::Channel::new(1).unwrap(),ExecutionContext::Simulation,5000).await.unwrap();
    let generation=f.actor.task_generation().await.unwrap();let fence=f.actor.manual_fence().await.unwrap();
    let batch=||homesense_hap::requests::Batch {controller:f.controller,intents:[1,3,5].into_iter().map(|key|homesense_hap::requests::Intent {device:DeviceId::new(format!("key{key}")).unwrap(),command:Command::PanelLed(false)}).collect(),subscriptions:vec![]};
    assert_eq!(f.actor.submit_scene(generation.clone(),batch(),fence).await.unwrap_err(),hap::server::WriteStatus::ResourceBusy);
    let count=rusqlite::Connection::open(&f.path).unwrap().query_row("SELECT count(*) FROM command_journal",[],|row|row.get::<_,i64>(0)).unwrap();assert_eq!(count,0);
    assert_eq!(frames.lock().unwrap().as_slice(),&[vec![0xae,0x20,0xa1,0xf2,0xee]],"Busy must not transmit a scene subset or retry");
    owner.finish_status_read("a");
    let ids=f.actor.submit_scene(generation,batch(),fence).await.unwrap();assert_eq!(ids.len(),3,"only a new explicit independent submission is accepted after the read retires");
    f.close().await;
}

struct InterruptWire {
    time:&'static AtomicU64,frames:Arc<Mutex<Vec<Vec<u8>>>>,
    interrupt:Option<Box<dyn FnOnce()+Send>>,
}
impl Read for InterruptWire {
    fn read(&mut self,_:&mut[u8])->std::io::Result<usize>{self.time.fetch_add(21,Ordering::SeqCst);Err(std::io::ErrorKind::WouldBlock.into())}
}
impl Write for InterruptWire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        let(frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());self.frames.lock().unwrap().extend(frames.iter().map(|f|f.to_vec()));self.time.fetch_add(21,Ordering::SeqCst);
        if let Some(interrupt)=self.interrupt.take(){interrupt();}Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
#[tokio::test]
async fn bounded_away_quantum_observes_stop_and_source_revocation_between_physical_writes() {
    for revoke in [false,true] {
        let f=F::new(true,true,|_|{}).await;
        let frames=Arc::new(Mutex::new(vec![]));let (stop,receive)=tokio::sync::watch::channel(false);
        let stop_on_write=stop.clone();let tokens=f.tokens.clone();let credential=f.credential;
        let revoked_at=Arc::new(AtomicU64::new(u64::MAX));let fence=revoked_at.clone();let captured=frames.clone();
        let wire=InterruptWire {time:f.time,frames:frames.clone(),interrupt:Some(Box::new(move||{
            if revoke {
                // Mutation serializes with the source's in-progress write. It
                // must run on another thread rather than reenter that lock.
                std::thread::spawn(move||{tokens.revoke(credential).unwrap();fence.store(captured.lock().unwrap().len() as u64,Ordering::Release);});
            }else{stop_on_write.send_replace(true);}
        }))};
        let transport=crate::transport_owner::TransportOwner::new(f.actor.clone(),[("a".into(),hb_transport::Connection::for_simulation(wire,1))].into());
        let feedback=crate::feedback::Feedback::new(&f.config,ExecutionContext::Simulation,Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),1).unwrap();let mut owner=crate::observed_transport::ObservedTransport::new(transport,feedback);
        let mut runtime=crate::automation_runtime::AutomationRuntime::load(f.actor.clone(),f.controller,1_800_000_000_000,1000,64,Default::default()).await.unwrap();
        let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();
        let drive=runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000));
        let monitor=async {if revoke {loop {
            let store=Store::open(&f.path).unwrap();
            if ids.iter().all(|id|store.command_status(f.actor.boot_id(),*id).unwrap().is_some_and(|s|matches!(s,Status::Written|Status::Cancelled|Status::Failed|Status::Uncertain))){stop.send_replace(true);break;}
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }}};
        let (result,())=tokio::time::timeout(std::time::Duration::from_secs(4),async {tokio::join!(drive,monitor)}).await.unwrap();result.unwrap();
        let count=frames.lock().unwrap().len();
        if revoke {assert!(count<13);assert_eq!(count as u64,revoked_at.load(Ordering::Acquire),"no subsequent physical write after exact source revocation linearizes");}
        else {assert_eq!(count,12,"STOP observes the boundary of the admitted12-frame batch; reset remains unsent");}
        let store=Store::open(&f.path).unwrap();assert_eq!(store.command_status(f.actor.boot_id(),ids[0]).unwrap(),Some(Status::Written));
        assert!(ids[count..].iter().all(|id|store.command_status(f.actor.boot_id(),*id).unwrap()!=Some(Status::Written)));drop(store);
        f.close().await;
    }
}

#[tokio::test]
async fn away_twelve_modules_share_one_sixty_byte_write_and_green_requires_actual_off_feedback(){
    let f=F::new(true,true,|_|{}).await;let ids=f.actor.submit_away_off(1,f.lease(true),true).await.unwrap();
    let bytes=Arc::new(Mutex::new(vec![]));let mut owner=crate::transport_owner::TransportOwner::new(f.actor.clone(),[("a".into(),hb_transport::Connection::for_simulation(Bytes(bytes.clone()),1))].into());
    let first=tokio::time::timeout(std::time::Duration::from_secs(2),owner.step()).await.unwrap().unwrap().unwrap();first.result.unwrap();assert_eq!(first.completed.len(),12);assert_eq!(first.physical_frames,12);assert_eq!(first.refresh_devices.len(),96);
    assert_eq!(bytes.lock().unwrap().len(),60);assert!(bytes.lock().unwrap().as_chunks::<5>().0.iter().all(|frame|frame[2]==0));
    assert!(owner.step().await.unwrap().is_none(),"all Written cannot stand in for actual Off feedback");assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[12]).unwrap(),Some(Status::Accepted));
    all_off_feedback(&f).await;owner.step().await.unwrap().unwrap().result.unwrap();assert_eq!(&bytes.lock().unwrap()[60..],&[0xae,0x6a,6,0xf2,0xee]);
    let store=Store::open(&f.path).unwrap();assert!(ids.iter().all(|id|store.command_status(f.actor.boot_id(),*id).unwrap()==Some(Status::Written)));drop(store);f.close().await;
}
