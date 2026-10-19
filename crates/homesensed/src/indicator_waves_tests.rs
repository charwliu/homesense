use super::*;
use crate::{admission::Options,transport_owner::GatewayRead,automation_drive::background_led_phase::Phase};
use homesense_core::{Command,ExecutionContext};
use homesense_storage::Store;
use serde_json::json;
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering}},io::{Read,Write},path::PathBuf};
struct Fixture {actor:Admission,controller:Uuid,clock:&'static AtomicU64,auth:Arc<AtomicBool>,metadata_delay:Arc<AtomicU64>,path:PathBuf,document:Document,feedback:Feedback,scenes:SceneScheduler}
impl Fixture {
    async fn new()->Self { Self::with_combined_light(false).await }
    async fn with_combined_light(combined:bool)->Self {Self::with_lights(combined,false).await}
    async fn with_lights(combined:bool,separate:bool)->Self {Self::with_native(combined,separate,false).await}
    async fn with_native(combined:bool,separate:bool,native:bool)->Self {Self::with_native_layout(combined,separate,native,false).await}
    async fn with_native_layout(combined:bool,separate:bool,native:bool,sparse:bool)->Self {
        let mut modules=(0..28).map(|n|json!({"id":format!("panel{n}"),"address":format!("{:02X}",0x60+n),"profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}})).collect::<Vec<_>>();
        modules.push(json!({"id":"relay","address":"20","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
        if combined {modules.push(json!({"id":"dimmer","address":"21","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}));}
        let mut devices=(0..168).map(|n|json!({"id":format!("led{n}"),"name":"Synthetic indicator","module_id":format!("panel{}",n/6),"channel":n%6+1,"enabled":true})).collect::<Vec<_>>();
        devices.push(if combined {json!({"id":"light","name":"Synthetic combined light","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100,"supply":{"module_id":"relay","channel":1}})}else{json!({"id":"light","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true})});
        if combined {devices.push(json!({"id":"supply","name":"Synthetic supply","module_id":"relay","channel":1,"enabled":true}));}
        if separate {devices.push(json!({"id":"other-light","name":"Independent relay","module_id":"relay","channel":2,"enabled":true}));}
        let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
        let mut rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"indicator_links":(0..3).map(|n|json!({"id":format!("link{n}"),"enabled":true,"review_evidence":"synthetic wave capacity","lights":["light"],"leds":(n*64..168.min((n+1)*64)).map(|i|format!("led{i}")).collect::<Vec<_>>()})).collect::<Vec<_>>()});
        if native {
            assert!(separate && !combined);
            rules["variables"]=json!([{"id":"selection","kind":"integer","lifetime":"persistent"}]);
            rules["timed_scenes"]=json!([("first",true,false),("second",false,true),("off",false,false)].map(|(id,on,other)|json!({"id":id,"enabled":true,"review_evidence":"synthetic native completion cache","max_lateness_ms":1000,"steps":[{"source":"synthetic main","offset_ms":0,"device":"light","command":{"kind":"switch","on":on}},{"source":"synthetic other","offset_ms":0,"device":"other-light","command":{"kind":"switch","on":other}}]})));
            rules["scene_switch_groups"]=json!([{"id":"native-group","variable":"selection","review_evidence":"synthetic source group","members":[{"scene":"first","value":1,"off_scene":"off"},{"scene":"second","value":2,"off_scene":"off"}],"reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":20000,"review_evidence":"synthetic current physical patterns"}}]);
            for link in rules["indicator_links"].as_array_mut().unwrap(){link["scene"]=json!("first");link["lights"]=json!(["light","other-light"]);}
            if sparse {
                let mut links=vec![json!({"id":"link0","enabled":true,"review_evidence":"synthetic unchanged aliases","scene":"second","lights":["light","other-light"],"leds":(0..8).map(|n|format!("led{n}")).collect::<Vec<_>>()})];
                for (index,(start,end)) in [(8,72),(72,136),(136,167)].into_iter().enumerate(){links.push(json!({"id":format!("ordinary{index}"),"enabled":true,"review_evidence":"synthetic independent capacity","lights":["other-light"],"leds":(start..end).map(|n|format!("led{n}")).collect::<Vec<_>>()}));}
                links.push(json!({"id":"link2","enabled":true,"review_evidence":"synthetic changed ninth alias","scene":"first","lights":["light","other-light"],"leds":["led167"]}));rules["indicator_links"]=json!(links);
            }
        }
        let rules=serde_json::to_vec(&rules).unwrap();
        let parsed=homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();let mut document=Document::parse(&rules,&parsed).unwrap();
        if separate && !sparse {document.indicator_links[0].leds.truncate(2);document.indicator_links[1].leds=(2..66).map(|n|DeviceId::new(format!("led{n}")).unwrap()).collect();document.indicator_links[2].leds=(66..168).map(|n|DeviceId::new(format!("led{n}")).unwrap()).collect();for link in &mut document.indicator_links[1..] {link.lights=vec![DeviceId::new("other-light").unwrap()];}}
        let path=std::env::temp_dir().join(format!("led168-{}.sqlite",Uuid::new_v4()));Store::open(&path).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
        let clock=&*Box::leak(Box::new(AtomicU64::new(500)));let auth=Arc::new(AtomicBool::new(true));let flag=auth.clone();
        let metadata_delay=Arc::new(AtomicU64::new(0));let delay=metadata_delay.clone();
        let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(20),..Default::default()},move |_|{clock.fetch_add(delay.swap(0,Ordering::SeqCst),Ordering::SeqCst);flag.load(Ordering::Acquire)},clock).await.unwrap();actor.synchronized(1,1).await.unwrap();
        let controller=Uuid::new_v4();let mut feedback=Feedback::new(&parsed,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:500,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd1,value:0xf2}]}}).unwrap();
        if separate {feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:500,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd2,value:0xf2}]}}).unwrap();}
        if combined {feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:500,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x21,command:0xd1,value:100}]}}).unwrap();}
        let scenes=SceneScheduler::new(actor.clone(),controller,8,500).unwrap();Self{actor,controller,clock,auth,metadata_delay,path,document,feedback,scenes}
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

