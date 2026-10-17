//! Generated 28-panel coverage over a real loopback TCP connection. This proves
//! the configured raw-event execution path, not household wiring or source parity.
use crate::{admission::{Admission,Options},automation_control::{channel,Control,Receiver},
    configured_rules::ConfiguredRules,feedback::Feedback,observed_transport::ObservedTransport,
    panel_rules::PanelRules,scene_scheduler::SceneScheduler,transport_owner::TransportOwner};
use homesense_config::rule_document::Document;
use homesense_core::{DeviceId,ExecutionContext,rule::Budget};
use serde_json::json;
use std::{io::{Read,Write},net::{TcpListener,TcpStream},sync::atomic::{AtomicU64,Ordering},time::Duration};
use uuid::Uuid;

struct Fixture {
    directory:std::path::PathBuf,actor:Admission,controller:Uuid,document:Document,
    panels:PanelRules,receiver:Receiver,scenes:SceneScheduler,router:crate::deferred_router::Router,owner:ObservedTransport<TcpStream>,
    peer:TcpStream,time:&'static AtomicU64,_control:Control,
}
impl Fixture {
    async fn new(execute:bool)->Self {
        Self::build(execute,false,true).await
    }
    async fn build(execute:bool,motor:bool,bind:bool)->Self {
        let _=env_logger::Builder::new().filter_level(log::LevelFilter::Warn).is_test(true).try_init();
        let mut modules=vec![json!({"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}})];
        let mut panels=vec![];let mut scenes=vec![];
        for p in 0..28u8 {
            modules.push(json!({"id":format!("module-{p}"),"address":format!("{:02X}",0x40+p),
                "profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
            let mut keys=vec![];
            for key in 1..=6u8 {
                let scene=format!("scene-{p}-{key}");let endpoint=format!("key-{p}-{key}");let on=key%2==1;
                keys.push(json!({"number":key,"label":key.to_string(),"led_endpoint":{"id":endpoint,"name":"Synthetic key","enabled":true},
                    "action":{"kind":"scene_execute","scene":scene,"review_evidence":"Generated exact native key action"}}));
                scenes.push(json!({"id":scene,"enabled":true,"review_evidence":"Generated two-command scene",
                    "activate":[{"source":"synthetic load","action":{"kind":"command","device":"load","command":{"kind":"switch","on":on}}},
                        {"source":"synthetic own LED","action":{"kind":"command","device":endpoint,"command":{"kind":"panel_led","on":on}}}]}));
            }
            let mut panel=json!({"id":format!("panel-{p}"),"name":"Synthetic panel","module_id":format!("module-{p}"),"enabled":true,"keys":keys});
            if execute {panel["event_control"]=json!({"owner":"homesense","legacy_writer_isolated":true,
                "review_evidence":"Generated loopback only: no legacy controller or physical bus connected"});}
            panels.push(panel);
        }
        let mut devices=vec![json!({"id":"load","name":"Synthetic load","module_id":"relay","channel":1,"enabled":true})];
        if motor {
            scenes.retain(|scene|!matches!(scene["id"].as_str(),Some("scene-0-1"|"scene-0-2"|"scene-0-3")));
            modules.push(json!({"id":"motor-module","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            devices.push(json!({"id":"curtain","name":"Synthetic motor","module_id":"motor-module","channel":1,"enabled":true,
                "motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"Synthetic loopback motor","verified_at_unix_ms":1},
                "curtain_state":{"open_travel_ms":10000,"close_travel_ms":10000,"stop_led_idle_ms":1,
                    "evidence":"Synthetic shared trajectory","reviewed_at_unix_ms":1,
                    "leds":{"open":["key-0-1"],"stop":["key-0-2"],"close":["key-0-3"]}}}));
            for (index,action) in [(0,"open"),(1,"stop"),(2,"close")] {
                panels[0]["keys"][index]["action"]=json!({"kind":"motor","device":"curtain","action":action,
                    "press_behavior":if action=="stop"{"absolute"}else{"repeat_to_stop"},"review_evidence":"Synthetic raw-event motor mapping"});
            }
        }
        let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,
            "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],
            "devices":devices,"keypads":panels})).unwrap();
        let rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":scenes})).unwrap();
        let parsed=homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();
        Document::parse(&rules,&parsed).unwrap();
        let directory=std::env::temp_dir().join(format!("native-panels-tcp-{}",Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
        let bundle=homesense_storage::Store::open(directory.join("runtime.sqlite")).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
        let document=bundle.automation.unwrap();let controller=Uuid::new_v4();let time=Box::leak(Box::new(AtomicU64::new(100)));
        let actor=Admission::open_with_test_time(directory.join("runtime.sqlite"),Options {
            context:ExecutionContext::Simulation,write_interval:Duration::from_millis(20),..Default::default()
        },move|id|id==controller,time).await.unwrap();actor.synchronized(1,1).await.unwrap();
        let configured=ConfiguredRules::load_document(actor.clone(),controller,&document,Budget::default()).await.unwrap();
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let socket=TcpStream::connect(listener.local_addr().unwrap()).unwrap();let (peer,address)=listener.accept().unwrap();assert!(address.ip().is_loopback());drop(listener);
        socket.set_nonblocking(true).unwrap();socket.set_nodelay(true).unwrap();peer.set_nodelay(true).unwrap();peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let feedback=Feedback::new(&bundle.configuration,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
        let transport=TransportOwner::new(actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(socket,1))].into());
        let owner=ObservedTransport::new(transport,feedback);let mut panels=configured.panels;
        if bind {panels.bind_feedback(owner.feedback()).unwrap();}
        let (control,receiver)=channel(controller,actor.task_generation().await.unwrap());
        let scenes=SceneScheduler::new(actor.clone(),controller,16,actor.monotonic_ms()).unwrap();
        let router=crate::deferred_router::Router::new(actor.clone(),controller,&scenes,16).unwrap();
        Self {directory,actor,controller,document,panels,receiver,scenes,router,owner,peer,time,_control:control}
    }
    async fn input(&mut self,bytes:&[u8]) {
        self.time.fetch_add(100,Ordering::SeqCst);self.peer.write_all(bytes).unwrap();
        for _ in 0..100 {
            let read=self.owner.receive("bus").await.unwrap();
            if matches!(read.result,hb_transport::Receive::Frames{ref frames,..} if !frames.is_empty()){return;}
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("loopback input never reached the decoder");
    }
    async fn route(&mut self)->usize {
        self.collect().await;
        let events=self.panels.take_native_dispatch();let count=events.len();self.receiver.enqueue_native(events).unwrap();
        self.receiver.poll_causal(&self.actor,&self.document,&mut self.scenes,self.owner.feedback(),5000,None,Some(&mut self.router)).await;
        self.router.poll(&mut self.scenes,Some((self.owner.feedback(),5000))).await.unwrap();
        count
    }
    async fn collect(&mut self) {
        self.panels.consume_feedback(self.owner.feedback(),self.actor.monotonic_ms(),5000).await.unwrap();
    }
    async fn click(&mut self,p:u8,key:u8) {
        self.input(&[0xae,0x40+p,0xd0+key,0xf1,0xee,0xae,0x40+p,0xd0+key,0xf2,0xee]).await;
    }
    async fn flush_scene(&mut self) {
        for _ in 0..2 {
            self.time.fetch_add(21,Ordering::SeqCst);
            self.owner.step().await.unwrap().expect("source cue and load must reach writer").result.unwrap();
        }
        let mut bytes=[0;10];self.peer.read_exact(&mut bytes).unwrap();
    }
    fn journal_count(&self)->i64 {
        let db=rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap();
        db.query_row("SELECT count(*) FROM command_journal",[],|r|r.get(0)).unwrap()
    }
    async fn finish(mut self) {
        self.panels.disconnect();self.receiver.close();self.router.close(&mut self.scenes).await.unwrap();self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}

#[tokio::test]
async fn all_28_by_6_raw_tcp_keys_execute_once_with_own_cue_before_load() {
    let mut f=Fixture::new(true).await;
    let diagnostic=f.actor.diagnostics(f.controller,f.actor.task_generation().await.unwrap()).await.unwrap();
    assert_eq!((diagnostic.native_events.configured_keypads,diagnostic.native_events.configured_endpoints),(28,168));assert!(diagnostic.native_events.owner_active);
    let mut previous_operation=0u64;
    for p in 0..28u8 {for key in 1..=6u8 {
        let down=[0xae,0x40+p,0xd0+key,0xf1,0xee];let up=[0xae,0x40+p,0xd0+key,0xf2,0xee];
        // Duplicate physical down/release packets are real decoded input, not
        // duplicate API calls. Exactly one release gesture may execute.
        f.input(&[down,down,up,up].concat()).await;
        assert_eq!(f.route().await,1,"panel {p} key {key}");
        for _ in 0..2 {
            f.time.fetch_add(21,Ordering::SeqCst);
            let outcome=f.owner.step().await.unwrap().expect("source cue and load must reach writer");outcome.result.unwrap();
        }
        let on=key%2==1;let mut wire=[0u8;10];f.peer.read_exact(&mut wire).unwrap();
        assert_eq!(wire,[[0xae,0x40+p,key,if on{0xf1}else{0xf2},0xee],[0xae,3,1,if on{0xf1}else{0xf2},0xee]].concat().as_slice());
        let endpoint=DeviceId::new(format!("key-{p}-{key}")).unwrap();
        let cues=f.actor.keypad_indications(f.controller,f.actor.task_generation().await.unwrap(),vec![endpoint.clone()]).await.unwrap();let cue=&cues[&endpoint];
        assert_eq!(cue.source,"button_event");assert_eq!(cue.status,"written");assert!(!cue.observed);
        let event=cue.event_sequence.as_ref().unwrap().parse::<u64>().unwrap();assert!(event>0);
        assert_eq!(cue.request_id,format!("wall-{:032x}-{event}",Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128()));
        let sequence=cue.operation_sequence.parse::<u64>().unwrap();assert!(sequence>previous_operation);previous_operation=sequence;
        assert_eq!(cue.color,Some(if on{"red"}else{"green"}));
        assert_eq!(f.journal_count(),i64::from(p)*12+i64::from(key)*2);
        // The outgoing LED syntax echoed by a gateway is not a button event.
        f.input(&wire[..5]).await;assert_eq!(f.route().await,0);
        f.time.fetch_add(21,Ordering::SeqCst);assert!(f.owner.step().await.unwrap().is_none());
    }}
    assert_eq!(f.journal_count(),336);f.finish().await;
}

#[tokio::test]
async fn all_28_by_6_without_event_owner_only_observe_tcp_frames() {
    let mut f=Fixture::new(false).await;
    for p in 0..28u8 {for key in 1..=6u8 {
        f.input(&[0xae,0x40+p,0xd0+key,0xf1,0xee,0xae,0x40+p,0xd0+key,0xf2,0xee]).await;
        assert_eq!(f.route().await,0);
    }}
    let diagnostic=f.actor.diagnostics(f.controller,f.actor.task_generation().await.unwrap()).await.unwrap();
    assert_eq!(diagnostic.native_events.configured_endpoints,0);assert!(!diagnostic.native_events.owner_active);
    assert_eq!(f.journal_count(),0);assert!(f.owner.step().await.unwrap().is_none());f.finish().await;
}

#[tokio::test]
async fn long_release_executes_once_without_restarting_from_long_end() {
    let mut f=Fixture::new(true).await;
    // The archived programs execute on Release, including an ordinary long
    // press. LongEnd must not create a second toggle or motor movement.
    f.input(&[0xae,0x40,0xd1,0xf1,0xee]).await;assert_eq!(f.route().await,0);
    f.time.fetch_add(700,Ordering::SeqCst);assert_eq!(f.route().await,0);
    assert_eq!(f.journal_count(),0);
    f.input(&[0xae,0x40,0xd1,0xf2,0xee,0xae,0x40,0xd1,0xf2,0xee]).await;
    assert_eq!(f.route().await,1);f.flush_scene().await;
    assert_eq!(f.route().await,0);assert_eq!(f.journal_count(),2);f.finish().await;
}

#[tokio::test]
async fn release_without_down_and_cancelled_overlong_hold_do_not_execute() {
    let mut f=Fixture::new(true).await;
    f.input(&[0xae,0x40,0xd1,0xf2,0xee]).await;assert_eq!(f.route().await,0);
    f.input(&[0xae,0x40,0xd1,0xf1,0xee]).await;assert_eq!(f.route().await,0);
    f.time.fetch_add(10000,Ordering::SeqCst);assert_eq!(f.route().await,0);
    f.input(&[0xae,0x40,0xd1,0xf2,0xee]).await;assert_eq!(f.route().await,0);
    assert_eq!(f.journal_count(),0);f.finish().await;
}

#[tokio::test]
async fn held_snapshot_at_reconnect_does_not_replay_release() {
    let mut f=Fixture::build(true,false,false).await;
    f.input(&[0xae,0x40,0xd1,0xf1,0xee]).await;
    f.panels.bind_feedback(f.owner.feedback()).unwrap();
    f.input(&[0xae,0x40,0xd1,0xf2,0xee]).await;assert_eq!(f.route().await,0);
    assert_eq!(f.journal_count(),0);
    f.click(0,1).await;assert_eq!(f.route().await,1);f.flush_scene().await;
    assert_eq!(f.journal_count(),2);f.finish().await;
}

#[tokio::test]
async fn queued_wall_event_is_rejected_after_owner_disconnect() {
    let mut f=Fixture::new(true).await;f.click(0,1).await;f.collect().await;
    let pending=f.panels.take_native_dispatch();assert_eq!(pending.len(),1);
    f.receiver.enqueue_native(pending).unwrap();f.panels.disconnect();
    f.receiver.poll_causal(&f.actor,&f.document,&mut f.scenes,f.owner.feedback(),5000,None,Some(&mut f.router)).await;
    assert!(f.owner.step().await.unwrap().is_none());assert_eq!(f.journal_count(),0);
    let status=f.actor.diagnostics(f.controller,f.actor.task_generation().await.unwrap()).await.unwrap();
    assert!(!status.native_events.owner_active);f.finish().await;
}

#[tokio::test]
async fn queued_wall_event_expires_without_replay_or_owner_exit() {
    let mut f=Fixture::new(true).await;f.click(0,1).await;f.collect().await;
    let pending=f.panels.take_native_dispatch();assert_eq!(pending.len(),1);
    f.receiver.enqueue_native(pending).unwrap();f.time.fetch_add(5000,Ordering::SeqCst);
    f.receiver.poll_causal(&f.actor,&f.document,&mut f.scenes,f.owner.feedback(),5000,None,Some(&mut f.router)).await;
    assert!(f.owner.step().await.unwrap().is_none());assert_eq!(f.journal_count(),0);
    let status=f.actor.diagnostics(f.controller,f.actor.task_generation().await.unwrap()).await.unwrap();
    assert!(status.native_events.owner_active);assert_eq!(f.route().await,0);f.finish().await;
}

#[tokio::test]
async fn raw_stop_survives_full_upstream_ordinary_dispatch_and_reaches_wire_first() {
    let mut f=Fixture::build(true,true,true).await;
    for _ in 0..257 {f.click(1,1).await;f.collect().await;}
    f.click(0,2).await;f.collect().await;
    let pending=f.panels.take_native_dispatch();
    assert_eq!(pending.iter().filter(|p|p.priority==2).count(),256);
    assert_eq!(pending.iter().filter(|p|p.priority==0).count(),1,"upstream overload must preserve STOP reserve");
    f.receiver.enqueue_native(pending).unwrap();
    // An unrelated real key is still held: query protection must not become a
    // global writer lock or consume STOP's reserved admission capacity.
    f.input(&[0xae,0x41,0xd1,0xf1,0xee]).await;
    assert!(f.owner.feedback().query_deferred("bus",f.actor.monotonic_ms(),true));
    f.receiver.poll_causal(&f.actor,&f.document,&mut f.scenes,f.owner.feedback(),5000,None,Some(&mut f.router)).await;
    f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().expect("STOP must reach writer").result.unwrap();
    let mut first=[0;5];f.peer.read_exact(&mut first).unwrap();
    assert_eq!(first,[0xae,0x22,1,0xf0,0xee],"STOP precedes source indication and queued ordinary loads");
    let status=f.actor.diagnostics(f.controller,f.actor.task_generation().await.unwrap()).await.unwrap();
    assert!(status.native_events.owner_active);f.finish().await;
}

#[tokio::test]
async fn central_reactor_executes_raw_key_without_manual_routing_or_http() {
    let mut f=Fixture::build(true,false,false).await;
    let mut peer=f.peer.try_clone().unwrap();peer.set_nonblocking(true).unwrap();
    let (stop,receive)=tokio::sync::watch::channel(false);
    let time=f.time;
    let drive=crate::automation_drive::drive_controlled(&f.actor,&mut f.owner,&mut f.panels,&mut f.scenes,None,
        Duration::from_millis(1),5000,receive,||Ok(0),Some((&mut f.receiver,&f.document)),Some(&mut f.router));
    let client=async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        peer.write_all(&[0xae,0x40,0xd1,0xf1,0xee,0xae,0x40,0xd1,0xf2,0xee]).unwrap();
        let deadline=tokio::time::Instant::now()+Duration::from_secs(3);let mut wire=vec![];
        while wire.len()<10 && tokio::time::Instant::now()<deadline {
            time.fetch_add(5,Ordering::SeqCst);
            let mut bytes=[0;32];
            match peer.read(&mut bytes) {
                Ok(n)=>wire.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{},
                Err(error)=>panic!("loopback peer read: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        stop.send(true).unwrap();
        // Keep the peer alive until drive observes STOP, rather than allowing
        // EOF to turn this normal shutdown into a gateway-disconnect test.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(wire,[0xae,0x40,1,0xf1,0xee,0xae,3,1,0xf1,0xee]);
    };
    let (result,())=tokio::join!(drive,client);assert!(!result.unwrap());
    assert_eq!(f.journal_count(),2);f.actor.shutdown().await.unwrap();std::fs::remove_dir_all(f.directory).unwrap();
}

#[tokio::test]
async fn one_live_native_owner_per_generation_and_retired_owner_can_be_replaced() {
    let mut f=Fixture::new(true).await;
    let second=ConfiguredRules::load_document(f.actor.clone(),f.controller,&f.document,Budget::default()).await;
    assert_eq!(second.err().expect("two live routers must never own the same raw keys"),"native owner registration: ResourceBusy");
    // A rejected registration must not retire the valid original owner.
    f.click(0,1).await;assert_eq!(f.route().await,1);f.flush_scene().await;
    assert_eq!(f.journal_count(),2);
    f.panels.disconnect();
    let replacement=ConfiguredRules::load_document(f.actor.clone(),f.controller,&f.document,Budget::default()).await.unwrap();
    f.panels=replacement.panels;f.panels.bind_feedback(f.owner.feedback()).unwrap();
    assert_eq!(f.route().await,0,"a replacement cannot replay the old release");
    f.click(0,2).await;assert_eq!(f.route().await,1);f.flush_scene().await;
    assert_eq!(f.journal_count(),4);f.finish().await;
}

#[tokio::test]
async fn stop_waits_for_inflight_query_reply_then_takes_the_next_bus_slot() {
    let mut f=Fixture::build(true,true,true).await;
    f.actor.module_query(f.controller,f.actor.task_generation().await.unwrap(),"relay".into(),
        crate::module_queries::Scope::Channel {channel:1},true).await.unwrap();
    f.owner.step().await.unwrap().unwrap().result.unwrap();
    let mut frame=[0;5];f.peer.read_exact(&mut frame).unwrap();assert_eq!(frame,[0xae,3,0xa1,0xf2,0xee]);
    f.click(0,2).await;assert_eq!(f.route().await,1);
    assert!(f.owner.step().await.unwrap().is_none(),"STOP cannot collide with a query's pending response");
    f.input(&[0xae,3,0xd1,0xf2,0xee]).await;
    f.time.fetch_add(21,Ordering::SeqCst);
    f.owner.step().await.unwrap().unwrap().result.unwrap();
    f.peer.read_exact(&mut frame).unwrap();assert_eq!(frame,[0xae,0x22,1,0xf0,0xee]);
    f.finish().await;
}
