use hap::server::WriteStatus;
use homesense_core::{Command, DeviceId, ExecutionContext};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    transport_owner::TransportOwner,
    web_auth::{Grant, ModuleOffLease, Role, Tokens},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;
const SECRET: &str = "away-synthetic-test-token-abcdefghijklmnopqrstuvwxyz";
fn documents() -> (Vec<u8>, Vec<u8>, BTreeSet<String>) {
    let mut modules = vec![];
    let mut devices = vec![];
    let mut reviews = vec![];
    let mut ids = BTreeSet::new();
    for (id, address, profile) in [
        ("relay", "27", "r8i8_din_pro_v2_7_7"),
        ("dimmer", "38", "ad8_din_rs485_v2_2_1"),
    ] {
        modules.push(json!({"id":id,"address":address,"profile":profile,"verification":{"status":"simulated"}}));
        let mut names = vec![];
        for channel in 1..=8 {
            let name = format!("{id}:{channel}");
            ids.insert(name.clone());
            names.push(name.clone());
            devices.push(json!({"id":name,"name":"Synthetic light","module_id":id,"channel":channel,"enabled":true}));
        }
        reviews.push(json!({"module_id":id,"devices":names,"review_evidence":"All synthetic eight ports are lighting"}));
    }
    modules.push(json!({"id":"utility","address":"49","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    devices.push(json!({"id":"pump","name":"Synthetic non-lighting load","module_id":"utility","channel":1,"enabled":true}));
    ids.insert("pump".into());
    modules.push(json!({"id":"motor","address":"4A","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    devices.push(json!({"id":"curtain","name":"Synthetic curtain","module_id":"motor","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"Synthetic only","verified_at_unix_ms":1}}));
    ids.insert("curtain".into());
    let devices = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices});
    let rules = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"review_evidence":"Explicit synthetic Away policy, no original scene approval","active_execution_policy":"cancel_affected_lighting","modules":reviews}});
    (
        serde_json::to_vec(&devices).unwrap(),
        serde_json::to_vec(&rules).unwrap(),
        ids,
    )
}
struct Database(PathBuf);
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
        let _ = std::fs::remove_file(self.0.with_extension("runtime.lock"));
    }
}
async fn fixture(omit: bool) -> (Database, Admission, Arc<Tokens>, Uuid, ModuleOffLease) {
    let (path, rules, mut ids) = documents();
    let db = Database(std::env::temp_dir().join(format!("away-runtime-{}.sqlite", Uuid::new_v4())));
    Store::open(&db.0)
        .unwrap()
        .publish_automation(None, &path, &rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let credential = Uuid::new_v4();
    if omit {
        ids.remove("relay:8");
    }
    let tokens = Arc::new(Tokens::default());
    tokens
        .insert(
            credential,
            SECRET,
            Grant {
                controller,
                role: Role::Member,
                devices: ids,
                scenes: BTreeSet::new(),
            },
        )
        .unwrap();
    let lease = tokens
        .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
        .unwrap();
    let actor = Admission::open(
        db.0.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    (db, actor, tokens, credential, lease)
}
#[derive(Clone)]
struct Bus {
    bytes: Arc<Mutex<Vec<u8>>>,
    limit: usize,
}
impl Default for Bus {
    fn default() -> Self {
        Self {
            bytes: Default::default(),
            limit: usize::MAX,
        }
    }
}
impl Write for Bus {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let mut bytes = self.bytes.lock().unwrap();
        let n = b.len().min(self.limit.saturating_sub(bytes.len()));
        if n == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        bytes.extend_from_slice(&b[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Read for Bus {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
fn owner(actor: &Admission, bus: Bus) -> TransportOwner<Bus> {
    TransportOwner::new(
        actor.clone(),
        BTreeMap::from([(
            "bus".into(),
            hb_transport::Connection::for_simulation(bus, 1),
        )]),
    )
}
fn off(controller: Uuid) -> Batch {
    Batch {
        controller,
        intents: vec![Intent {
            device: DeviceId::new("relay:8").unwrap(),
            command: Command::Switch(false),
        }],
        subscriptions: vec![],
    }
}
#[tokio::test]
async fn explicit_runtime_entry_writes_two_frames_and_sixteen_durable_effects() {
    let (db, actor, _tokens, _credential, lease) = fixture(false).await;
    let bus = Bus::default();
    let mut owner = owner(&actor, bus.clone());
    let ids = actor.submit_away_off(1, lease, true).await.unwrap();
    assert_eq!(ids.len(), 2);
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE intent=x'01080000' AND status='accepted'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        actor
            .submit(off(_tokens.summaries().unwrap()[0].grant.controller))
            .await
            .unwrap_err(),
        WriteStatus::ResourceBusy
    );
    let event=owner.step().await.unwrap().unwrap();event.result.unwrap();assert_eq!(event.physical_frames,2);
    assert_eq!(event.completed.iter().map(|(id,_)|*id).collect::<Vec<_>>(),ids);
    let store=Store::open(&db.0).unwrap();for id in &ids{assert_eq!(store.command_status(actor.boot_id(),*id).unwrap(),Some(Status::Written));}drop(store);
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(
        *bus.bytes.lock().unwrap(),
        vec![0xAE, 0x38, 0, 0, 0xEE, 0xAE, 0x27, 0, 0xF2, 0xEE]
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE status='written'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE status='observed'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let store = Store::open(&db.0).unwrap();
    let history = store
        .recent_device_commands(
            actor.boot_id(),
            1,
            &[
                DeviceId::new("dimmer:8").unwrap(),
                DeviceId::new("relay:8").unwrap(),
            ],
            8,
        )
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|r| r.intent == [1, 8, 0, 0] && r.status == "written")
    );
    assert_eq!(
        history
            .iter()
            .map(|r| r.device.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["dimmer:8", "relay:8"])
    );
    actor
        .submit(off(_tokens.summaries().unwrap()[0].grant.controller))
        .await
        .unwrap();
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn partial_principal_cannot_accept_any_module_or_create_effect_rows() {
    let (db, actor, _tokens, _credential, lease) = fixture(true).await;
    assert!(actor.submit_away_off(1, lease, true).await.is_err());
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn credential_revocation_before_dispatch_cancels_without_output() {
    let (db, actor, tokens, credential, lease) = fixture(false).await;
    let ids = actor.submit_away_off(1, lease, true).await.unwrap();
    tokens.revoke(credential).unwrap();
    let bus = Bus::default();
    let mut owner = owner(&actor, bus.clone());
    assert!(owner.step().await.unwrap().is_none());
    assert!(bus.bytes.lock().unwrap().is_empty());
    let store = Store::open(&db.0).unwrap();
    for id in ids {
        assert_eq!(
            store.command_status(actor.boot_id(), id).unwrap(),
            Some(Status::Cancelled)
        );
    }
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn incomplete_module_frame_is_uncertain_and_never_replayed() {
    let (db, actor, _tokens, _credential, lease) = fixture(false).await;
    let ids = actor.submit_away_off(1, lease, true).await.unwrap();
    let bus = Bus {
        limit: 2,
        ..Default::default()
    };
    let mut owner = owner(&actor, bus.clone());
    assert_eq!(
        owner
            .step()
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap_err()
            .bytes_written,
        2
    );
    assert!(owner.step().await.is_err());
    assert_eq!(bus.bytes.lock().unwrap().len(), 2);
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), ids[0]).unwrap(),
        Some(Status::Uncertain)
    );
    actor.shutdown().await.unwrap();
    let actor = Admission::open(
        db.0.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn publication_barrier_cancels_queued_modules_without_writing() {
    let (db, actor, _tokens, _credential, lease) = fixture(false).await;
    let ids = actor.submit_away_off(1, lease, true).await.unwrap();
    let (devices, rules, _) = documents();
    let mut dev: Value = serde_json::from_slice(&devices).unwrap();
    dev["revision"] = 2.into();
    let mut doc: Value = serde_json::from_slice(&rules).unwrap();
    doc["configuration_revision"] = 2.into();
    actor
        .begin_automation_publication(
            serde_json::to_vec(&dev).unwrap(),
            serde_json::to_vec(&doc).unwrap(),
            1,
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
    let bus = Bus::default();
    let mut owner = owner(&actor, bus.clone());
    assert!(owner.step().await.unwrap().is_none());
    assert!(bus.bytes.lock().unwrap().is_empty());
    let store = Store::open(&db.0).unwrap();
    for id in ids {
        assert_eq!(
            store.command_status(actor.boot_id(), id).unwrap(),
            Some(Status::Cancelled)
        );
    }
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn away_atomically_supersedes_covered_queue_and_preserves_unrelated_motor_stop() {
    let (db, actor, tokens, _credential, lease) = fixture(false).await;
    let controller = tokens.summaries().unwrap()[0].grant.controller;
    let old = actor.submit(off(controller)).await.unwrap()[0];
    let stop = actor
        .submit_motor(
            controller,
            DeviceId::new("curtain").unwrap(),
            homesense_core::MotorAction::Stop,
        )
        .await
        .unwrap()[0];
    let away = actor.submit_away_off(1, lease, true).await.unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), old).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        store.command_status(actor.boot_id(), stop).unwrap(),
        Some(Status::Accepted)
    );
    let bus = Bus::default();
    let mut owner = owner(&actor, bus.clone());
    let first = owner.step().await.unwrap().unwrap();
    assert_eq!(first.command_id, stop);
    first.result.unwrap();
    let event=owner.step().await.unwrap().unwrap();event.result.unwrap();assert_eq!(event.completed.iter().map(|(id,_)|*id).collect::<Vec<_>>(),away);
    for id in &away{assert_eq!(store.command_status(actor.boot_id(),*id).unwrap(),Some(Status::Written));}
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(
        *bus.bytes.lock().unwrap(),
        vec![
            0xAE, 0x4A, 1, 0xF0, 0xEE, 0xAE, 0x38, 0, 0, 0xEE, 0xAE, 0x27, 0, 0xF2, 0xEE
        ]
    );
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn old_delayed_lamp_is_cancelled_while_other_load_in_same_scene_continues() {
    let (db, actor, tokens, _credential, lease) = fixture(false).await;
    let controller = tokens.summaries().unwrap()[0].grant.controller;
    let now = 0;
    let scene_clock = std::time::Instant::now();
    let steps = vec![
        homesense_core::scene::Step {
            device: DeviceId::new("relay:8").unwrap(),
            command: Command::Switch(true),
            delay_ms: 25,
            source: "old lighting node".into(),
        },
        homesense_core::scene::Step {
            device: DeviceId::new("pump").unwrap(),
            command: Command::Switch(true),
            delay_ms: 50,
            source: "unrelated utility node".into(),
        },
    ];
    let mut scene =
        homesensed::scene_runtime::SceneRuntime::new(actor.clone(), controller, steps, now, 1000)
            .await
            .unwrap();
    actor.submit_away_off(1, lease, true).await.unwrap();
    let bus = Bus::default();
    let mut owner = owner(&actor, bus.clone());
    owner.step().await.unwrap();
    owner.step().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(
        scene
            .poll(scene_clock.elapsed().as_millis() as u64)
            .await
            .unwrap()
            .is_none()
    );
    let ids = scene
        .poll(scene_clock.elapsed().as_millis() as u64)
        .await
        .unwrap()
        .unwrap();
    let write = owner.step().await.unwrap().unwrap();
    assert_eq!(write.command_id, ids[0]);
    scene.written(write.command_id, true).await.unwrap();
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(
        *bus.bytes.lock().unwrap(),
        vec![
            0xAE, 0x38, 0, 0, 0xEE, 0xAE, 0x27, 0, 0xF2, 0xEE, 0xAE, 0x49, 1, 0xF1, 0xEE
        ]
    );
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_cancellation WHERE device_id='relay:8' AND reason='manual_override'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    actor.shutdown().await.unwrap();
}

#[test]
fn enabled_away_policy_needs_complete_known_reviewed_module_coverage() {
    let (devices, rules, _) = documents();
    let config =
        homesense_config::Configuration::parse(&devices, ExecutionContext::Simulation).unwrap();
    for field in ["coverage", "evidence", "module"] {
        let mut rules: Value = serde_json::from_slice(&rules).unwrap();
        match field {
            "coverage" => {
                rules["away_off"]["modules"][0]["devices"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            "evidence" => {
                rules["away_off"]["review_evidence"] = "".into();
            }
            _ => {
                rules["away_off"]["modules"][0]["module_id"] = "missing".into();
            }
        }
        assert!(
            homesense_config::rule_document::Document::parse(
                &serde_json::to_vec(&rules).unwrap(),
                &config
            )
            .is_err(),
            "{field}"
        );
    }
}
#[tokio::test]
async fn module_scope_audit_failure_rolls_back_acceptance_and_outputs_nothing() {
    let (db, actor, _tokens, _credential, lease) = fixture(false).await;
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER synthetic_module_audit_failure BEFORE INSERT ON module_off_effect BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;").unwrap();
    assert!(actor.submit_away_off(1, lease, true).await.is_err());
    assert_eq!(
        sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(actor.dispatch().await.is_err());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_lights_accept_full_logical_scope_without_exposing_supplies() {
 let (bytes,rules,_) = documents();
 let mut value:Value=serde_json::from_slice(&bytes).unwrap();
 for d in value["devices"].as_array_mut().unwrap() {
  if d["module_id"]=="dimmer" {d["default_on_level"]=json!(75);d["supply"]=json!({"module_id":"relay","channel":d["channel"].as_u64().unwrap()});}
 }
 let config=serde_json::to_vec(&value).unwrap();
 homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();
 let db=Database(std::env::temp_dir().join(format!("root-combined-away-{}.sqlite",Uuid::new_v4())));
 Store::open(&db.0).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
 let controller=Uuid::new_v4();
 let tokens=Arc::new(Tokens::default());
 let grant=Grant{controller,role:Role::Member,devices:(1..=8).map(|n|format!("dimmer:{n}")).collect(),scenes:BTreeSet::new()};
 for n in 1..=8 {assert!(grant.may_control_device(&format!("dimmer:{n}")));assert!(!grant.may_control_device(&format!("relay:{n}")));}
 tokens.insert(Uuid::new_v4(),SECRET,grant).unwrap();
 let lease=tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap();
 let actor=Admission::open(db.0.clone(),Options{context:ExecutionContext::Simulation,..Options::default()},move|id|id==controller).await.unwrap();
 let result=actor.submit_away_off(1,lease,true).await;
 actor.shutdown().await.unwrap();
 assert!(result.is_ok(),"all eight visible combined lights are authorized, hidden supplies should inherit their full owner scope: {result:?}");
 assert_eq!(result.unwrap().len(),2);
}

#[tokio::test]
async fn reject_raw_supplies_without_their_logical_light_owners() {
 let (bytes,rules,_) = documents();
 let mut value:Value=serde_json::from_slice(&bytes).unwrap();
 for d in value["devices"].as_array_mut().unwrap() {
  if d["module_id"]=="dimmer" {d["default_on_level"]=json!(75);d["supply"]=json!({"module_id":"relay","channel":d["channel"].as_u64().unwrap()});}
 }
 let config=serde_json::to_vec(&value).unwrap();
 homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();
 let mut rules:Value=serde_json::from_slice(&rules).unwrap();
 rules["away_off"]["modules"].as_array_mut().unwrap().retain(|m|m["module_id"]=="relay");
 let rules=serde_json::to_vec(&rules).unwrap();
 let db=Database(std::env::temp_dir().join(format!("root-hidden-away-{}.sqlite",Uuid::new_v4())));
 Store::open(&db.0).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
 let controller=Uuid::new_v4();let tokens=Arc::new(Tokens::default());
 let grant=Grant{controller,role:Role::Member,devices:(1..=8).map(|n|format!("relay:{n}")).collect(),scenes:BTreeSet::new()};
 for n in 1..=8 {assert!(!grant.may_control_device(&format!("dimmer:{n}")));}
 tokens.insert(Uuid::new_v4(),SECRET,grant).unwrap();
 let lease=tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap();
 let actor=Admission::open(db.0.clone(),Options{context:ExecutionContext::Simulation,..Options::default()},move|id|id==controller).await.unwrap();
 let result=actor.submit_away_off(1,lease,true).await;
 actor.shutdown().await.unwrap();
 assert_eq!(result,Err(WriteStatus::InsufficientPrivileges),"a raw supply binding must not grant control of ungranted logical owner lights");
}

#[tokio::test]
async fn ninety_six_lighting_ports_emit_twelve_module_frames() {
 let mut modules=vec![];let mut devices=vec![];let mut reviews=vec![];let mut all=BTreeSet::new();let mut expected=vec![];
 for address in (1u8..=7).chain(0x11..=0x15) {
  let module=format!("module:{address:02X}");let relay=address<0x10;
  modules.push(json!({"id":module,"address":format!("{address:02X}"),"profile":if relay {"r8i8_din_pro_v2_7_7"} else {"ad8_din_rs485_v2_2_1"},"verification":{"status":"simulated"}}));
  let mut names=vec![];
  for channel in 1..=8 {let id=format!("lamp:{address:02X}:{channel}");all.insert(id.clone());names.push(id.clone());devices.push(json!({"id":id,"name":"Synthetic light","module_id":module,"channel":channel,"enabled":true}));}
  reviews.push(json!({"module_id":module,"devices":names,"review_evidence":"All8 synthetic physical ports lighting"}));
  expected.extend([0xAE,address,0,if relay {0xF2}else{0},0xEE]);
 }
 let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
 let rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"review_evidence":"Synthetic 96-port Away operation","active_execution_policy":"cancel_affected_lighting","modules":reviews}})).unwrap();
 let db=Database(std::env::temp_dir().join(format!("away96-{}.sqlite",Uuid::new_v4())));
 Store::open(&db.0).unwrap().publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
 let controller=Uuid::new_v4();let tokens=Arc::new(Tokens::default());tokens.insert(Uuid::new_v4(),SECRET,Grant{controller,role:Role::Member,devices:all,scenes:BTreeSet::new()}).unwrap();
 let lease=tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap();
 let actor=Admission::open(db.0.clone(),Options{context:ExecutionContext::Simulation,..Options::default()},move|id|id==controller).await.unwrap();
 let bus=Bus::default();let mut owner=owner(&actor,bus.clone());let ids=actor.submit_away_off(1,lease,true).await.unwrap();assert_eq!(ids.len(),12);
 let result=owner.step().await.unwrap().unwrap();result.result.unwrap();assert_eq!(result.physical_frames,12);assert_eq!(result.completed.iter().map(|(id,_)|*id).collect::<Vec<_>>(),ids);let store=Store::open(&db.0).unwrap();for id in &ids{assert_eq!(store.command_status(actor.boot_id(),*id).unwrap(),Some(Status::Written));}drop(store);
 assert!(owner.step().await.unwrap().is_none());assert_eq!(*bus.bytes.lock().unwrap(),expected);assert_eq!(expected.len(),60);
 let sql=rusqlite::Connection::open(&db.0).unwrap();
 assert_eq!(sql.query_row("SELECT count(*) FROM module_off_effect",[],|r|r.get::<_,i64>(0)).unwrap(),96);
 assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written' AND intent=x'01080000'",[],|r|r.get::<_,i64>(0)).unwrap(),12);
 assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='observed'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
 actor.shutdown().await.unwrap();
}
