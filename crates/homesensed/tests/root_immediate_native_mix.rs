//! Synthetic public Control requests through the actual runtime and memory bus.
//! No sockets, original scene inventory, or physical gateway connections.
#![allow(clippy::chunks_exact_to_as_chunks)]
use homesense_config::Configuration;
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    automation_control::Action,
    automation_runtime::{AutomationRuntime, StartupPolicy},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};
use uuid::Uuid;

struct Memory(Arc<Mutex<Vec<u8>>>);
impl std::io::Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Memory {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(value); Ok(value.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

#[derive(Clone, Copy)]
enum Case { Immediate, Timed, ValidMixed, InvalidMixed, Unauthorized, NativeMixed }
async fn check(case: Case) {
    let root = std::env::temp_dir().join(format!("root-immediate-contract-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap(); let db = root.join("runtime.sqlite");
    let mut cfg: serde_json::Value = serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    cfg["devices"].as_array_mut().unwrap().push(json!({"id":"simulation:other","name":"Other synthetic dimmer","gateway":"simulation","address":"15","channel":2,"enabled":true}));
    let cfg_bytes=serde_json::to_vec(&cfg).unwrap();
    let config=Configuration::parse(&cfg_bytes,ExecutionContext::Simulation).unwrap();
    let command=|source:&str,level:u8|json!({"source":source,"action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":level,"ramp":false}}});
    let document=json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"marker","kind":"integer","lifetime":"persistent"}],"rules":[],
      "immediate_scenes":[{"id":"immediate","enabled":true,"review_evidence":"synthetic root contract","activate":[
        {"source":"root/set","action":{"kind":"set","variable":"marker","value":{"kind":"integer","value":7}}},
        {"source":"root/if","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"marker"},"right":{"kind":"integer","value":7}},"then_actions":[command("root/first",10)],"else_actions":[command("root/unreachable",100)]}},
        command("root/second",40),command("root/repeat",10)]}],
      "timed_scenes":[{"id":"timed","enabled":true,"review_evidence":"synthetic explicit 100ms contract","max_lateness_ms":1000,"steps":[{"source":"root/timed","offset_ms":100,"device":"simulation:other","command":{"kind":"level","value":60,"ramp":false}}]}]});
    let mut document=document;
    document["variables"].as_array_mut().unwrap().push(json!({"id":"native.selection","kind":"integer","lifetime":"persistent"}));
    document["timed_scenes"].as_array_mut().unwrap().push(json!({"id":"native","enabled":true,"review_evidence":"synthetic existing selection contract","max_lateness_ms":1000,"steps":[{"source":"root/native","offset_ms":100,"device":"simulation:other","command":{"kind":"level","value":80,"ramp":false}}]}));
    document["timed_scenes"].as_array_mut().unwrap().push(json!({"id":"off","enabled":true,"review_evidence":"synthetic existing off contract","max_lateness_ms":1000,"steps":[{"source":"root/off","offset_ms":0,"device":"simulation:other","command":{"kind":"level","value":0,"ramp":false}}]}));
    document["timed_scenes"].as_array_mut().unwrap().push(json!({"id":"native_other","enabled":true,"review_evidence":"synthetic second existing group member","max_lateness_ms":1000,"steps":[{"source":"root/native_other","offset_ms":0,"device":"simulation:other","command":{"kind":"level","value":30,"ramp":false}}]}));
    document["scene_switch_groups"]=json!([{"id":"existing.group","variable":"native.selection","review_evidence":"synthetic explicit existing selection provider","members":[{"scene":"native","value":1,"off_scene":"off"},{"scene":"native_other","value":2,"off_scene":"off"}],"reset_scenes":["off"]}]);
    homesense_config::rule_document::Document::parse(&serde_json::to_vec(&document).unwrap(),&config).unwrap();
    Store::open(&db).unwrap().publish_automation(None,&cfg_bytes,&serde_json::to_vec(&document).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let rejected=Uuid::new_v4();
    let admission=Admission::open(db.clone(),Options{context:ExecutionContext::Simulation,..Options::default()},move |id|id==controller).await.unwrap();
    admission.synchronized(1,1).await.unwrap();
    let mut runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,1_800_000_000_000,1000,16,Default::default(),StartupPolicy::Suppress).await.unwrap();
    let control=runtime.control();
    let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap();
    let bytes=Arc::new(Mutex::new(vec![]));
    let mut owner=ObservedTransport::new(TransportOwner::new(admission.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Memory(bytes.clone()),1))].into()),feedback);
    let (stop,receive)=tokio::sync::watch::channel(false);
    let drive=tokio::spawn(async move{runtime.drive(&mut owner,Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)).await});
    let action=match case {
      Case::Immediate=>Action::SceneAction{controller,revision:1,scene:"immediate".into(),execute:true},
      Case::Timed=>Action::SceneAction{controller,revision:1,scene:"timed".into(),execute:true},
      Case::ValidMixed=>Action::SceneActions{controller,revision:1,scenes:vec![("immediate".into(),true),("timed".into(),true)]},
      Case::InvalidMixed=>Action::SceneActions{controller,revision:1,scenes:vec![("immediate".into(),true),("timed".into(),true),("absent".into(),true)]},
      Case::NativeMixed=>Action::SceneActions{controller,revision:1,scenes:vec![("immediate".into(),true),("native".into(),true)]},
      Case::Unauthorized=>Action::SceneAction{controller:rejected,revision:1,scene:"immediate".into(),execute:true},
    };
    let result=control.request(action).await;
    let expected=match case {Case::Immediate=>vec![10,40,10],Case::Timed=>vec![60],Case::ValidMixed=>vec![10,40,10,60],Case::NativeMixed=>vec![10,40,10,80],_=>vec![]};
    if result.is_ok() && !expected.is_empty() {
      for _ in 0..300 {
        let n:i64=rusqlite::Connection::open(&db).unwrap().query_row("SELECT count(*) FROM command_journal WHERE substr(intent,2,1)!=X'00' AND status='written'",[],|r|r.get(0)).unwrap();
        if n>=expected.len() as i64{break;}tokio::time::sleep(Duration::from_millis(5)).await;
      }
    } else { tokio::time::sleep(Duration::from_millis(150)).await; }
    stop.send(true).unwrap();drive.await.unwrap().unwrap();
    let sql=rusqlite::Connection::open(&db).unwrap();
    let rows:Vec<(Vec<u8>,String,Option<String>)>=sql.prepare("SELECT j.intent,j.status,c.source FROM command_journal j LEFT JOIN rule_command c ON j.boot_id=c.boot_id AND j.command_id=c.command_id WHERE substr(j.intent,2,1)!=X'00' ORDER BY j.command_id").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().map(Result::unwrap).collect();
    let marker=Store::open(&db).unwrap().logical_variable(1,"marker").unwrap().unwrap().value;
    let native_selection=Store::open(&db).unwrap().logical_variable(1,"native.selection").unwrap().unwrap().value;
    let success=result.is_ok();let result_text=format!("{result:?}");
    let sources=rows.iter().map(|r|r.2.clone()).collect::<Vec<_>>();
    let levels=rows.iter().map(|r|r.0[2]).collect::<Vec<_>>();
    let statuses=rows.iter().map(|r|r.1.clone()).collect::<Vec<_>>();
    let wire=bytes.lock().unwrap().clone();
    assert_eq!(wire.len()%5,0,"memory wire contains partial frames");
    let actual_wire=wire.chunks_exact(5).filter(|f|(1..=8).contains(&f[2])).map(|f|f.to_vec()).collect::<Vec<_>>();
    let expected_wire=expected.iter().enumerate().map(|(i,n)|vec![0xae,0x15,if matches!(case,Case::Timed) || (matches!(case,Case::ValidMixed | Case::NativeMixed) && i==3){2}else{1},*n,0xee]).collect::<Vec<_>>();
    drop(sql);drop(admission);std::fs::remove_dir_all(root).unwrap();
    if expected.is_empty() {
      assert!(!success,"invalid or unauthorized action was accepted: {result_text}");
      assert!(rows.is_empty(),"rejected batch leaked outputs: {rows:?}");assert!(actual_wire.is_empty(),"rejected action leaked memory bus frames");assert!(marker.is_none(),"rejected batch committed a variable");
    } else {
      assert!(success,"reviewed action missing despite valid definition: {result_text}");
      assert_eq!(levels,expected,"ordered targets changed");assert_eq!(actual_wire,expected_wire,"actual memory bus order or values changed");assert!(statuses.iter().all(|s|s=="written"),"planned targets are not actual writes: {statuses:?}");
      if !matches!(case,Case::Timed) {
        assert_eq!(marker,Some(homesense_core::rule::Value::Integer(7)));
        if matches!(case,Case::NativeMixed){assert_eq!(native_selection,Some(homesense_core::rule::Value::Integer(1)),"existing provider selection must commit with the action, not be silently bypassed");}
        for (source,suffix) in sources.iter().zip(["root/first","root/second","root/repeat"]) {assert!(source.as_ref().is_some_and(|s|s.ends_with(suffix)),"source node lost: {source:?}");}
      }
    }
}
#[tokio::test] async fn immediate_condition_variable_and_repeated_targets_reach_actual_wire(){check(Case::Immediate).await;}
#[tokio::test] async fn timed_direct_action_positive_control(){check(Case::Timed).await;}
#[tokio::test] async fn valid_mixed_batch_preserves_immediate_definition_and_timed_execution(){check(Case::ValidMixed).await;}
#[tokio::test] async fn invalid_later_mixed_item_cannot_commit_any_variable_or_output(){check(Case::InvalidMixed).await;}
#[tokio::test] async fn caller_without_admission_authority_cannot_execute_immediate(){check(Case::Unauthorized).await;}

#[tokio::test] async fn reviewed_immediate_and_existing_selection_group_mixed_batch_preserves_both_providers(){check(Case::NativeMixed).await;}
