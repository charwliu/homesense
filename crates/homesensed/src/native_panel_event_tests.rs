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
        Self::build_with_away(execute,motor,bind,false).await
    }
    async fn build_with_away(execute:bool,motor:bool,bind:bool,away:bool)->Self {
        Self::build_with_away_supply(execute,motor,bind,away,false).await
    }
    async fn build_with_away_supply(execute:bool,motor:bool,bind:bool,away:bool,combined:bool)->Self {
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
            modules.push(json!({"id":"motor-module","address":if away{"7E"}else{"22"},"profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
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
        if away {
            scenes.retain(|scene|scene["id"]!="scene-0-6");
            panels[0]["keys"][5]["action"]=json!({"kind":"away_off","indicator_reset":"green",
                "review_evidence":"Synthetic native Away key with twelve reviewed modules"});
            for n in 0..12u8 {
                modules.push(json!({"id":format!("away-{n}"),"address":format!("{:02X}",0x20+n),
                    "profile":if n<7{"r8i8_din_pro_v2_7_7"}else{"ad8_din_rs485_v2_2_1"},"verification":{"status":"simulated"}}));
                for channel in 1..=8 {
                    let mut device=json!({"id":format!("away-{n}:{channel}"),"name":"Synthetic Away output",
                        "module_id":format!("away-{n}"),"channel":channel,"enabled":true});
                    if combined && n==7 && channel==1 {
                        device["supply"]=json!({"module_id":"away-0","channel":1});
                        device["default_on_level"]=json!(100);
                    }
                    devices.push(device);
                }
            }
        }
        let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,
            "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],
            "devices":devices,"keypads":panels})).unwrap();
        let mut rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":scenes});
        if away {
            rules["away_off"]=json!({"enabled":true,"review_evidence":"Synthetic complete lighting-only scope",
                "active_execution_policy":"cancel_affected_lighting","modules":(0..12).map(|n|json!({
                    "module_id":format!("away-{n}"),"devices":(1..=8).map(|channel|format!("away-{n}:{channel}")).collect::<Vec<_>>(),
                    "review_evidence":"Synthetic reviewed eight outputs"})).collect::<Vec<_>>()});
        }
        let rules=serde_json::to_vec(&rules).unwrap();
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
        self.time.fetch_add(21,Ordering::SeqCst);
        let written=self.owner.step().await.unwrap().expect("source cue and load must share one write");
        written.result.unwrap();assert_eq!(written.physical_frames,2);
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
async fn native_away_keeps_raw_event_authority_and_serializes_twelve_modules() {
    for disconnect in [false,true] {
        let mut f=Fixture::build_with_away(true,false,true,true).await;
        f.click(0,6).await;f.collect().await;
        let pending=f.panels.take_native_dispatch();assert_eq!(pending.len(),1);assert_eq!(pending[0].priority,3,"Away must use its dedicated native reserve");
        f.receiver.enqueue_native(pending).unwrap();
        if disconnect {f.panels.disconnect();}
        f.receiver.poll_causal(&f.actor,&f.document,&mut f.scenes,f.owner.feedback(),5000,None,Some(&mut f.router)).await;
        assert_eq!(f.journal_count(),0,"RX turnaround has not elapsed");
        if disconnect {
            f.time.fetch_add(21,Ordering::SeqCst);tick_away(&mut f).await;
            assert!(f.owner.step().await.unwrap().is_none());assert_eq!(f.journal_count(),0);
        } else {
            serial_away_first_round(&mut f,true,|_|Some(false)).await;
            assert_eq!(f.journal_count(),13,"twelve separate module commands and one exact source cue");
        }
        f.finish().await;
    }
}

#[tokio::test]
async fn managed_native_away_waits_for_read_without_spending_rounds_or_replaying_retired_source() {
    let operation_budget=homesense_core::away_execution::Policy::default().total_ms;
    for retirement in ["ready","owner-revoked","expired","long-read"] {
        let mut f=Fixture::build_with_away(true,false,true,true).await;let(_tokens,lease)=away_status_lease(&f);
        f.click(0,6).await;f.collect().await;
        let events=f.panels.take_native_dispatch();assert_eq!(events.len(),1);f.receiver.enqueue_native(events).unwrap();
        let permit=f.actor.reserve_physical_read("bus".into(),f.time.load(Ordering::SeqCst)+if retirement=="long-read"{operation_budget}else{300}).await.unwrap();
        f.receiver.poll_causal(&f.actor,&f.document,&mut f.scenes,f.owner.feedback(),5000,None,Some(&mut f.router)).await;
        assert_eq!(f.journal_count(),0,"logical admission waits; no short-lived physical tickets are renewed behind a read");
        let pending=away_status(&f,&lease).await;assert_eq!((pending.state.as_str(),pending.rounds),("pending",0));
        assert!(f.owner.step().await.unwrap().is_none());
        if retirement=="owner-revoked" {f.panels.disconnect();}
        if retirement=="expired" {f.time.fetch_add(operation_budget+1,Ordering::SeqCst);}
        if retirement=="long-read" {f.time.fetch_add(operation_budget+500,Ordering::SeqCst);}
        drop(permit);f.time.fetch_add(51,Ordering::SeqCst);tick_away(&mut f).await;
        if retirement=="ready" {
            assert_eq!(f.journal_count(),2);assert_eq!(away_status(&f,&lease).await.rounds,1);
            serial_away_first_round(&mut f,true,|_|Some(false)).await;
            assert_eq!(f.journal_count(),13);
        } else {
            let done=away_status(&f,&lease).await;assert_eq!(done.rounds,0);
            assert_eq!(done.state,if matches!(retirement,"expired"|"long-read"){"failed"}else{"cancelled"});
            assert!(f.owner.step().await.unwrap().is_none());assert_eq!(f.journal_count(),0);
            f.peer.set_nonblocking(true).unwrap();let mut byte=[0];assert_eq!(f.peer.read(&mut byte).unwrap_err().kind(),std::io::ErrorKind::WouldBlock);
        }
        f.finish().await;
    }
}

#[tokio::test]
async fn all_28_by_6_raw_tcp_keys_execute_once_with_own_cue_in_same_lighting_batch() {
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
        f.time.fetch_add(21,Ordering::SeqCst);
        let outcome=f.owner.step().await.unwrap().expect("source cue and load must share one write");outcome.result.unwrap();
        assert_eq!(outcome.physical_frames,2);assert_eq!(outcome.completed.len(),2);
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

fn away_status_lease(f:&Fixture)->(std::sync::Arc<crate::web_auth::Tokens>,crate::away_auth::Authorization) {
    let tokens=std::sync::Arc::new(crate::web_auth::Tokens::default());
    let mut scope:std::collections::BTreeSet<_>=f.document.away_off.as_ref().unwrap().modules.iter().flat_map(|m|m.devices.iter().cloned()).collect();
    scope.insert("key-0-6".into());
    let secret="synthetic-away-operation-status-reader-0123456789";
    tokens.insert(Uuid::new_v4(),secret,crate::web_auth::Grant{controller:f.controller,role:crate::web_auth::Role::Member,devices:scope,scenes:Default::default()}).unwrap();
    let lease=tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap();(tokens,lease.into())
}
async fn tick_away(f:&mut Fixture) {
    let observation=f.owner.feedback().away_observation(f.actor.monotonic_ms()).unwrap();
    f.actor.tick_away_operations(f.controller,f.actor.task_generation().await.unwrap(),observation).await.unwrap();
}
// This adapter fixture drives Admission directly, without a periodic query
// worker. Supply a synthetic failed read turn, never synthetic Off observations.
async fn failed_away_query(f:&mut Fixture,module:u8) {
    let written=f.actor.monotonic_ms();f.time.fetch_add(500,Ordering::SeqCst);
    let now=f.actor.monotonic_ms();let mut observation=f.owner.feedback().away_observation(now).unwrap();
    for channel in 1..=if module<7{1}else{8} {
        observation.queries.insert(DeviceId::new(format!("away-{module}:{channel}")).unwrap(),
            homesense_core::away_execution::ReadTurn{written_ms:written,finished_ms:now});
    }
    f.actor.tick_away_operations(f.controller,f.actor.task_generation().await.unwrap(),observation).await.unwrap();
}
async fn retry_delay(f:&mut Fixture) {
    f.time.fetch_add(homesense_core::away_execution::Policy::default().delay_ms,Ordering::SeqCst);
    tick_away(f).await;
}
async fn away_status(f:&Fixture,lease:&crate::away_auth::Authorization)->crate::away_execution::View {
    let views=f.actor.away_operation_views(f.controller,f.actor.task_generation().await.unwrap(),lease.clone()).await.unwrap();
    assert_eq!(views.len(),1);views.into_iter().next().unwrap()
}
async fn away_module_feedback(f:&mut Fixture,n:u8,on:bool) {
    let frames=(1..=8u8).flat_map(|ch|[0xae,0x20+n,0xd0+ch,if n<7 {if on{0xf1}else{0xf2}}else if on{100}else{0},0xee]).collect::<Vec<_>>();
    f.input(&frames).await;
}
// Real admission/writer/decoder, one module per step. No future physical
// ticket may write before this module replies or its bounded read fails.
async fn serial_away_first_round(f:&mut Fixture,local_cue:bool,replies:impl Fn(u8)->Option<bool>) {
    serial_away_first_round_partial(f,local_cue,replies,None).await;
}
async fn serial_away_first_round_partial(f:&mut Fixture,local_cue:bool,replies:impl Fn(u8)->Option<bool>,partial:Option<u8>) {
    let mut seen=std::collections::BTreeSet::new();
    let terminal=f.actor.away_terminal_updates();
    for index in 0..12 {
        f.time.fetch_add(21,Ordering::SeqCst);tick_away(f).await;
        f.time.fetch_add(21,Ordering::SeqCst);
        let batch=f.owner.step().await.unwrap().expect("current module must be queued");
        let count=batch.physical_frames;
        batch.result.unwrap();assert_eq!(count,1+usize::from(index==0 && local_cue));
        let mut bytes=vec![0;count*5];f.peer.read_exact(&mut bytes).unwrap();
        if index==0 && local_cue {
            assert_eq!(&bytes[..5],&[0xae,0x40,6,0xf2,0xee],"source cue precedes module request so no later cue writes into its replies");
        }
        let mut module=None;
        for frame in bytes.as_chunks::<5>().0 {
            assert_eq!((frame[0],frame[4]),(0xae,0xee));
            if frame[1]==0x40 {assert_eq!(frame,&[0xae,0x40,6,0xf2,0xee]);}
            else {
                assert!((0x20..0x2c).contains(&frame[1]));assert_eq!(frame[2],0);
                assert_eq!(frame[3],if frame[1]<0x27{0xf2}else{0});
                assert!(module.replace(frame[1]-0x20).is_none());
            }
        }
        let n=module.unwrap();assert!(seen.insert(n));
        assert!(f.owner.step().await.unwrap().is_none(),"next module must wait for the current response");
        if partial==Some(n) {
            let frames=(1..=8u8).flat_map(|ch|[0xae,0x20+n,0xd0+ch,u8::from(ch==1)*100,0xee]).collect::<Vec<_>>();
            f.input(&frames).await;
        }else{match replies(n) {
            Some(on)=>away_module_feedback(f,n,on).await,
            None=>failed_away_query(f,n).await,
        }}
        crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
        assert!(!terminal.has_changed().unwrap(),"partial feedback must not amplify WSS polling");
    }
    assert_eq!(seen.len(),12);
    f.time.fetch_add(21,Ordering::SeqCst);tick_away(f).await;
}
#[tokio::test]
async fn away_terminal_notifies_all_state_readers_once_without_progress_wakes() {
    let mut f=Fixture::build_with_away(true,false,true,true).await;
    let(_tokens,lease)=away_status_lease(&f);
    let mut first=f.actor.away_terminal_updates();let mut second=first.clone();
    f.click(0,6).await;assert_eq!(f.route().await,1);
    assert!(!first.has_changed().unwrap(),"acceptance is not a terminal state");
    serial_away_first_round(&mut f,true,|_|Some(false)).await;
    tokio::time::timeout(Duration::from_millis(50),first.changed()).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_millis(50),second.changed()).await.unwrap().unwrap();
    assert_eq!(away_status(&f,&lease).await.state,"confirmed");
    tick_away(&mut f).await;
    assert!(!first.has_changed().unwrap(),"a completed operation must not wake readers again");
    f.finish().await;
}

#[tokio::test]
async fn managed_native_away_retries_only_failed_module_and_reports_all96_confirmed() {
    let mut f=Fixture::build_with_away(true,false,true,true).await;let(_tokens,lease)=away_status_lease(&f);
    f.input(&[0xae,3,0xd1,0xf1,0xee]).await; // unrelated physical load staysOn
    f.click(0,6).await;assert_eq!(f.route().await,1);assert_eq!(f.journal_count(),0);
    serial_away_first_round(&mut f,true,|n|Some(n==3)).await;
    let pending=away_status(&f,&lease).await;assert_eq!(pending.state,"confirming");assert_eq!(pending.rounds,1);assert_eq!(pending.confirmed_outputs,88);
    let delay=homesense_core::away_execution::Policy::default().delay_ms;
    f.time.fetch_add(delay-1,Ordering::SeqCst);tick_away(&mut f).await;assert_eq!(f.journal_count(),13);
    f.time.fetch_add(1,Ordering::SeqCst);tick_away(&mut f).await;assert_eq!(f.journal_count(),14,"one failed module; no second cue or repeat of confirmed modules");
    f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().unwrap().result.unwrap();
    let mut retry=[0;5];f.peer.read_exact(&mut retry).unwrap();assert_eq!(retry,[0xae,0x23,0,0xf2,0xee]);
    for n in 0..12 {away_module_feedback(&mut f,n,false).await;}
    f.time.fetch_add(21,Ordering::SeqCst);tick_away(&mut f).await;let done=away_status(&f,&lease).await;
    assert_eq!((done.state.as_str(),done.rounds,done.confirmed_outputs),("confirmed",2,96));assert!(done.missing.is_empty());
    assert_eq!(f.owner.feedback().snapshot().devices[&DeviceId::new("load").unwrap()].observed,Some(homesense_core::Observation::Switch(true)));
    f.time.fetch_add(1000,Ordering::SeqCst);tick_away(&mut f).await;assert_eq!(f.journal_count(),14);assert!(f.owner.step().await.unwrap().is_none());
    f.finish().await;
}
#[tokio::test]
async fn managed_native_away_missing_feedback_exhausts_three_rounds_with_exact_scope() {
    let mut f=Fixture::build_with_away(true,false,true,true).await;let(_tokens,lease)=away_status_lease(&f);
    f.click(0,6).await;f.route().await;
    serial_away_first_round(&mut f,true,|n|if n==11 {None}else{Some(false)}).await;
    // Each failure is a missing AD8 module; refreshed good ports are only
    // observations, never additional Off commands or forged journal receipts.
    for _ in 0..2 {
        failed_away_query(&mut f,11).await;
        retry_delay(&mut f).await;
        f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().unwrap().result.unwrap();
        let mut retry=[0;5];f.peer.read_exact(&mut retry).unwrap();assert_eq!(retry,[0xae,0x2b,0,0,0xee]);
    }
    failed_away_query(&mut f,11).await;
    let failed=away_status(&f,&lease).await;
    assert_eq!((failed.state.as_str(),failed.rounds),("failed",3));assert_eq!(failed.reason.as_deref(),Some("attempts_exhausted"));
    assert_eq!(failed.missing,vec![crate::away_execution::Missing{module_id:"away-11".into(),channels:(1..=8).collect()}]);assert_eq!(f.journal_count(),15);
    f.time.fetch_add(1000,Ordering::SeqCst);tick_away(&mut f).await;assert_eq!(away_status(&f,&lease).await,failed);assert!(f.owner.step().await.unwrap().is_none());
    f.finish().await;
}

#[tokio::test]
async fn managed_away_pending_read_preserves_explicit_motor_stop_priority() {
    let mut f=Fixture::build_with_away(true,true,true,true).await;
    f.click(0,6).await;
    let permit=f.actor.reserve_physical_read("bus".into(),f.actor.monotonic_ms()+300).await.unwrap();
    assert_eq!(f.route().await,1);assert_eq!(f.journal_count(),0);
    f.click(0,2).await;assert_eq!(f.route().await,1);
    assert!(f.owner.step().await.unwrap().is_none(),"STOP also waits for the real half-duplex read owner");
    drop(permit);f.time.fetch_add(21,Ordering::SeqCst);
    let result=f.owner.step().await.unwrap().expect("STOP is not gated behind a pending Away logical job");result.result.unwrap();
    let mut first=[0;5];f.peer.read_exact(&mut first).unwrap();assert_eq!(first,[0xae,0x7e,1,0xf0,0xee]);
    f.finish().await;
}

#[tokio::test]
async fn managed_away_serial_admission_cancels_future_module_on_before_first_module_writes() {
    let mut f=Fixture::build_with_away(true,false,true,true).await;
    f.actor.submit(homesense_hap::requests::Batch {controller:f.controller,intents:vec![homesense_hap::requests::Intent {
        device:DeviceId::new("away-9:1").unwrap(),command:homesense_core::Command::SetLevel {
            level:homesense_core::Percent::new(100).unwrap(),transition:homesense_core::LevelTransition::Immediate},
    }],subscriptions:vec![]}).await.unwrap();
    f.click(0,6).await;assert_eq!(f.route().await,1);
    // A preexisting On of a future module must retire now, not when that
    // module eventually acquires its new short-lived Off ticket.
    let db=rusqlite::Connection::open(f.directory.join("runtime.sqlite")).unwrap();
    let status:String=db.query_row("SELECT status FROM command_journal ORDER BY command_id LIMIT 1",[],|r|r.get(0)).unwrap();
    assert_eq!(status,"cancelled");drop(db);
    serial_away_first_round(&mut f,true,|_|Some(false)).await;
    assert_eq!(f.journal_count(),14);
    assert!(f.owner.step().await.unwrap().is_none());
    f.finish().await;
}

#[tokio::test]
async fn managed_native_away_cancels_delayed_retry_after_owner_retirement_or_manual_override() {
    for manual in [false,true] {
        let mut f=Fixture::build_with_away(true,false,true,true).await;let(_tokens,lease)=away_status_lease(&f);
        f.click(0,6).await;f.route().await;
        serial_away_first_round(&mut f,true,|_|Some(true)).await;
        if manual {
            f.actor.submit(homesense_hap::requests::Batch{controller:f.controller,intents:vec![homesense_hap::requests::Intent{
                device:DeviceId::new("away-0:1").unwrap(),command:homesense_core::Command::Switch(true)}],subscriptions:vec![]}).await.unwrap();
        }else{f.panels.disconnect();}
        retry_delay(&mut f).await;
        let cancelled=away_status(&f,&lease).await;assert_eq!((cancelled.state.as_str(),cancelled.rounds),("cancelled",1));
        assert_eq!(cancelled.reason.as_deref(),Some(if manual{"manual_override"}else{"authority_changed"}));
        if manual {
            f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().unwrap().result.unwrap();
            let mut explicit=[0;5];f.peer.read_exact(&mut explicit).unwrap();assert_eq!(explicit,[0xae,0x20,1,0xf1,0xee]);
        }
        assert_eq!(f.journal_count(),if manual{14}else{13});
        f.time.fetch_add(1000,Ordering::SeqCst);tick_away(&mut f).await;assert!(f.owner.step().await.unwrap().is_none());assert_eq!(away_status(&f,&lease).await,cancelled);
        f.finish().await;
    }
}

#[tokio::test]
async fn managed_away_cancels_queued_and_selected_retry_without_poisoning_transport() {
    for selected in [false,true] {
        let mut f=Fixture::build_with_away(true,false,true,true).await;let(_tokens,lease)=away_status_lease(&f);
        f.click(0,6).await;f.route().await;
        serial_away_first_round(&mut f,true,|n|Some(n==3)).await;
        retry_delay(&mut f).await;
        assert_eq!(away_status(&f,&lease).await.rounds,2);
        let retry_id={let db=rusqlite::Connection::open(f.directory.join("runtime.sqlite")).unwrap();db.query_row("SELECT max(command_id) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap()};
        let ticket=if selected {Some(f.actor.dispatch().await.unwrap().unwrap())}else{None};
        // Another module was already confirmed Off and is deliberately reopened.
        // This must cancel the whole operation, including its queued retry of 23.
        f.actor.submit(homesense_hap::requests::Batch{controller:f.controller,intents:vec![homesense_hap::requests::Intent{
            device:DeviceId::new("away-0:1").unwrap(),command:homesense_core::Command::Switch(true)}],subscriptions:vec![]}).await.unwrap();
        if let Some(ticket)=ticket {
            assert!(ticket.away_authorization.as_ref().unwrap().guarded_operation());
            let listener=TcpListener::bind("127.0.0.1:0").unwrap();
            let socket=TcpStream::connect(listener.local_addr().unwrap()).unwrap();let(mut peer,_)=listener.accept().unwrap();
            let mut connection=hb_transport::Connection::for_simulation(socket,1);
            let(result,proof)=crate::transport_owner::send_guarded_ticket(&ticket,&mut connection,||f.actor.monotonic_ms());
            assert_eq!(result.unwrap_err().bytes_written,0);let proof=proof.expect("guard refusal has sealed zero-byte evidence");assert!(proof.away());
            f.actor.finish_projection_discard(proof).await.unwrap();
            peer.set_nonblocking(true).unwrap();let mut byte=[0];assert_eq!(peer.read(&mut byte).unwrap_err().kind(),std::io::ErrorKind::WouldBlock);
        }
        assert_eq!(away_status(&f,&lease).await.reason.as_deref(),Some("manual_override"));
        f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().unwrap().result.unwrap();
        let mut explicit=[0;5];f.peer.read_exact(&mut explicit).unwrap();assert_eq!(explicit,[0xae,0x20,1,0xf1,0xee]);
        assert!(f.owner.step().await.unwrap().is_none());
        assert!(f.actor.task_generation().await.is_ok(),"zero-byte retirement must not fault the transport");
        let db=rusqlite::Connection::open(f.directory.join("runtime.sqlite")).unwrap();
        let state:String=db.query_row("SELECT status FROM command_journal WHERE command_id=?1",[retry_id],|r|r.get(0)).unwrap();assert_eq!(state,"cancelled");drop(db);
        f.finish().await;
    }
}

#[tokio::test]
async fn managed_away_success_keeps_only_feedback_gated_legacy_reset_authority() {
    let mut f=Fixture::build_with_away(true,false,true,true).await;
    let(_tokens,status_lease)=away_status_lease(&f);
    let crate::away_auth::Authorization::Web(web)=status_lease.clone() else{unreachable!()};
    let lease=crate::away_auth::Authorization::keypad(web,&crate::away_auth::KeypadCommand::KeypadPress{keypad:"panel-0".into(),key:6}).unwrap();
    let observation=f.owner.feedback().away_observation(f.actor.monotonic_ms()).unwrap();
    let accepted=f.actor.start_away_operation(f.actor.task_generation().await.unwrap(),lease,observation,std::time::Instant::now()+Duration::from_secs(2)).await.unwrap();
    assert_eq!(accepted.commands.len(),2,"only first module and original completion LED are accepted");
    serial_away_first_round(&mut f,false,|_|Some(false)).await;
    assert_eq!(away_status(&f,&status_lease).await.state,"confirmed");
    crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
    f.time.fetch_add(21,Ordering::SeqCst);let reset=f.owner.step().await.unwrap().expect("success must not revoke the pending legitimate completion LED");reset.result.unwrap();
    let mut frame=[0;5];f.peer.read_exact(&mut frame).unwrap();assert_eq!(frame,[0xae,0x40,6,0xf2,0xee]);
    assert!(f.owner.step().await.unwrap().is_none());
    f.finish().await;
}

#[tokio::test]
async fn managed_away_legacy_completion_follows_retry_writes_and_never_signals_exhaustion() {
    for exhaust in [false,true] {
        let mut f=Fixture::build_with_away(true,false,true,true).await;
        let(_tokens,status_lease)=away_status_lease(&f);
        let crate::away_auth::Authorization::Web(web)=status_lease.clone() else{unreachable!()};
        let lease=crate::away_auth::Authorization::keypad(web,&crate::away_auth::KeypadCommand::KeypadPress{keypad:"panel-0".into(),key:6}).unwrap();
        let observation=f.owner.feedback().away_observation(f.actor.monotonic_ms()).unwrap();
        let accepted=f.actor.start_away_operation(f.actor.task_generation().await.unwrap(),lease,observation,std::time::Instant::now()+Duration::from_secs(2)).await.unwrap();
        let reset=*accepted.commands.last().unwrap();
        serial_away_first_round(&mut f,false,|n|Some(n==3)).await;
        crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
        assert!(f.owner.step().await.unwrap().is_none());
        for _ in 0..2 {
            retry_delay(&mut f).await;
            if !exhaust {
                // Old successful samples, even refreshed after retry admission,
                // cannot release the LED before the new module actually writes.
                for n in 0..12 {away_module_feedback(&mut f,n,false).await;}
                crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
            }
            f.time.fetch_add(21,Ordering::SeqCst);let batch=f.owner.step().await.unwrap().unwrap();batch.result.unwrap();assert_eq!(batch.physical_frames,1);
            let mut retry=[0;5];f.peer.read_exact(&mut retry).unwrap();assert_eq!(retry,[0xae,0x23,0,0xf2,0xee]);
            crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
            assert!(f.owner.step().await.unwrap().is_none(),"pre-write feedback cannot release the original completion LED");
            for n in 0..12 {away_module_feedback(&mut f,n,exhaust && n==3).await;}
            f.time.fetch_add(21,Ordering::SeqCst);tick_away(&mut f).await;
            crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
            if !exhaust {
                let done=away_status(&f,&status_lease).await;
                assert_eq!((done.state.as_str(),done.rounds,done.confirmed_outputs),("confirmed",2,96));
                f.time.fetch_add(21,Ordering::SeqCst);let led=f.owner.step().await.unwrap().expect("same original LED must survive this operation's own retry");led.result.unwrap();
                let mut frame=[0;5];f.peer.read_exact(&mut frame).unwrap();assert_eq!(frame,[0xae,0x40,6,0xf2,0xee]);
                assert_eq!(f.journal_count(),14,"retry cannot create a replacement completion ticket");
                break;
            }
            assert!(f.owner.step().await.unwrap().is_none());
        }
        if exhaust {
            let waiting=away_status(&f,&status_lease).await;
            assert_eq!((waiting.state.as_str(),waiting.rounds),("failed",3),"known On after the third attempt fails without waiting for the total deadline");
            assert!(f.owner.step().await.unwrap().is_none(),"completion LED cannot run after failure");
            let admitted=f.journal_count();
            for n in 0..12 {if n!=3 {away_module_feedback(&mut f,n,false).await;}}
            tick_away(&mut f).await;
            let failed=away_status(&f,&status_lease).await;
            assert_eq!((failed.state.as_str(),failed.rounds,failed.confirmed_outputs),("failed",3,88));
            assert_eq!(failed.missing.len(),1);
            assert_eq!(f.journal_count(),admitted,"final confirmation must not admit another write or LED ticket");
            assert!(f.owner.step().await.unwrap().is_none());
            let db=rusqlite::Connection::open(f.directory.join("runtime.sqlite")).unwrap();
            let status:String=db.query_row("SELECT status FROM command_journal WHERE command_id=?1",[reset as i64],|row|row.get(0)).unwrap();
            assert_eq!(status,"cancelled");
        }
        f.finish().await;
    }
}

#[tokio::test]
async fn managed_away_completion_requires_combined_dimmer_zero_even_when_supply_is_off() {
    let mut f=Fixture::build_with_away_supply(true,false,true,true,true).await;
    let(_tokens,status_lease)=away_status_lease(&f);
    let crate::away_auth::Authorization::Web(web)=status_lease.clone() else{unreachable!()};
    let lease=crate::away_auth::Authorization::keypad(web,&crate::away_auth::KeypadCommand::KeypadPress{keypad:"panel-0".into(),key:6}).unwrap();
    let observation=f.owner.feedback().away_observation(f.actor.monotonic_ms()).unwrap();
    f.actor.start_away_operation(f.actor.task_generation().await.unwrap(),lease,observation,std::time::Instant::now()+Duration::from_secs(2)).await.unwrap();
    serial_away_first_round_partial(&mut f,false,|_|Some(false),Some(7)).await;
    // All relays are Off, but one AD8 output did not reach zero.
    tick_away(&mut f).await;
    assert_eq!(away_status(&f,&status_lease).await.confirmed_outputs,95);
    crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
    assert!(f.owner.step().await.unwrap().is_none(),"a dark combined light is not proof that all96 physical outputs closed");
    f.time.fetch_add(homesense_core::away_execution::Policy::default().feedback_ms/2+1,Ordering::SeqCst);
    let config=homesense_storage::Store::open(f.directory.join("runtime.sqlite")).unwrap().active_bundle(ExecutionContext::Simulation).unwrap().unwrap().configuration;
    let targets=crate::physical_queries::plan(&config,ExecutionContext::Simulation).unwrap();
    let reads=crate::control_batch::query_requirements(&f.actor,f.owner.feedback(),&config,&targets,f.controller).await.unwrap();
    assert!(reads.keys().any(|index|targets[*index].module.address().get()==0x27),"managed Away cannot replace the AD8 refresh with its supply relay");
    f.input(&[0xae,0x27,0xd1,0,0xee]).await;
    f.time.fetch_add(21,Ordering::SeqCst);tick_away(&mut f).await;assert_eq!(away_status(&f,&status_lease).await.state,"confirmed");
    crate::control_batch::tick(&f.actor,f.controller,f.owner.feedback()).await.unwrap();
    f.time.fetch_add(21,Ordering::SeqCst);f.owner.step().await.unwrap().unwrap().result.unwrap();
    let mut led=[0;5];f.peer.read_exact(&mut led).unwrap();assert_eq!(led,[0xae,0x40,6,0xf2,0xee]);
    f.finish().await;
}