#[tokio::test]
async fn expired_hard_delivery_deadline_cannot_be_extended_by_shared_preparation() {
    let f=Fixture::new().await;
    let mut sync=Synchronizer::default();
    let deadline=f.clock.load(Ordering::SeqCst);
    assert_eq!(sync.tick_wave(&f.actor,f.controller,&f.document,&f.scenes,&f.feedback,20000,deadline).await.unwrap(),0);
    assert!(sync.take_emitted_batches().is_empty());
    assert_eq!(f.count(),0,"expired hard deadline must admit no LED writes");
    f.close().await;
}

#[tokio::test]
async fn metadata_read_delay_preserves_delivery_without_extending_original_feedback_horizon() {
    let f=Fixture::new().await;let mut waves=Waves::default();
    // Deterministic owner-read work before the wave is prepared, not wire
    // latency. The old wave spent 1900ms of its 2000ms before admitting LEDs.
    f.metadata_delay.store(1900,Ordering::SeqCst);
    let wave=f.advance(&mut waves,false).await.unwrap();
    assert_eq!(waves.cycle.as_ref().unwrap().deadline,20500,"the original physical horizon is unchanged");
    assert!(f.write(wave,Arc::new(Mutex::new(vec![]))).await,"all 16 accepted LEDs must fit their delivery phase");
    assert_eq!(f.count(),16);f.close().await;

    let f=Fixture::new().await;let mut waves=Waves::default();
    f.metadata_delay.store(20000,Ordering::SeqCst);
    assert!(f.advance(&mut waves,false).await.is_none(),"metadata work never extends an already expired feedback horizon");
    assert_eq!(f.count(),0);f.close().await;
}


#[tokio::test]
async fn combined_supply_only_change_invalidates_indicator_wave_cache() {
    let mut f=Fixture::with_combined_light(true).await;
    let generation=f.actor.task_generation().await.unwrap();
    let before=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    f.feedback.apply(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:501,
        result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame {address:0x20,command:0xd1,value:0xf1}]}}).unwrap();
    let after=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    assert_ne!(before,after,"unchanged AD8 brightness cannot hide a changed supply relay from LED reconciliation");
    f.close().await;
}

#[tokio::test]
async fn expired_transition_invalidates_cache_even_with_identical_physical_values() {
    let f=Fixture::with_combined_light(true).await;
    let generation=f.actor.task_generation().await.unwrap();
    f.actor.submit(homesense_hap::requests::Batch {controller:f.controller,intents:vec![homesense_hap::requests::Intent {
        device:DeviceId::new("light").unwrap(),command:homesense_core::Command::SetLevel {level:Percent::new(100).unwrap(),transition:homesense_core::LevelTransition::Immediate}
    }],subscriptions:vec![]}).await.unwrap();
    let pending=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    assert_eq!(pending.pending.get(&DeviceId::new("light").unwrap()),Some(&Percent::new(100).unwrap()));
    f.clock.store(16_500,Ordering::SeqCst);
    let expired=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    assert!(expired.pending.is_empty());
    assert_eq!(pending.lights,expired.lights);
    assert_ne!(pending,expired,"previously deferred indicators must be reconsidered after target expiry");
    f.close().await;
}

