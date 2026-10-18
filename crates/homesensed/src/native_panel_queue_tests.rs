//! Queue policy only. Generated capabilities do not claim physical-event proof.
use super::*;
use crate::{panel_rules::NativeDispatch, source_auth::Source, web_auth::{Grant, Role, Tokens}};
use homesense_core::ExecutionContext;
use std::sync::Arc;

struct Fixture {
    directory: std::path::PathBuf,
    actor: Admission,
    receiver: Receiver,
    controller: Uuid,
    tokens: Arc<Tokens>,
    _control: Control,
    stop:tokio::sync::watch::Sender<bool>,
}
impl Fixture {
    async fn new() -> Self {
        Self::build(false).await
    }
    async fn build(mapped_motor:bool) -> Self {
        let directory=std::env::temp_dir().join(format!("native-queue-{}",Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path=directory.join("runtime.sqlite");
        let mut config:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/devices.json")).unwrap();
        if mapped_motor {
            use serde_json::json;
            config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({
                "id":"motor-module","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            config["devices"].as_array_mut().unwrap().push(json!({"id":"curtain","name":"Synthetic queue motor",
                "module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{
                    "open_direction":"up","stop_verified":true,"evidence":"Synthetic scheduling scope","verified_at_unix_ms":1},
                "curtain_state":{"open_travel_ms":10000,"close_travel_ms":10000,"stop_led_idle_ms":1,
                    "evidence":"Synthetic shared trajectory and LED policy","reviewed_at_unix_ms":1,
                    "leds":{"open":["legacy-key-2"],"close":["legacy-key-3"],"stop":["legacy-key-4"]}}}));
            for (index,action) in [(1,"open"),(2,"close"),(3,"stop")] {
                config["keypads"][0]["keys"][index]["action"]=json!({"kind":"motor","device":"curtain",
                    "action":action,"press_behavior":if action=="stop"{"absolute"}else{"repeat_to_stop"},
                    "review_evidence":"Synthetic same-direction stop queue mapping"});
            }
        }
        let bundle=homesense_storage::Store::open(&path).unwrap().publish_automation(None,
            &serde_json::to_vec(&config).unwrap(),include_bytes!("../../../config/test/native-app-keypad/automation.json"),
            ExecutionContext::Simulation,&[]).unwrap();
        let actor=Admission::open(path,crate::admission::Options {
            context:ExecutionContext::Simulation,..Default::default()
        }, |_|true).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        let controller=Uuid::new_v4();
        let (control,mut receiver)=channel(controller,actor.task_generation().await.unwrap());
        let (stop,receive)=tokio::sync::watch::channel(false);
        if mapped_motor {receiver.bind_selection_reader(&actor,&bundle.automation.unwrap(),receive).await.unwrap();}
        let tokens=Arc::new(Tokens::default());
        tokens.insert(Uuid::new_v4(),"synthetic-native-queue-policy-0123456789",Grant {
            controller,role:Role::Member,devices:["fixture-light".into()].into(),scenes:Default::default(),
        }).unwrap();
        Self {directory,actor,receiver,controller,tokens,_control:control,stop}
    }
    fn event(&self,priority:u8)->NativeDispatch {
        let action=Action::DeviceCommand {controller:self.controller,revision:1,
            device:homesense_core::DeviceId::new("fixture-light").unwrap(),
            action:crate::management_devices::DeviceAction::Power {on:true}};
        let lease=self.tokens.module_off_lease(Some("Bearer synthetic-native-queue-policy-0123456789"),None,None).unwrap();
        NativeDispatch {source:Source::capture(lease,&action,None).unwrap(),action,priority,
            deadline:Instant::now()+Duration::from_secs(5)}
    }
    async fn finish(mut self) {
        self.receiver.close();self.actor.shutdown().await.unwrap();
        let db=rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0,
            "queue-only tests must not claim actuator execution");
        drop(db);std::fs::remove_dir_all(self.directory).unwrap();
    }
}

