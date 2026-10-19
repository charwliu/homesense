//! Direct AD8 writes query without a ramp delay; exact-level LEDs still wait for actual feedback.
use super::*;
use crate::admission::{Admission,Options};
use homesense_core::{Command,DeviceId,ExecutionContext,LevelTransition,Percent};
use homesense_hap::requests::{Batch,Intent};
use homesense_storage::{Store,journal::Status};
use std::{collections::VecDeque,sync::{Arc,Mutex},time::{Duration,Instant}};
use uuid::Uuid;

#[derive(Default)]
struct Bus {input:VecDeque<u8>,writes:Vec<([u8;5],Instant)>,power:bool,ramp:Option<Instant>}
#[derive(Clone)]
struct Wire(Arc<Mutex<Bus>>);
impl Read for Wire {
    fn read(&mut self,out:&mut [u8])->std::io::Result<usize> {
        let mut b=self.0.lock().unwrap();
        if b.input.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into());}
        let n=out.len().min(b.input.len());for byte in &mut out[..n]{*byte=b.input.pop_front().unwrap();}Ok(n)
    }
}
impl Write for Wire {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
        let (frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());let mut b=self.0.lock().unwrap();
        for &f in frames {
            b.writes.push((f,Instant::now()));
            match (f[1],f[2]) {
                (3,1)=>{b.power=f[3]==0xf1;},
                (3,2)=>assert_eq!(f[3],0xf2),
                (3,0xa1..=0xa8)=>{let v=if f[2]==0xa1 && b.power {0xf1}else{0xf2};b.input.extend([0xae,3,f[2]+0x30,v,0xee]);},
                (0x15,1)=>{assert_eq!(f[3],17);b.ramp=Some(Instant::now());},
                (0x15,0xa0)=>{let v=if b.ramp.is_some(){17}else{60};for ch in 1..=8{b.input.extend([0xae,0x15,0xd0+ch,v,0xee]);}},
                (0x41,1|2)=>assert!(matches!(f[3],0xf1|0xf2)),
                _=>panic!("unexpected output {f:x?}"),
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}

async fn run_with_scope(independent:bool,power_policy:Option<bool>,preserve:bool) {
    let gateways=if independent{vec!["bus","other"]}else{vec!["bus"]};
    let cfg=serde_json::json!({"schema_version":2,"revision":1,
        "gateways":gateways.iter().map(|g|serde_json::json!({"id":g,"endpoint":if *g=="bus"{"127.0.0.1:1"}else{"127.0.0.1:2"},"modules":[
            {"id":format!("{g}-relay"),"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":format!("{g}-ad8"),"address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
            {"id":format!("{g}-panel"),"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}
        ]})).collect::<Vec<_>>(),
        "devices":[
            {"id":"dim","name":"Combined dimmer","module_id":"bus-ad8","channel":1,"supply":{"module_id":"bus-relay","channel":1},"default_on_level":100,"enabled":true},
            {"id":"power","name":"Power port","module_id":"bus-relay","channel":1,"enabled":true},
            {"id":"off","name":"Other off load","module_id":"bus-relay","channel":2,"enabled":true},
            {"id":"led1","name":"Linked LED one","module_id":"bus-panel","channel":1,"enabled":true},
            {"id":"led2","name":"Linked LED two","module_id":"bus-panel","channel":2,"enabled":true}
        ]});
    let raw=serde_json::to_vec(&cfg).unwrap();let c=homesense_config::Configuration::parse(&raw,ExecutionContext::Simulation).unwrap();
    let dir=std::env::temp_dir().join(format!("mixed-feedback-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let path=dir.join("runtime.sqlite");
    if let Some(collision)=power_policy {
        let step=|id:&str,value:u8|serde_json::json!({"source":"synthetic power review","offset_ms":0,"device":id,"command":if id=="dim"{serde_json::json!({"kind":"level","value":value,"ramp":false})}else{serde_json::json!({"kind":"switch","on":value>0})}});
        let scene=|id:&str,dim:u8,other:u8|serde_json::json!({"id":id,"enabled":true,"review_evidence":"synthetic reviewed power profile","max_lateness_ms":5000,"steps":[step("dim",dim),step("off",other)]});
        let mut doc=serde_json::json!({"schema_version":1,"configuration_revision":1,
            "variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],
            "timed_scenes":[scene("eco",17,0),scene("other",if collision{30}else{0},if collision{0}else{1}),scene("all-off",0,0)],
            "scene_switch_groups":[{"id":"selection","variable":"selection","review_evidence":"synthetic reviewed selection","members":[{"scene":"eco","value":1,"off_scene":"all-off"},{"scene":"other","value":2,"off_scene":"all-off"}],"reconciliation":{"enabled":true,"policy":if collision{"power_pattern_then_profile_level"}else{"unique_light_on_pattern"},"max_age_ms":20000,"review_evidence":"synthetic uniqueness contract"}}],
            "indicator_links":[{"id":"one","enabled":true,"review_evidence":"synthetic LED profile","lights":["dim","off"],"leds":["led1"],"scene":"eco"},{"id":"two","enabled":true,"review_evidence":"synthetic LED profile","lights":["dim","off"],"leds":["led2"],"scene":"other"}]
        });
        if preserve {
            doc["scene_switch_groups"][0]["reconciliation"]["omitted_targets"]=serde_json::json!("preserve");
            doc["timed_scenes"][0]["steps"].as_array_mut().unwrap().pop();
            doc["indicator_links"][0]["lights"]=serde_json::json!(["dim"]);
        }
        let doc=serde_json::to_vec(&doc).unwrap();
        homesense_config::rule_document::Document::parse(&doc,&c).unwrap();
        Store::open(&path).unwrap().publish_automation(None,&raw,&doc,ExecutionContext::Simulation,&[]).unwrap();
    }else {Store::open(&path).unwrap().publish_configuration(None,&raw,ExecutionContext::Simulation,&[]).unwrap();}
    let controller=Uuid::new_v4();
    let a=Admission::open(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();a.transport_failed().await.unwrap();
    let buses=gateways.iter().map(|g|(g.to_string(),Arc::new(Mutex::new(Bus::default())))).collect::<std::collections::BTreeMap<_,_>>();
    let conns=buses.iter().map(|(g,b)|(g.clone(),hb_transport::Connection::for_simulation(Wire(b.clone()),2))).collect();
    let feedback=Feedback::new(&c,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),2).unwrap();
    let mut owner=ObservedTransport::new_recovering(TransportOwner::new(a.clone(),conns),feedback,2).unwrap();let age=c.refresh_policy().observation_max_age_ms();
    owner.query_recovery_with_policy(&c,ExecutionContext::Simulation,500,age,Synchronization::OutputsAndKeypadEvents).await.unwrap();
    if independent {owner.enable_independent(&c,Arc::new(|_,_|Err(std::io::ErrorKind::NotConnected.into()))).await.unwrap();}
    owner.configure_output_refresh(&c,controller,a.task_generation().await.unwrap(),ExecutionContext::Simulation,500,age).unwrap();
    // The real decoded key suppresses idle polling for 300s. Confirmation may
    // still read these three changed ports; no unrelated read is permitted.
    buses["bus"].lock().unwrap().input.extend([0xae,0x41,0xd1,0xf1,0xee,0xae,0x41,0xd1,0xf2,0xee]);
    if independent {owner.start_local_reads().unwrap();for _ in 0..10{owner.local_tick().await.unwrap();owner.start_local_reads().unwrap();tokio::time::sleep(Duration::from_millis(2)).await;}}
    else {owner.receive("bus").await.unwrap();}
    let baseline=buses["bus"].lock().unwrap().writes.len();
    let intents=vec![
        Intent{device:DeviceId::new("dim").unwrap(),command:Command::SetLevel{level:Percent::new(17).unwrap(),transition:LevelTransition::Immediate}},
        Intent{device:DeviceId::new("off").unwrap(),command:Command::Switch(false)},
        Intent{device:DeviceId::new("led1").unwrap(),command:Command::PanelLed(true)},
        Intent{device:DeviceId::new("led2").unwrap(),command:Command::PanelLed(false)},
    ];
    let ids=a.submit_scene_with_feedback_before(a.task_generation().await.unwrap(),Batch{controller,intents,subscriptions:vec![]},0,None,None).await.unwrap();
    let end=Instant::now()+Duration::from_secs(10);let store=Store::open(&path).unwrap();
    loop {
        if independent {owner.local_tick().await.unwrap();}else{owner.receive_pending_output().await.unwrap();owner.receive("bus").await.unwrap();owner.refresh_outputs_if_due().await.unwrap();}
        crate::control_batch::tick(&a,controller,owner.feedback()).await.unwrap();if let Some(write)=owner.step().await.unwrap(){write.result.unwrap();}
        if independent {owner.start_local_reads().unwrap();}
        if ids.iter().all(|id|store.command_status(a.boot_id(),*id).unwrap()==Some(Status::Written))
            && owner.feedback().module_truth().received_port_state("bus",0x15,1).unwrap().observed==Some(homesense_core::Observation::Level(Percent::new(17).unwrap())) {break;}
        assert!(Instant::now()<end,"fresh relay and direct level evidence must complete before the LED deadline");tokio::time::sleep(Duration::from_millis(2)).await;
    }
    {let b=buses["bus"].lock().unwrap();let ramp=b.ramp.unwrap();let writes=&b.writes[baseline..];
        let dim_queries=writes.iter().filter(|(f,_)|f[1]==0x15&&f[2]==0xa0).collect::<Vec<_>>();assert!(!dim_queries.is_empty());assert!(dim_queries.iter().all(|(_,at)|at.duration_since(ramp)<Duration::from_secs(1)),"direct percentage must not wait six seconds before querying");
        let led=writes.iter().find(|(f,_)|f[1]==0x41).unwrap().1;assert!(led.duration_since(ramp)<Duration::from_secs(10));
        if power_policy==Some(false) {assert!(led.duration_since(ramp)<Duration::from_secs(1),"unique power scene LED uses actual relay feedback before brightness arrives");}
        else {let query=dim_queries.first().unwrap().1;assert!(led>=query,"ambiguous or unreviewed LEDs still require actual brightness readback");assert!(led.duration_since(ramp)<Duration::from_secs(1));}
        for ch in [1,2] {let at=writes.iter().rev().find(|(f,at)|f[1]==3&&f[2]==0xa0+ch&&*at<=led).unwrap().1;assert!(led.duration_since(at)<Duration::from_secs(5));}
        assert!(writes.iter().all(|(f,_)|f[1]!=3||f[2]<=2||matches!(f[2],0xa1|0xa2)),"no unrelated relay channel reads in the quiet window");
    }
    drop(store);drop(owner);a.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn direct_level_query_and_relay_feedback_satisfy_led_barrier(){run(false,None).await;}
#[tokio::test]
async fn independent_direct_level_query_and_relay_feedback_satisfy_led_barrier(){run(true,None).await;}
#[tokio::test]
async fn unique_power_scene_led_does_not_wait_for_a_ramp_delay(){run(false,Some(false)).await;}
#[tokio::test]
async fn independent_unique_power_scene_led_does_not_wait_for_a_ramp_delay(){run(true,Some(false)).await;}
#[tokio::test]
async fn colliding_power_profiles_still_wait_for_brightness(){run(false,Some(true)).await;}
#[tokio::test]
async fn independent_colliding_power_profiles_still_wait_for_brightness(){run(true,Some(true)).await;}

async fn run(independent:bool,power_policy:Option<bool>) {run_with_scope(independent,power_policy,false).await;}
#[tokio::test]
async fn overlapping_sparse_profiles_wait_for_actual_brightness_despite_different_target_maps(){run_with_scope(false,Some(true),true).await;}
#[tokio::test]
async fn independent_overlapping_sparse_profiles_wait_for_actual_brightness(){run_with_scope(true,Some(true),true).await;}