#[tokio::test]
async fn changed_visited_light_preempts_remaining_catalog_without_replay_or_new_deadline() {
    let mut f=Fixture::with_lights(false,true).await;
    let mut waves=Waves::default();let frames=Arc::new(Mutex::new(vec![]));
    f.feedback.apply(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:500,result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame {address:0x20,command:0xd1,value:0xf1}]}}).unwrap();
    let first=f.advance(&mut waves,false).await.unwrap();
    let deadline=waves.cycle.as_ref().unwrap().deadline;
    assert!(f.write(first,frames.clone()).await);waves.phase_ended(true);f.advance(&mut waves,true).await;
    assert_eq!(f.count(),14);
    f.feedback.apply(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:f.clock.load(Ordering::SeqCst),result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame {address:0x20,command:0xd1,value:0xf2}]}}).unwrap();
    let next=f.advance(&mut waves,false).await.unwrap();
    assert_eq!(waves.cycle.as_ref().unwrap().deadline,deadline);
    assert!(f.write(next,frames.clone()).await);waves.phase_ended(true);
    let written=frames.lock().unwrap().clone();
    assert_eq!(&written[14][1..4],&[0x60,1,0xf2],"already visited changed LED must lead the next wave");
    assert_eq!(&written[15][1..4],&[0x60,2,0xf2]);
    for _ in 0..100 {if let Some(w)=f.advance(&mut waves,false).await {assert!(f.write(w,frames.clone()).await);waves.phase_ended(true);}
        if waves.cycle.is_none(){break;}}
    assert_eq!(waves.outcome.as_ref().unwrap().kind,Kind::Synced);
    assert_eq!(f.count(),168,"unchanged Written LEDs cannot replay during reprioritization");
    assert_eq!(frames.lock().unwrap().len(),168);
    f.close().await;
}

#[tokio::test]
async fn repeated_hot_feedback_leaves_capacity_for_every_other_indicator() {
    let mut f=Fixture::with_lights(false,true).await;
    let mut waves=Waves::default();let frames=Arc::new(Mutex::new(vec![]));let mut deadline=None;
    for round in 0..30 {
        let value=if round%2==0 {0xf2}else{0xf1};
        f.feedback.apply(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:f.clock.load(Ordering::SeqCst),result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame {address:0x20,command:0xd1,value}]}}).unwrap();
        if let Some(wave)=f.advance(&mut waves,false).await {
            let current=waves.cycle.as_ref().unwrap().deadline;
            if let Some(first)=deadline {assert_eq!(current,first);}else{deadline=Some(current);}
            assert!(wave.batches.iter().map(Vec::len).sum::<usize>()<=16);
            assert!(f.write(wave,frames.clone()).await);waves.phase_ended(true);
        }
        f.advance(&mut waves,true).await;
        let ordinary=frames.lock().unwrap().iter().filter(|b|!(b[1]==0x60 && b[2]<=2)).map(|b|(b[1],b[2])).collect::<Vec<_>>();
        let unique=ordinary.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(),ordinary.len(),"unrelated successful writes must not replay");
        if unique.len()==166 {assert!(round<20,"hot feedback must not starve unrelated indicators");f.close().await;return;}
    }
    panic!("hot feedback starved remaining catalog");
}

#[tokio::test]
async fn native_completion_rechecks_finished_wave_without_any_new_physical_value_or_variable() {
    let mut f=Fixture::with_native(false,true,true).await;
    let apply=|f:&mut Fixture,on:bool,other:bool| {let at=f.clock.fetch_add(2,Ordering::SeqCst)+2;f.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd1,value:if on{0xf1}else{0xf2}},hb_protocol::RawFrame{address:0x20,command:0xd2,value:if other{0xf1}else{0xf2}}]}}).unwrap();};
    apply(&mut f,true,true);
    let generation=f.actor.task_generation().await.unwrap();let mut manager=crate::native_scene::Manager::default();
    manager.start(&f.actor,f.controller,generation.clone(),&f.document,"first".into(),"cache-completion".into(),&f.feedback).await.unwrap();
    for _ in 0..2 {f.clock.fetch_add(21,Ordering::SeqCst);let t=f.actor.dispatch().await.unwrap().unwrap();assert!(matches!(t.command.command,Command::Switch(_)));f.actor.finish_write(t.command.id,true).await.unwrap();}
    manager.tick(&f.actor,&f.document,&f.feedback).await;
    for _ in 0..2 {f.clock.fetch_add(21,Ordering::SeqCst);let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::Query);f.actor.finish_write(t.command.id,true).await.unwrap();}
    apply(&mut f,true,false);
    let mut waves=Waves::default();
    for _ in 0..80 {assert!(f.advance(&mut waves,false).await.is_none(),"pending native operation cannot publish success LEDs");if waves.outcome.is_some(){break;}}
    assert!(waves.cycle.is_none(),"the full pending scan really finished");
    let before=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    assert!(before.pending.is_empty(),"relay Off cannot create a pending brightness target");
    manager.tick(&f.actor,&f.document,&f.feedback).await;
    let row=f.actor.native_scene_status(f.controller,generation.clone(),"cache-completion".into()).await.unwrap().unwrap();assert_eq!(row.status,homesense_storage::native_scene::Status::Observed);assert_eq!(row.selected,Some(1));
    let after=Waves::signal(&f.actor,f.controller,&generation,&f.document,&f.feedback).await.unwrap();
    assert_eq!(before.lights,after.lights);assert_eq!(before.variables,after.variables);assert_eq!(before.recorded,after.recorded);assert_eq!(before.pending,after.pending);
    assert_ne!(before,after,"native completion alone must invalidate the finished LED scan cache");
    f.clock.fetch_add(251,Ordering::SeqCst);let wave=f.advance(&mut waves,false).await.unwrap();let frames=Arc::new(Mutex::new(vec![]));assert!(f.write(wave,frames.clone()).await);waves.phase_ended(true);
    assert!(frames.lock().unwrap().iter().all(|b|b[3]==0xf1));assert!(!frames.lock().unwrap().is_empty());f.close().await;
}