#[tokio::test]
async fn full_ordinary_queue_cannot_discard_stop_from_same_batch() {
    let mut f=Fixture::new().await;
    let ordinary=(0..256).map(|_|f.event(2)).collect();
    f.receiver.enqueue_native(ordinary).unwrap();
    let stop=f.event(0);let normal=f.event(2);
    f.receiver.enqueue_native(vec![stop,normal]).expect("ordinary overload must not terminate owner");
    assert_eq!(f.receiver.native[0].len(),1,"reserved STOP must survive mixed-batch overload");
    assert_eq!(f.receiver.native[2].len(),256,"ordinary queue remains bounded");
    f.finish().await;
}

#[tokio::test]
async fn per_priority_overload_keeps_existing_stops_and_other_capacity() {
    let mut f=Fixture::new().await;
    let stops=(0..64).map(|_|f.event(0)).collect();
    f.receiver.enqueue_native(stops).unwrap();
    let extra=f.event(0);let motor=f.event(1);let ordinary=f.event(2);
    f.receiver.enqueue_native(vec![extra,motor,ordinary]).unwrap();
    assert_eq!(f.receiver.native.each_ref().map(|q|q.len()),[64,1,1,0]);
    f.finish().await;
}

#[tokio::test]
async fn malformed_priority_rejects_batch_before_any_partial_enqueue() {
    let mut f=Fixture::new().await;
    let stop=f.event(0);let malformed=f.event(4);
    assert!(f.receiver.enqueue_native(vec![stop,malformed]).is_err());
    assert!(f.receiver.native.iter().all(|q|q.is_empty()));
    f.finish().await;
}

fn web_request(action:Action)->(Request,oneshot::Receiver<Result<Reply,String>>) {
    let(reply,receive)=oneshot::channel();
    (Request {action,source:None,watch:None,deadline:Instant::now()+Duration::from_secs(5),reply},receive)
}

#[tokio::test]
async fn explicit_app_stop_has_capacity_when_both_ordinary_queues_are_full() {
    let mut f=Fixture::new().await;
    let events=(0..256).map(|_|f.event(2)).collect();
    f.receiver.enqueue_native(events).unwrap();
    let mut waiting=vec![];
    for _ in 0..32 {
        let(request,reply)=web_request(Action::Diagnostics {controller:f.controller,revision:1});
        f._control.sender.try_send(request).unwrap();waiting.push(reply);
    }
    let mut stop=Box::pin(f._control.request(Action::DeviceCommand {
        controller:f.controller,revision:1,device:homesense_core::DeviceId::new("fixture-light").unwrap(),
        action:crate::management_devices::DeviceAction::Stop,
    }));
    assert!(tokio::time::timeout(Duration::from_millis(20),&mut stop).await.is_err(),
        "STOP must be enqueued in its reserve, not rejected by the full ordinary channel");
    let(request,native)=f.receiver.next_request().unwrap();
    assert!(native.is_none());
    assert!(matches!(request.action,Action::DeviceCommand{action:crate::management_devices::DeviceAction::Stop,..}));
    assert_eq!(f.receiver.receiver.len(),32);
    assert_eq!(f.receiver.native[2].len(),256);
    request.reply.send(Ok(Reply::CommandsAccepted(vec![]))).unwrap();
    assert!(matches!(tokio::time::timeout(Duration::from_millis(200),&mut stop).await.unwrap(),Ok(Reply::CommandsAccepted(_))));
    drop(stop);drop(waiting);f.finish().await;
}

#[tokio::test]
async fn native_and_http_ordinary_work_are_fair_but_stop_stays_first() {
    let mut f=Fixture::new().await;
    let events=(0..256).map(|_|f.event(2)).collect();
    f.receiver.enqueue_native(events).unwrap();
    let mut waiting=vec![];
    for _ in 0..32 {
        let(request,reply)=web_request(Action::Diagnostics {controller:f.controller,revision:1});
        f._control.sender.try_send(request).unwrap();waiting.push(reply);
    }
    let stop=f.event(0);f.receiver.enqueue_native(vec![stop]).unwrap();
    assert!(f.receiver.next_request().unwrap().1.is_some(),"native STOP wins over both ordinary queues");
    assert!(f.receiver.native[0].is_empty());
    let mut kinds=[0usize;2];
    for _ in 0..8 {
        let(_,native)=f.receiver.next_request().unwrap();kinds[usize::from(native.is_some())]+=1;
    }
    assert_eq!(kinds,[4,4],"neither wall activity nor HTTP reads may starve the other entrance");
    drop(waiting);f.finish().await;
}

#[tokio::test]
async fn closing_owner_rejects_pending_stop_and_drops_unadmitted_native_work() {
    let mut f=Fixture::new().await;
    let stop=f.event(0);f.receiver.enqueue_native(vec![stop]).unwrap();
    let mut request=Box::pin(f._control.request(Action::DeviceCommand {
        controller:f.controller,revision:1,device:homesense_core::DeviceId::new("fixture-light").unwrap(),
        action:crate::management_devices::DeviceAction::Stop,
    }));
    assert!(tokio::time::timeout(Duration::from_millis(20),&mut request).await.is_err());
    f.receiver.close();
    assert!(tokio::time::timeout(Duration::from_millis(200),&mut request).await.unwrap().is_err());
    assert!(f.receiver.next_request().is_none(),"old queued STOPs are not replayed after owner close");
    drop(request);f.finish().await;
}

fn keypad(controller:Uuid,key:u8,revision:&str)->Action {
    Action::KeypadAction {controller,request:crate::keypad_actions::Request {
        revision:revision.into(),keypad:"panel".into(),key,request_id:Uuid::new_v4().to_string(),
        gesture:crate::keypad_actions::Gesture::Activate,
    }}
}

#[tokio::test]
async fn only_published_same_revision_motor_keys_receive_reserved_capacity() {
    let f=Fixture::build(true).await;
    assert_eq!(f._control.request_lane(&keypad(f.controller,2,"1")),RequestLane::Motor);
    assert_eq!(f._control.request_lane(&keypad(f.controller,3,"1")),RequestLane::Motor);
    assert_eq!(f._control.request_lane(&keypad(f.controller,4,"1")),RequestLane::Stop);
    assert_eq!(f._control.request_lane(&keypad(f.controller,1,"1")),RequestLane::Ordinary);
    assert_eq!(f._control.request_lane(&keypad(f.controller,4,"2")),RequestLane::Ordinary);
    assert_eq!(f._control.request_lane(&keypad(f.controller,4,"01")),RequestLane::Ordinary);
    assert_eq!(f._control.request_lane(&keypad(f.controller,7,"1")),RequestLane::Ordinary);
    f.stop.send(true).unwrap();
    assert_eq!(f._control.request_lane(&keypad(f.controller,4,"1")),RequestLane::Ordinary,
        "a stopped publication must not lend its old motor mapping");
    f.finish().await;
}