#[tokio::test]
async fn native_completion_prioritizes_changed_ninth_alias_over_eight_already_matching_aliases() {
    let mut f=Fixture::with_native_layout(false,true,true,true).await;
    let frames=Arc::new(Mutex::new(vec![]));let mut wire=hb_transport::Connection::for_simulation(Wire(frames.clone()),1);
    // Actual successful initial writes, not fabricated output-ledger entries.
    for n in 0..168 {
        f.actor.submit_panel_led(f.controller,DeviceId::new(format!("led{n}")).unwrap(),false).await.unwrap();
        f.clock.fetch_add(21,Ordering::SeqCst);let ticket=f.actor.dispatch().await.unwrap().unwrap();crate::transport_owner::send_ticket(&ticket,&mut wire,||f.clock.load(Ordering::SeqCst)).unwrap();f.actor.finish_write(ticket.command.id,true).await.unwrap();
    }
    let apply=|f:&mut Fixture| {let at=f.clock.fetch_add(2,Ordering::SeqCst)+2;f.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x20,command:0xd1,value:0xf1},hb_protocol::RawFrame{address:0x20,command:0xd2,value:0xf2}]}}).unwrap();};
    apply(&mut f);let generation=f.actor.task_generation().await.unwrap();let mut manager=crate::native_scene::Manager::default();
    manager.start(&f.actor,f.controller,generation.clone(),&f.document,"first".into(),"sparse-completion".into(),&f.feedback).await.unwrap();
    for _ in 0..2 {f.clock.fetch_add(21,Ordering::SeqCst);let t=f.actor.dispatch().await.unwrap().unwrap();assert!(matches!(t.command.command,Command::Switch(_)));f.actor.finish_write(t.command.id,true).await.unwrap();}
    manager.tick(&f.actor,&f.document,&f.feedback).await;
    for _ in 0..2 {f.clock.fetch_add(21,Ordering::SeqCst);let t=f.actor.dispatch().await.unwrap().unwrap();assert_eq!(t.command.command,Command::Query);f.actor.finish_write(t.command.id,true).await.unwrap();}
    apply(&mut f);let mut waves=Waves::default();
    for _ in 0..80 {
        if let Some(wave)=f.advance(&mut waves,false).await {assert!(f.write(wave,frames.clone()).await);waves.phase_ended(true);}
        if waves.outcome.is_some(){break;}
    }
    assert!(waves.cycle.is_none(),"complete the old pending scan before final proof");
    assert!(!frames.lock().unwrap().iter().any(|frame|frame==&[0xae,0x7b,6,0xf1,0xee]),"pending operation cannot publish the changed alias");
    manager.tick(&f.actor,&f.document,&f.feedback).await;
    assert_eq!(f.actor.native_scene_status(f.controller,generation,"sparse-completion".into()).await.unwrap().unwrap().status,homesense_storage::native_scene::Status::Observed);
    f.clock.fetch_add(251,Ordering::SeqCst);
    let wave=f.advance(&mut waves,false).await.expect("the first new wave must reach the only changed alias, even though eight earlier aliases already match");
    assert!(wave.batches.iter().map(Vec::len).sum::<usize>()<=16);
    let count=frames.lock().unwrap().len();assert!(f.write(wave,frames.clone()).await);waves.phase_ended(true);
    assert!(frames.lock().unwrap()[count..].iter().any(|frame|frame==&[0xae,0x7b,6,0xf1,0xee]));f.close().await;
}