#[tokio::test]
async fn unified_stop_and_same_direction_keys_bypass_full_ordinary_work() {
    let mut f=Fixture::build(true).await;
    let events=(0..256).map(|_|f.event(2)).collect();f.receiver.enqueue_native(events).unwrap();
    let mut waiting=vec![];
    for _ in 0..32 {
        let(request,reply)=web_request(Action::Diagnostics {controller:f.controller,revision:1});
        f._control.sender.try_send(request).unwrap();waiting.push(reply);
    }
    // A same-direction press is classified as potential STOP, not pre-resolved
    // from stale movement. It cannot consume the explicit STOP reserve.
    let mut direction=Box::pin(f._control.request(keypad(f.controller,2,"1")));
    assert!(tokio::time::timeout(Duration::from_millis(20),&mut direction).await.is_err());
    let mut stop=Box::pin(f._control.request(keypad(f.controller,4,"1")));
    assert!(tokio::time::timeout(Duration::from_millis(20),&mut stop).await.is_err());
    let(request,_)=f.receiver.next_request().unwrap();
    assert!(matches!(request.action,Action::KeypadAction{request:crate::keypad_actions::Request{key:4,..},..}));
    request.reply.send(Ok(Reply::CommandsAccepted(vec![]))).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(200),&mut stop).await.unwrap().is_ok());
    let(request,_)=f.receiver.next_request().unwrap();
    assert!(matches!(request.action,Action::KeypadAction{request:crate::keypad_actions::Request{key:2,..},..}));
    request.reply.send(Ok(Reply::CommandsAccepted(vec![]))).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(200),&mut direction).await.unwrap().is_ok());
    assert_eq!(f.receiver.receiver.len(),32);assert_eq!(f.receiver.native[2].len(),256);
    drop(stop);drop(direction);drop(waiting);f.finish().await;
}

#[tokio::test]
async fn away_reserve_survives_full_ordinary_queues_and_runs_after_only_safety_stop() {
    let mut f=Fixture::new().await;
    let ordinary=(0..256).map(|_|f.event(2)).collect();f.receiver.enqueue_native(ordinary).unwrap();
    let mut waiting=vec![];
    for _ in 0..32 {let(request,reply)=web_request(Action::Diagnostics{controller:f.controller,revision:1});f._control.sender.try_send(request).unwrap();waiting.push(reply);}
    let away=f.event(3);let motor=f.event(1);let stop=f.event(0);
    f.receiver.enqueue_native(vec![away,motor,stop]).unwrap();
    assert_eq!(f.receiver.native.each_ref().map(|q|q.len()),[1,1,256,1]);
    assert!(f.receiver.next_request().unwrap().1.is_some());assert!(f.receiver.native[0].is_empty());
    assert!(f.receiver.next_request().unwrap().1.is_some());assert!(f.receiver.native[3].is_empty());
    assert_eq!(f.receiver.native[1].len(),1,"Away precedes new movement and lighting work");
    assert_eq!(f.receiver.receiver.len(),32);assert_eq!(f.receiver.native[2].len(),256);
    drop(waiting);f.finish().await;
}

#[tokio::test]
async fn explicit_http_away_has_reserved_capacity_and_fair_service_with_native_away() {
    let mut f=Fixture::new().await;
    let ordinary=(0..256).map(|_|f.event(2)).collect();f.receiver.enqueue_native(ordinary).unwrap();
    let mut waiting=vec![];
    for _ in 0..32 {let(request,reply)=web_request(Action::Diagnostics{controller:f.controller,revision:1});f._control.sender.try_send(request).unwrap();waiting.push(reply);}
    let lease=f.tokens.module_off_lease(Some("Bearer synthetic-native-queue-policy-0123456789"),None,None).unwrap();
    let action=Action::AwayOff{controller:f.controller,revision:1,lease:lease.into(),submit:true};
    assert_eq!(f._control.request_lane(&action),RequestLane::Away);
    let mut pending=Box::pin(f._control.request(action));
    assert!(tokio::time::timeout(Duration::from_millis(20),&mut pending).await.is_err(),"Away is admitted to a separate channel despite saturated ordinary work");
    let native=f.event(3);f.receiver.enqueue_native(vec![native]).unwrap();
    let(request,native)=f.receiver.next_request().unwrap();assert!(native.is_none());assert!(matches!(request.action,Action::AwayOff{..}));
    request.reply.send(Ok(Reply::CommandsAccepted(vec![]))).unwrap();
    assert!(matches!(pending.await,Ok(Reply::CommandsAccepted(_))));
    assert!(f.receiver.next_request().unwrap().1.is_some(),"native Away receives the next fair high-priority slot");
    assert_eq!(f.receiver.receiver.len(),32);assert_eq!(f.receiver.native[2].len(),256);
    drop(waiting);f.finish().await;
}
