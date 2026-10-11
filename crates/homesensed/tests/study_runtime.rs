use homesense_core::{ExecutionContext, queue::Origin};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    study_runtime::{StudyRuntime, TimingPolicy},
};
use std::path::PathBuf;
use uuid::Uuid;
struct Db(PathBuf);
impl Drop for Db {
    fn drop(&mut self) {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", self.0.display()));
        }
        let _ = std::fs::remove_file(self.0.with_extension("runtime.lock"));
    }
}
fn database() -> Db {
    let mut d: hap::serde_json::Value = hap::serde_json::from_slice(include_bytes!(
        "../../../config/import/study-lights.draft.json"
    ))
    .unwrap();
    let c = &mut d["configuration"];
    for dev in c["devices"].as_array_mut().unwrap() {
        dev["enabled"] = true.into();
    }
    for m in c["gateways"][0]["modules"].as_array_mut().unwrap() {
        m["verification"] = hap::serde_json::json!({"status":"verified","evidence":"isolated in-memory test fixture only","verified_at_unix_ms":1});
    }
    c["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"verified","evidence":"isolated in-memory test fixture only","verified_at_unix_ms":1},"led_mode":"manual"}));
    for key in [1, 3, 5] {
        c["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":format!("control4:63:key:{key}"),"name":"fixture","gateway":"hopebeauty","address":"41","channel":key,"enabled":true}));
    }
    let db = Db(std::env::temp_dir().join(format!("study-{}.sqlite", Uuid::new_v4())));
    Store::open(&db.0)
        .unwrap()
        .publish_configuration(
            None,
            &hap::serde_json::to_vec(c).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    db
}
async fn actor(db: &Db, id: Uuid) -> Admission {
    let a = Admission::open(
        db.0.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |v| v == id,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    a
}
fn runtime(a: &Admission, id: Uuid) -> StudyRuntime {
    StudyRuntime::new(
        a.clone(),
        id,
        include_bytes!("../../../config/scenes/study.json"),
        TimingPolicy::ModuleNative,
    )
    .unwrap()
}
#[tokio::test]
async fn rule_origin_cancel_persistence_and_no_restart_replay() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    assert!(scene.toggle(1).await.is_err());
    let first = scene.activate(1).await.unwrap();
    assert_eq!(first.command_ids.len(), 7);
    assert_eq!(scene.mode().await.unwrap(), Some(1));
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin, Origin::Rule);
    assert_eq!(ticket.command.run_id, Some(first.command_ids[0]));
    assert_eq!(scene.cancel().await.unwrap(), 6);
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store
            .command_status(a.boot_id(), first.command_ids[1])
            .unwrap(),
        Some(Status::Cancelled)
    );
    let off = scene.toggle(1).await.unwrap();
    assert_eq!(off.mode, 0);
    let boot = a.boot_id().to_owned();
    a.shutdown().await.unwrap();
    let b = actor(&db, id).await;
    assert_eq!(b.study_mode().await.unwrap(), Some(0));
    assert!(b.dispatch().await.unwrap().is_none());
    assert_eq!(
        store.command_status(&boot, off.command_ids[0]).unwrap(),
        Some(Status::Uncertain)
    );
    b.shutdown().await.unwrap();
}
#[tokio::test]
async fn scene_acceptance_failure_rolls_back_mode_and_commands() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_mode BEFORE INSERT ON study_mode BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(runtime(&a, id).activate(3).await.is_err());
    assert_eq!(Store::open(&db.0).unwrap().study_mode(1).unwrap(), None);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn replacement_cancels_old_run_and_denies_other_principal() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    assert!(runtime(&a, Uuid::new_v4()).activate(1).await.is_err());
    let mut scene = runtime(&a, id);
    let old = scene.activate(1).await.unwrap();
    let new = scene.activate(5).await.unwrap();
    assert_eq!(new.mode, 3);
    let store = Store::open(&db.0).unwrap();
    for command in old.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), command).unwrap(),
            Some(Status::Cancelled)
        );
    }
    for _ in 0..7 {
        let ticket = a.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.run_id, Some(new.command_ids[0]));
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn direct_manual_control_cancels_only_that_devices_pending_scene_action() {
    use homesense_core::{Command, DeviceId};
    use homesense_hap::requests::{Batch, Intent};
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    let run = scene.activate(1).await.unwrap();
    a.submit(Batch {
        controller: id,
        intents: vec![Intent {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(false),
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store
            .command_status(a.boot_id(), run.command_ids[0])
            .unwrap(),
        Some(Status::Cancelled)
    );
    let manual = a.dispatch().await.unwrap().unwrap();
    assert_eq!(manual.command.origin, Origin::Hap);
    a.finish_write(manual.command.id, true).await.unwrap();
    let mut remaining = 0;
    while let Some(ticket) = a.dispatch().await.unwrap() {
        assert_ne!(ticket.command.device_id.as_str(), "control4:210");
        remaining += 1;
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert_eq!(remaining, 6);
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn energy_scene_executes_through_transport_owner_in_source_order() {
    use std::{
        io::{Read, Write},
        sync::{Arc, Mutex},
    };
    struct Wire(Arc<Mutex<Vec<u8>>>);
    impl Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::TimedOut.into())
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let db = database();
    let id = Uuid::new_v4();
    let a = Admission::open(db.0.clone(), Options::default(), move |v| v == id)
        .await
        .unwrap();
    a.synchronized(1, 1).await.unwrap();
    let mut scene = runtime(&a, id);
    let run = scene.activate(5).await.unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        a.clone(),
        [(
            "hopebeauty".into(),
            hb_transport::Connection::new(Wire(bytes.clone()), 1),
        )]
        .into(),
    );
    for command in &run.command_ids {
        let out = owner.step().await.unwrap().unwrap();
        assert_eq!(*command, out.command_id);
        out.result.unwrap();
    }
    assert_eq!(
        *bytes.lock().unwrap(),
        vec![
            0xae, 0x41, 5, 0xf1, 0xee, 0xae, 0x41, 1, 0xf2, 0xee, 0xae, 0x41, 3, 0xf2, 0xee, 0xae,
            3, 4, 0xf2, 0xee, 0xae, 7, 6, 0xf1, 0xee, 0xae, 0x11, 0xb5, 17, 0xee, 0xae, 0x12, 4, 0,
            0xee
        ]
    );
    let store = Store::open(&db.0).unwrap();
    for command in run.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), command).unwrap(),
            Some(Status::Written)
        );
    }
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn failed_cancellation_keeps_pending_run_and_audit_consistent() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    let run = scene.activate(3).await.unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE OF status ON command_journal WHEN NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(scene.cancel().await.is_err());
    let store = Store::open(&db.0).unwrap();
    for command in &run.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), *command).unwrap(),
            Some(Status::Accepted)
        );
    }
    sql.execute_batch("DROP TRIGGER reject_cancel;").unwrap();
    assert_eq!(scene.cancel().await.unwrap(), 7);
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn every_repeated_scene_turns_off_and_pre_dispatch_cancel_writes_nothing() {
    use homesense_core::Command;
    for key in [1, 3, 5] {
        let db = database();
        let id = Uuid::new_v4();
        let a = actor(&db, id).await;
        let mut scene = runtime(&a, id);
        scene.activate(key).await.unwrap();
        assert_eq!(scene.cancel().await.unwrap(), 7);
        assert!(a.dispatch().await.unwrap().is_none());
        // Cancellation retains local selection; a repeated key deactivates it.
        let off = scene.toggle(key).await.unwrap();
        assert_eq!(off.mode, 0);
        assert_eq!(off.command_ids.len(), 5);
        for expected in off.command_ids {
            let ticket = a.dispatch().await.unwrap().unwrap();
            assert_eq!(ticket.command.id, expected);
            match ticket.command.command {
                Command::Switch(on) | Command::PanelLed(on) => assert!(!on),
                Command::SetLevel { level, .. } => assert_eq!(level.get(), 0),
                other => panic!("unexpected off command: {other:?}"),
            }
            a.finish_write(expected, true).await.unwrap();
        }
        assert!(a.dispatch().await.unwrap().is_none());
        assert_eq!(scene.mode().await.unwrap(), Some(0));
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn scene_activity_requires_fresh_complete_observed_targets() {
    use homesense_core::{DeviceId, ModuleKind, Observation, Percent};
    use homesense_state::{Cursor, DeviceState, Quality, Snapshot};
    use homesensed::study_runtime::SceneActivity;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let scene = runtime(&a, id);
    let mut snapshot = Snapshot {
        cursor: Cursor {
            boot_id: 1,
            sequence: 0,
        },
        state_version: 0,
        devices: Default::default(),
    };
    assert_eq!(
        scene.activity("8", &snapshot, 100, 50).unwrap(),
        SceneActivity::Unknown
    );
    for (name, observed, kind) in [
        (
            "control4:210",
            Observation::Switch(false),
            ModuleKind::Switch,
        ),
        (
            "control4:278",
            Observation::Switch(true),
            ModuleKind::Switch,
        ),
        (
            "control4:308",
            Observation::Level(Percent::new(17).unwrap()),
            ModuleKind::Dimmer,
        ),
        (
            "control4:336",
            Observation::Level(Percent::new(0).unwrap()),
            ModuleKind::Dimmer,
        ),
    ] {
        snapshot.devices.insert(
            DeviceId::new(name).unwrap(),
            DeviceState {
                kind,
                desired: None,
                observed: Some(observed),
                quality: Quality::Confirmed,
                observed_at_ms: Some(90),
                last_command_id: None,
                pending_command: None,
                version: 1,
            },
        );
    }
    assert_eq!(
        scene.activity("8", &snapshot, 100, 50).unwrap(),
        SceneActivity::Active
    );
    let dev = DeviceId::new("control4:308").unwrap();
    snapshot.devices.get_mut(&dev).unwrap().pending_command = Some(1);
    assert_eq!(
        scene.activity("8", &snapshot, 100, 50).unwrap(),
        SceneActivity::Pending
    );
    snapshot.devices.get_mut(&dev).unwrap().pending_command = None;
    snapshot.devices.get_mut(&dev).unwrap().observed =
        Some(Observation::Level(Percent::new(18).unwrap()));
    assert_eq!(
        scene.activity("8", &snapshot, 100, 50).unwrap(),
        SceneActivity::Inactive
    );
    assert_eq!(
        scene.activity("8", &snapshot, 140, 50).unwrap(),
        SceneActivity::Unknown
    );
    assert_eq!(
        scene.activity("8", &snapshot, 80, 50).unwrap(),
        SceneActivity::Unknown
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeline_admission_is_rule_origin_and_cancels_queued_and_future_work() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let steps = vec![
        Step {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(true),
            delay_ms: 0,
            source: "first".into(),
        },
        Step {
            device: DeviceId::new("control4:278").unwrap(),
            command: Command::Switch(true),
            delay_ms: 100,
            source: "later".into(),
        },
    ];
    let mut scene = SceneRuntime::new(a.clone(), id, steps.clone(), 0, 100)
        .await
        .unwrap();
    let ids = scene.poll(0).await.unwrap().unwrap();
    assert_eq!(scene.cancel().await.unwrap(), 2);
    assert!(a.dispatch().await.unwrap().is_none());
    assert_eq!(scene.status(), Status::Cancelled);
    assert!(scene.poll(100).await.unwrap().is_none());
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(a.boot_id(), ids[0])
            .unwrap(),
        Some(homesense_storage::journal::Status::Cancelled)
    );
    let mut scene = SceneRuntime::new(a.clone(), id, steps, 200, 100)
        .await
        .unwrap();
    let ids = scene.poll(200).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin, Origin::Rule);
    assert_eq!(ticket.command.id, ids[0]);
    a.finish_write(ids[0], true).await.unwrap();
    scene.written(ids[0], true).await.unwrap();
    assert!(scene.poll(299).await.unwrap().is_none());
    assert_eq!(scene.cancel().await.unwrap(), 1);
    assert_eq!(scene.status(), Status::Partial);
    assert!(scene.poll(300).await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn delayed_scene_rejects_old_generation_after_reconnect() {
    use homesense_core::{Command, DeviceId, scene::Step};
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let steps = vec![Step {
        device: DeviceId::new("control4:210").unwrap(),
        command: Command::Switch(true),
        delay_ms: 100,
        source: "delayed".into(),
    }];
    let mut scene = SceneRuntime::new(a.clone(), id, steps, 0, 100)
        .await
        .unwrap();
    a.transport_failed().await.unwrap();
    assert!(scene.poll(100).await.is_err());
    a.recover_synchronized(2, 1).await.unwrap();
    assert!(scene.poll(101).await.is_err());
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_scene_cancels_remaining_output_after_failure_or_partial_cancel() {
    use homesense_core::{
        Command, DeviceId, LevelTransition, Percent,
        scene::{Status, Step},
    };
    use homesensed::scene_runtime::SceneRuntime;
    for first_succeeds in [false, true] {
        let db = database();
        let mut store = Store::open(&db.0).unwrap();
        let mut config: hap::serde_json::Value =
            hap::serde_json::from_slice(&store.active().unwrap().unwrap().payload).unwrap();
        config["revision"] = 2.into();
        let light = config["devices"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|d| d["id"] == "control4:336")
            .unwrap();
        light["supply_relay"] = "control4:210".into();
        light["default_on_level"] = 100.into();
        store
            .publish_configuration(
                Some(1),
                &hap::serde_json::to_vec(&config).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        drop(store);
        let id = Uuid::new_v4();
        let a = Admission::open(
            db.0.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            move |v| v == id,
        )
        .await
        .unwrap();
        a.synchronized(1, 2).await.unwrap();
        let mut scene = SceneRuntime::new(
            a.clone(),
            id,
            vec![Step {
                device: DeviceId::new("control4:336").unwrap(),
                command: Command::SetLevel {
                    level: Percent::new(50).unwrap(),
                    transition: LevelTransition::Immediate,
                },
                delay_ms: 0,
                source: "combined".into(),
            }],
            0,
            100,
        )
        .await
        .unwrap();
        let ids = scene.poll(0).await.unwrap().unwrap();
        assert_eq!(ids.len(), 2);
        let ticket = a.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.id, ids[0]);
        a.finish_write(ids[0], first_succeeds).await.unwrap();
        scene.written(ids[0], first_succeeds).await.unwrap();
        if first_succeeds {
            assert_eq!(scene.cancel().await.unwrap(), 1);
        }
        if first_succeeds {
            assert!(a.dispatch().await.unwrap().is_none());
        } else {
            assert!(a.dispatch().await.is_err());
        }
        assert_eq!(
            scene.status(),
            if first_succeeds {
                Status::Partial
            } else {
                Status::Failed
            }
        );
        assert_eq!(
            Store::open(&db.0)
                .unwrap()
                .command_status(a.boot_id(), ids[1])
                .unwrap(),
            Some(homesense_storage::journal::Status::Cancelled)
        );
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn logical_variable_admission_is_authorized_generation_scoped_and_atomic() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Value, ValueType},
    };
    use homesense_hap::requests::{Batch, Intent};
    use homesense_storage::variables::Update;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let generation = a.task_generation().await.unwrap();
    assert!(
        a.logical_variable(
            Uuid::new_v4(),
            generation.clone(),
            "logic".into(),
            Some(ValueType::Integer)
        )
        .await
        .is_err()
    );
    a.logical_variable(
        id,
        generation.clone(),
        "logic".into(),
        Some(ValueType::Integer),
    )
    .await
    .unwrap();
    let batch = || Batch {
        controller: id,
        intents: vec![Intent {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(true),
        }],
        subscriptions: vec![],
    };
    let update = || Update {
        revision: 1,
        name: "logic".into(),
        expected_version: 0,
        value: Some(Value::Integer(1)),
    };
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_logic BEFORE UPDATE ON logical_variable BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        a.submit_rule_update(generation.clone(), batch(), vec![update()])
            .await
            .is_err()
    );
    assert!(a.dispatch().await.is_err());
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "logic")
            .unwrap()
            .unwrap()
            .value,
        None
    );
    a.shutdown().await.unwrap();
    sql.execute_batch("DROP TRIGGER fail_logic;").unwrap();
    let a = actor(&db, id).await;
    let generation = a.task_generation().await.unwrap();
    let ids = a
        .submit_rule_update(generation.clone(), batch(), vec![update()])
        .await
        .unwrap();
    assert_eq!(
        a.logical_variable(id, generation.clone(), "logic".into(), None)
            .await
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(1))
    );
    assert!(
        a.submit_rule_update(generation.clone(), batch(), vec![update()])
            .await
            .is_err()
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, ids[0]);
    assert_eq!(ticket.command.origin, Origin::Rule);
    a.finish_write(ids[0], true).await.unwrap();
    let vars = vec![Update {
        expected_version: 1,
        value: Some(Value::Integer(2)),
        ..update()
    }];
    let empty = Batch {
        controller: id,
        intents: vec![],
        subscriptions: vec![],
    };
    assert!(
        a.submit_rule_update(generation.clone(), empty, vars)
            .await
            .unwrap()
            .is_empty()
    );
    a.transport_failed().await.unwrap();
    a.recover_synchronized(2, 1).await.unwrap();
    assert!(
        a.logical_variable(id, generation.clone(), "logic".into(), None)
            .await
            .is_err()
    );
    assert!(
        a.submit_rule_update(generation, batch(), vec![update()])
            .await
            .is_err()
    );
    let current = a.task_generation().await.unwrap();
    assert_eq!(
        a.logical_variable(id, current, "logic".into(), None)
            .await
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(2))
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn prepared_ast_checks_condition_read_set_before_atomic_commit() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Condition, Expr, Node, Value, ValueType},
    };
    use homesense_hap::requests::Batch;
    use homesense_storage::variables::Update;
    use homesensed::rule_runtime::PreparedRule;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let g = a.task_generation().await.unwrap();
    a.logical_variable(id, g.clone(), "enabled".into(), Some(ValueType::Bool))
        .await
        .unwrap();
    let nodes = vec![Node {
        source: "condition".into(),
        action: Action::If {
            condition: Condition::Equal(
                Expr::Variable("enabled".into()),
                Expr::Literal(Value::Bool(true)),
            ),
            then_actions: vec![Node {
                source: "on".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            }],
            else_actions: vec![],
        },
    }];
    let p = PreparedRule::prepare(&a, id, vec!["enabled".into()], &nodes, Budget::default())
        .await
        .unwrap();
    assert_eq!(p.unknown_branches, vec!["condition"]);
    assert!(p.commit(&a).await.unwrap().is_empty());
    assert!(a.dispatch().await.unwrap().is_none());
    a.submit_rule_update(
        g.clone(),
        Batch {
            controller: id,
            intents: vec![],
            subscriptions: vec![],
        },
        vec![Update {
            revision: 1,
            name: "enabled".into(),
            expected_version: 0,
            value: Some(Value::Bool(true)),
        }],
    )
    .await
    .unwrap();
    let first = PreparedRule::prepare(&a, id, vec!["enabled".into()], &nodes, Budget::default())
        .await
        .unwrap();
    let stale = PreparedRule::prepare(&a, id, vec!["enabled".into()], &nodes, Budget::default())
        .await
        .unwrap();
    let ids = first.commit(&a).await.unwrap();
    assert_eq!(ids.len(), 1);
    assert!(stale.commit(&a).await.is_err());
    let t = a.dispatch().await.unwrap().unwrap();
    assert_eq!(t.command.id, ids[0]);
    a.finish_write(ids[0], true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn reviewed_scene_expansion_reaches_durable_rule_queue() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Node},
        scene_expand::Definition,
    };
    use homesensed::rule_runtime::PreparedRule;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let scenes = [(
        "reviewed".into(),
        Definition {
            activate: vec![Node {
                source: "member".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            }],
            deactivate: None,
        },
    )]
    .into();
    let nodes = [Node {
        source: "event".into(),
        action: Action::Scene {
            id: "reviewed".into(),
            activate: true,
        },
    }];
    let p = PreparedRule::prepare_with_scenes(&a, id, vec![], &nodes, &scenes, Budget::default())
        .await
        .unwrap();
    let ids = p.commit(&a).await.unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, ids[0]);
    assert_eq!(ticket.command.origin, Origin::Rule);
    a.finish_write(ids[0], true).await.unwrap();
    let off = [Node {
        source: "event".into(),
        action: Action::Scene {
            id: "reviewed".into(),
            activate: false,
        },
    }];
    assert!(
        PreparedRule::prepare_with_scenes(&a, id, vec![], &off, &scenes, Budget::default())
            .await
            .is_err()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn panel_router_observes_without_writes_and_double_click_uses_atomic_rule_path() {
    use homesense_core::{
        Command, DeviceId,
        gesture::{Event, Settings},
        rule::{Action, Node},
    };
    use homesensed::panel_rules::{Binding, Key, Mode, PanelRules};
    for mode in [Mode::Observe, Mode::Execute] {
        let db = database();
        let id = Uuid::new_v4();
        let a = actor(&db, id).await;
        let key = DeviceId::new("control4:63:key:1").unwrap();
        let binding = |name: &str, event, on| Binding {
            id: name.into(),
            event,
            variables: vec![],
            actions: vec![Node {
                source: name.into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(on),
                },
            }],
        };
        let mut router = PanelRules::new(
            a.clone(),
            id,
            mode,
            vec![Key {
                device: key.clone(),
                settings: Settings {
                    double_click_enabled: true,
                    ..Settings::default()
                },
                bindings: vec![
                    binding("single", Event::Click, false),
                    binding("double", Event::DoubleClick, true),
                ],
            }],
            Default::default(),
            0,
        )
        .await
        .unwrap();
        assert!(router.edge(&key, true, 0).await.unwrap().is_empty());
        assert!(router.edge(&key, false, 50).await.unwrap().is_empty());
        assert!(a.dispatch().await.unwrap().is_none());
        router.edge(&key, true, 100).await.unwrap();
        let results = router.edge(&key, false, 150).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule, "double");
        if mode == Mode::Execute {
            let ticket = a.dispatch().await.unwrap().unwrap();
            assert_eq!(ticket.command.command, Command::Switch(true));
            assert_eq!(ticket.command.origin, Origin::Rule);
            a.finish_write(ticket.command.id, true).await.unwrap();
        } else {
            assert!(a.dispatch().await.unwrap().is_none());
        }
        assert!(router.advance(500).await.unwrap().is_empty());
        a.transport_failed().await.unwrap();
        a.recover_synchronized(2, 1).await.unwrap();
        assert!(router.edge(&key, true, 600).await.is_err());
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn long_dimming_release_cancels_pending_and_timeout_stops_without_burst() {
    use homesense_core::{DeviceId, ModuleKind, Observation, Percent};
    use homesense_state::{DeviceState, Quality};
    use homesensed::hold_dimming::{HoldDimming, ReleasePolicy};
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let panel = DeviceId::new("control4:63:key:1").unwrap();
    let light = DeviceId::new("control4:336").unwrap();
    let state = DeviceState {
        kind: ModuleKind::Dimmer,
        desired: None,
        observed: Some(Observation::Level(Percent::new(50).unwrap())),
        quality: Quality::Confirmed,
        observed_at_ms: Some(0),
        last_command_id: None,
        pending_command: None,
        version: 1,
    };
    let mut hold = HoldDimming::new(a.clone(), id, 0, 100, 5).unwrap();
    hold.start(
        panel.clone(),
        light.clone(),
        true,
        &state,
        0,
        ReleasePolicy::CancelPendingOnly,
    )
    .await
    .unwrap();
    assert!(
        hold.start(
            panel.clone(),
            light.clone(),
            true,
            &state,
            0,
            ReleasePolicy::CancelPendingOnly
        )
        .await
        .is_err()
    );
    assert_eq!(hold.tick(0).await.unwrap().len(), 1);
    assert_eq!(hold.tick(500).await.unwrap().len(), 1);
    hold.release(&panel).await.unwrap();
    assert_eq!(hold.active_count(), 0);
    assert!(a.dispatch().await.unwrap().is_none());
    hold.start(
        panel.clone(),
        light.clone(),
        false,
        &state,
        1000,
        ReleasePolicy::CancelPendingOnly,
    )
    .await
    .unwrap();
    assert!(hold.tick(11000).await.unwrap().is_empty());
    assert_eq!(hold.active_count(), 0);
    let mut fresh = state.clone();
    fresh.observed_at_ms = Some(12000);
    hold.start(
        panel,
        light,
        true,
        &fresh,
        12000,
        ReleasePolicy::CancelPendingOnly,
    )
    .await
    .unwrap();
    a.transport_failed().await.unwrap();
    assert!(hold.tick(12001).await.is_err());
    assert_eq!(hold.active_count(), 0);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn variable_only_storage_failure_gates_dispatch_but_cas_conflict_does_not() {
    use hap::server::WriteStatus;
    use homesense_core::rule::{Value, ValueType};
    use homesense_hap::requests::Batch;
    use homesense_storage::variables::Update;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(
        id,
        generation.clone(),
        "logic".into(),
        Some(ValueType::Integer),
    )
    .await
    .unwrap();
    let batch = || Batch {
        controller: id,
        intents: vec![],
        subscriptions: vec![],
    };
    let update = |expected_version| Update {
        revision: 1,
        name: "logic".into(),
        expected_version,
        value: Some(Value::Integer(1)),
    };
    assert_eq!(
        a.submit_rule_update(generation.clone(), batch(), vec![update(1)])
            .await
            .unwrap_err(),
        WriteStatus::InvalidValueInRequest
    );
    assert!(a.dispatch().await.unwrap().is_none());
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_logic BEFORE UPDATE ON logical_variable BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        a.submit_rule_update(generation.clone(), batch(), vec![update(0)])
            .await
            .unwrap_err(),
        WriteStatus::ServiceCommunicationFailure
    );
    assert!(a.dispatch().await.is_err());
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "logic")
            .unwrap()
            .unwrap()
            .version,
        0
    );
    sql.execute_batch("DROP TRIGGER fail_logic;").unwrap();
    // Removing the database fault alone must not silently reopen admission.
    assert!(
        a.submit_rule_update(generation, batch(), vec![update(0)])
            .await
            .is_err()
    );
    a.recover_synchronized(2, 1).await.unwrap();
    let generation = a.task_generation().await.unwrap();
    a.submit_rule_update(generation, batch(), vec![update(0)])
        .await
        .unwrap();
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "logic")
            .unwrap()
            .unwrap()
            .version,
        1
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn panel_event_rules_share_chain_budget_and_correlation() {
    use homesense_core::{
        DeviceId,
        gesture::{Event, Settings},
        rule::{Action, Condition, Expr, Node, Value},
    };
    use homesensed::panel_rules::{Binding, Key, Mode, PanelRules};
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let key = DeviceId::new("control4:63:key:1").unwrap();
    for count in [1, 200] {
        let bindings = (0..2)
            .map(|i| Binding {
                id: format!("rule{i}"),
                event: Event::Pressed,
                variables: vec![],
                actions: (0..count)
                    .map(|j| Node {
                        source: format!("r{i}/{j}"),
                        action: Action::If {
                            condition: Condition::Equal(
                                Expr::Literal(Value::Bool(true)),
                                Expr::Literal(Value::Bool(true)),
                            ),
                            then_actions: vec![],
                            else_actions: vec![],
                        },
                    })
                    .collect(),
            })
            .collect();
        let mut router = PanelRules::new(
            a.clone(),
            id,
            Mode::Execute,
            vec![Key {
                device: key.clone(),
                settings: Settings::default(),
                bindings,
            }],
            Default::default(),
            0,
        )
        .await
        .unwrap();
        let result = router.edge(&key, true, 0).await;
        if count == 1 {
            let fired = result.unwrap();
            assert_eq!(fired.len(), 2);
            let first = fired[0].cause.as_ref().unwrap();
            let second = fired[1].cause.as_ref().unwrap();
            assert_eq!(first.correlation_id(), second.correlation_id());
            assert_ne!(first.event_id(), second.event_id());
            assert_eq!(fired[0].causation_id, Some(0));
        } else {
            assert!(result.unwrap_err().message.contains("Budget"));
        }
        assert!(a.dispatch().await.unwrap().is_none());
    }
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn panel_failure_reports_prior_admission_without_replaying_or_running_later_rules() {
    use homesense_core::{
        Command, DeviceId,
        gesture::{Event, Settings},
        rule::{Action, Node},
    };
    use homesensed::panel_rules::{Binding, Key, Mode, PanelRules};
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let key = DeviceId::new("control4:63:key:1").unwrap();
    let bindings = (0..3)
        .map(|i| Binding {
            id: format!("rule{i}"),
            event: Event::Pressed,
            variables: if i == 1 {
                vec!["undefined".into()]
            } else {
                vec![]
            },
            actions: vec![Node {
                source: format!("r{i}"),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(i == 0),
                },
            }],
        })
        .collect();
    let mut router = PanelRules::new(
        a.clone(),
        id,
        Mode::Execute,
        vec![Key {
            device: key.clone(),
            settings: Settings::default(),
            bindings,
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    let failure = router.edge(&key, true, 0).await.unwrap_err();
    assert_eq!(failure.failed_rule.as_deref(), Some("rule1"));
    assert_eq!(failure.completed.len(), 1);
    assert_eq!(failure.completed[0].rule, "rule0");
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(failure.completed[0].commands, vec![ticket.command.id]);
    assert_eq!(ticket.command.command, Command::Switch(true));
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(router.edge(&key, true, 1).await.unwrap().is_empty());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_reconciles_overwritten_pending_and_keeps_other_device_timers() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesense_hap::requests::{Batch, Intent};
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let device = DeviceId::new("control4:210").unwrap();
    let other = DeviceId::new("control4:278").unwrap();
    let steps = vec![
        (device.clone(), 0),
        (device.clone(), 100),
        (other.clone(), 100),
    ]
    .into_iter()
    .map(|(device, delay_ms)| Step {
        device,
        delay_ms,
        command: Command::Switch(true),
        source: "test".into(),
    })
    .collect();
    let mut scene = SceneRuntime::new(a.clone(), id, steps, 0, 100)
        .await
        .unwrap();
    scene.poll(0).await.unwrap().unwrap();
    a.submit(Batch {
        controller: id,
        intents: vec![Intent {
            device,
            command: Command::Switch(false),
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    let manual = a.dispatch().await.unwrap().unwrap();
    a.finish_write(manual.command.id, true).await.unwrap();
    let ids = scene.poll(100).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, other);
    assert_eq!(ids, vec![ticket.command.id]);
    a.finish_write(ticket.command.id, true).await.unwrap();
    // A missed completion notification is recovered from the durable journal.
    scene.reconcile().await.unwrap();
    assert_eq!(scene.status(), Status::Partial);
    assert!(scene.poll(100).await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn device_override_cancels_future_actions_even_after_prior_output_completed() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let device = DeviceId::new("control4:210").unwrap();
    let other = DeviceId::new("control4:278").unwrap();
    let steps = vec![
        (device.clone(), 0),
        (device.clone(), 100),
        (other.clone(), 100),
    ]
    .into_iter()
    .map(|(device, delay_ms)| Step {
        device,
        delay_ms,
        command: Command::Switch(true),
        source: "test".into(),
    })
    .collect();
    let mut scene = SceneRuntime::new(a.clone(), id, steps, 0, 100)
        .await
        .unwrap();
    scene.poll(0).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    scene.written(ticket.command.id, true).await.unwrap();
    assert_eq!(scene.cancel_device(&device).await.unwrap(), 1);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT reason FROM scene_cancellation WHERE run_id=?1",[scene.audit_id()],|r|r.get::<_,String>(0)).unwrap(),"manual_override");
    assert_eq!(scene.cancel_device(&device).await.unwrap(),0);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_cancellation WHERE run_id=?1",[scene.audit_id()],|r|r.get::<_,i64>(0)).unwrap(),1);
    let query=homesense_storage::scene_run::Query::Cancellations{boot:a.boot_id().into(),run:scene.audit_id().into(),after:0,limit:1};
    let generation=a.task_generation().await.unwrap();
    assert!(a.scene_history(Uuid::new_v4(),generation.clone(),query.clone()).await.is_err());
    let page=a.scene_history(id,generation,query).await.unwrap().unwrap();
    assert_eq!(page.cancellations.len(),1);
    assert_eq!(page.cancellations[0].cancellation.reason,homesense_storage::scene_run::CancellationReason::ManualOverride);
    assert_eq!(page.cancellations[0].cancellation.device,device);


    scene.poll(100).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, other);
    a.finish_write(ticket.command.id, true).await.unwrap();
    scene.reconcile().await.unwrap();
    assert_eq!(scene.status(), Status::Partial);
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn device_override_does_not_pretend_inflight_output_was_cancelled() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let device = DeviceId::new("control4:210").unwrap();
    let steps = [0, 100]
        .into_iter()
        .map(|delay_ms| Step {
            device: device.clone(),
            delay_ms,
            command: Command::Switch(true),
            source: "test".into(),
        })
        .collect();
    let mut scene = SceneRuntime::new(a.clone(), id, steps, 0, 100)
        .await
        .unwrap();
    scene.poll(0).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(scene.cancel_device(&device).await.unwrap(), 1);
    assert_eq!(scene.status(), Status::Running);
    assert!(scene.poll(100).await.is_err());
    a.finish_write(ticket.command.id, true).await.unwrap();
    scene.reconcile().await.unwrap();
    assert_eq!(scene.status(), Status::Partial);
    assert!(scene.poll(100).await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scheduler_replaces_overlapping_devices_and_preserves_other_scene_members() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesensed::scene_scheduler::SceneScheduler;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let first = DeviceId::new("control4:210").unwrap();
    let other = DeviceId::new("control4:278").unwrap();
    let step = |device: DeviceId, delay_ms, on| Step {
        device,
        delay_ms,
        command: Command::Switch(on),
        source: "scheduler-test".into(),
    };
    let mut scheduler = SceneScheduler::new(a.clone(), controller, 2, 0).unwrap();
    let old = scheduler
        .start(
            vec![step(first.clone(), 0, true), step(other.clone(), 100, true)],
            0,
            100,
        )
        .await
        .unwrap();
    scheduler.tick(0).await.unwrap(); // queued old output, not dispatched
    let new = scheduler
        .start(vec![step(first.clone(), 0, false)], 0, 100)
        .await
        .unwrap();
    assert!(scheduler.retire(old).is_err());
    assert!(
        scheduler
            .start(vec![step(first.clone(), 0, true)], 0, 100)
            .await
            .is_err()
    );
    let results = scheduler.tick(0).await.unwrap();
    assert!(results.iter().all(|r| r.result.is_ok()));
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, first);
    assert_eq!(ticket.command.command, Command::Switch(false));
    // A second tick sees Writing and must not duplicate the command.
    assert!(
        scheduler
            .tick(0)
            .await
            .unwrap()
            .iter()
            .all(|r| r.result.as_ref().unwrap().is_empty())
    );
    a.finish_write(ticket.command.id, true).await.unwrap();
    scheduler.tick(100).await.unwrap();
    assert_eq!(scheduler.status(new), Some(Status::CommandsWritten));
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, other);
    a.finish_write(ticket.command.id, true).await.unwrap();
    scheduler.tick(100).await.unwrap();
    assert_eq!(scheduler.retire(old).unwrap(), Status::Partial);
    assert_eq!(scheduler.retire(new).unwrap(), Status::CommandsWritten);
    assert!(scheduler.tick(99).await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scheduler_stop_reconciles_transport_cancelled_runs_and_invalidates_timers() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status, Step},
    };
    use homesensed::scene_scheduler::SceneScheduler;
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let step = |name: &str, delay_ms| Step {
        device: DeviceId::new(name).unwrap(),
        delay_ms,
        command: Command::Switch(true),
        source: "stop-test".into(),
    };
    let mut scheduler = SceneScheduler::new(a.clone(), id, 2, 0).unwrap();
    let first = scheduler
        .start(vec![step("control4:210", 0)], 0, 100)
        .await
        .unwrap();
    let second = scheduler
        .start(vec![step("control4:278", 100)], 0, 100)
        .await
        .unwrap();
    scheduler.tick(0).await.unwrap();
    a.transport_failed().await.unwrap();
    let results = scheduler.stop().await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, result)| result.is_ok()), "transport audit already cancelled pending output");
    assert_eq!(scheduler.status(first), Some(Status::Cancelled));
    assert_eq!(scheduler.status(second), Some(Status::Cancelled));
    assert!(scheduler.tick(100).await.is_err());
    a.recover_synchronized(2, 1).await.unwrap();
    assert!(
        scheduler
            .start(vec![step("control4:278", 0)], 100, 100)
            .await
            .is_err()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn manual_admission_fence_rejects_stale_delayed_scene_without_notification() {
    use homesense_core::{Command, DeviceId, scene::Step};
    use homesense_hap::requests::{Batch, Intent};
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let device = DeviceId::new("control4:210").unwrap();
    let other = DeviceId::new("control4:278").unwrap();
    let step = |device| Step {
        device,
        delay_ms: 100,
        command: Command::Switch(true),
        source: "fence-test".into(),
    };
    let mut old = SceneRuntime::new(
        a.clone(),
        controller,
        vec![step(device.clone()), step(other.clone())],
        0,
        100,
    )
    .await
    .unwrap();
    a.submit(Batch {
        controller,
        intents: vec![Intent {
            device: device.clone(),
            command: Command::Switch(false),
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    let manual = a.dispatch().await.unwrap().unwrap();
    a.finish_write(manual.command.id, true).await.unwrap();
    // Deliberately omit scheduler.manual_override: polling detects the accepted override.
    assert!(old.poll(100).await.unwrap().is_none());
    assert!(a.dispatch().await.unwrap().is_none());
    old.poll(100).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, other);
    a.finish_write(ticket.command.id, true).await.unwrap();
    old.reconcile().await.unwrap();
    // A newly requested scene after manual control is still allowed.
    let mut new = SceneRuntime::new(a.clone(), controller, vec![step(device)], 100, 100)
        .await
        .unwrap();
    new.poll(200).await.unwrap().unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.command, Command::Switch(true));
    a.finish_write(ticket.command.id, true).await.unwrap();
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn feedback_change_rejects_entire_rule_before_variable_and_command_commit() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Condition, Expr, Node, Value, ValueType},
    };
    use homesensed::{feedback::Feedback, rule_runtime::PreparedRule};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config = homesense_config::Configuration::parse(&document, ExecutionContext::Live).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Live,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(
        controller,
        generation.clone(),
        "flag".into(),
        Some(ValueType::Bool),
    )
    .await
    .unwrap();
    let nodes = [Node {
        source: "test/unknown".into(),
        action: Action::If {
            condition: Condition::Unknown(Expr::DeviceOn(DeviceId::new("control4:210").unwrap())),
            then_actions: vec![
                Node {
                    source: "test/variable".into(),
                    action: Action::Set {
                        variable: "flag".into(),
                        value: Expr::Literal(Value::Bool(true)),
                    },
                },
                Node {
                    source: "test/command".into(),
                    action: Action::Command {
                        device: DeviceId::new("control4:210").unwrap(),
                        command: Command::Switch(false),
                    },
                },
            ],
            else_actions: vec![],
        },
    }];
    let stale = PreparedRule::prepare_with_feedback(
        &a,
        controller,
        vec!["flag".into()],
        &nodes,
        Budget::default(),
        &feedback,
        5000,
    )
    .await
    .unwrap();
    // No-op expiry no longer changes the feedback snapshot. Use an actual
    // observation to invalidate the plan before its atomic commit.
    assert_eq!(feedback.apply(&homesensed::transport_owner::GatewayRead {
        gateway_id: "hopebeauty".into(), transport_epoch: 1, received_at_ms: 0,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame { address: 0x11, command: 0xd5, value: 30 }],
        },
    }).unwrap(), 1);
    assert!(stale.commit(&a).await.is_err());
    assert!(a.dispatch().await.unwrap().is_none());
    assert_eq!(
        a.logical_variable(controller, generation.clone(), "flag".into(), None)
            .await
            .unwrap()
            .unwrap()
            .version,
        0
    );
    let fresh = PreparedRule::prepare_with_feedback(
        &a,
        controller,
        vec!["flag".into()],
        &nodes,
        Budget::default(),
        &feedback,
        5000,
    )
    .await
    .unwrap();
    let ids = fresh.commit(&a).await.unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        a.logical_variable(controller, generation, "flag".into(), None)
            .await
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Bool(true))
    );
    let pending = PreparedRule::prepare_with_feedback(
        &a,
        controller,
        vec!["flag".into()],
        &nodes,
        Budget::default(),
        &feedback,
        5000,
    )
    .await
    .unwrap();
    assert!(pending.commit(&a).await.is_err());
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, ids[0]);
    a.finish_write(ids[0], true).await.unwrap();
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn panel_feedback_route_uses_shared_chain_and_pending_output_guard() {
    use homesense_core::{
        Command, DeviceId,
        gesture::{Event, Settings},
        rule::{Action, Condition, Expr, Node},
    };
    use homesensed::{
        feedback::Feedback,
        panel_rules::{Binding, Key, Mode, PanelRules},
    };
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config = homesense_config::Configuration::parse(&document, ExecutionContext::Live).unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Live,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let key = DeviceId::new("control4:63:key:1").unwrap();
    let device = DeviceId::new("control4:210").unwrap();
    let bindings = (0..2)
        .map(|i| Binding {
            id: format!("feedback{i}"),
            event: Event::Pressed,
            variables: vec![],
            actions: vec![Node {
                source: format!("rule{i}"),
                action: Action::If {
                    condition: Condition::Unknown(Expr::DeviceOn(device.clone())),
                    then_actions: vec![Node {
                        source: "fallback".into(),
                        action: Action::Command {
                            device: device.clone(),
                            command: Command::Switch(false),
                        },
                    }],
                    else_actions: vec![],
                },
            }],
        })
        .collect();
    let mut router = PanelRules::new(
        a.clone(),
        id,
        Mode::Execute,
        vec![Key {
            device: key.clone(),
            settings: Settings::default(),
            bindings,
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    let failure = router
        .edge_with_feedback(&key, true, 0, Some((&feedback, 5000)))
        .await
        .unwrap_err();
    assert_eq!(failure.completed.len(), 1);
    assert_eq!(failure.failed_rule.as_deref(), Some("feedback1"));
    assert!(failure.completed[0].cause.is_some());
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(failure.completed[0].commands, vec![ticket.command.id]);
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn panel_subscription_skips_history_deduplicates_cursor_and_stops_on_gap() {
    use homesense_core::{
        DeviceId,
        gesture::{Event, Settings},
    };
    use homesensed::{
        feedback::Feedback,
        panel_rules::{Binding, Key, Mode, PanelRules},
        transport_owner::GatewayRead,
    };
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config = homesense_config::Configuration::parse(&document, ExecutionContext::Live).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Live,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let sample = |time, value| GatewayRead {
        gateway_id: "hopebeauty".into(),
        transport_epoch: 1,
        received_at_ms: time,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame {
                address: 0x41,
                command: 0xd1,
                value,
            }],
        },
    };
    feedback.apply(&sample(0, 0xf1)).unwrap();
    let mut router = PanelRules::new(
        a.clone(),
        id,
        Mode::Observe,
        vec![Key {
            device: DeviceId::new("control4:63:key:1").unwrap(),
            settings: Settings::default(),
            bindings: vec![Binding {
                id: "press".into(),
                event: Event::Pressed,
                variables: vec![],
                actions: vec![],
            }],
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    router.bind_feedback(&feedback).unwrap();
    assert!(
        router
            .consume_feedback(&feedback, 0, 1000)
            .await
            .unwrap()
            .is_empty()
    );
    feedback.apply(&sample(1, 0xf2)).unwrap();
    router.consume_feedback(&feedback, 1, 1000).await.unwrap();
    feedback.apply(&sample(40, 0xf1)).unwrap();
    let events = router.consume_feedback(&feedback, 40, 1000).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].observed_only);
    assert!(
        router
            .consume_feedback(&feedback, 40, 1000)
            .await
            .unwrap()
            .is_empty()
    );
    for time in 41..341 {
        feedback.apply(&sample(time, 0xf1)).unwrap();
    }
    assert!(router.consume_feedback(&feedback, 341, 1000).await.is_err());
    assert!(router.advance(342).await.is_err());
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn automation_loop_routes_received_panel_edge_to_audited_output_then_stops() {
    automation_loop_fixture(false, false).await;
}

#[tokio::test]
async fn automation_loop_routes_persisted_calendar_to_output_and_stops_timer() {
    automation_loop_fixture(true, false).await;
}

#[tokio::test]
async fn automation_loop_pre_stopped_calendar_never_checkpoints_or_writes() {
    automation_loop_fixture(true, true).await;
}

async fn automation_loop_fixture(scheduled: bool, pre_stopped: bool) {
    use homesense_core::{
        Command, DeviceId,
        gesture::{Event, Settings},
        rule::{Action, Node},
    };
    use homesensed::{
        feedback::Feedback,
        observed_transport::ObservedTransport,
        panel_rules::{Binding, Key, Mode, PanelRules},
        scene_scheduler::SceneScheduler,
        transport_owner::TransportOwner,
    };
    use std::{
        io::{Read, Write},
        sync::{Arc, Mutex},
        time::Duration,
    };
    struct Wire {
        input: Option<[u8; 5]>,
        output: Arc<Mutex<Vec<u8>>>,
        stop: tokio::sync::watch::Sender<bool>,
    }
    impl Read for Wire {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if let Some(frame) = self.input.take() {
                b[..5].copy_from_slice(&frame);
                Ok(5)
            } else {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.lock().unwrap().extend_from_slice(b);
            self.stop.send(true).unwrap();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&document, ExecutionContext::Simulation).unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(pre_stopped);
    let output = Arc::new(Mutex::new(vec![]));
    let wire = Wire {
        input: if scheduled {
            None
        } else {
            Some([0xae, 0x41, 0xd1, 0xf1, 0xee])
        },
        output: output.clone(),
        stop,
    };
    let transport = TransportOwner::new(
        a.clone(),
        [(
            "hopebeauty".into(),
            hb_transport::Connection::for_simulation(wire, 1),
        )]
        .into(),
    );
    let mut owner = ObservedTransport::new(transport, feedback);
    let mut panels = PanelRules::new(
        a.clone(),
        id,
        Mode::Execute,
        vec![Key {
            device: DeviceId::new("control4:63:key:1").unwrap(),
            settings: Settings::default(),
            bindings: vec![Binding {
                id: "press".into(),
                event: Event::Pressed,
                variables: vec![],
                actions: vec![Node {
                    source: "loop-test".into(),
                    action: Action::Command {
                        device: DeviceId::new("control4:210").unwrap(),
                        command: Command::Switch(true),
                    },
                }],
            }],
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    let mut scenes = SceneScheduler::new(a.clone(), id, 4, 0).unwrap();
    use homesensed::event_rules::{Binding as EventBinding, EventRules, ScheduledRules, Trigger};
    let due = 946_828_860_000;
    let mut calendar = if scheduled {
        let rules = EventRules::new(
            a.clone(),
            id,
            vec![EventBinding {
                id: "calendar-loop".into(),
                trigger: Trigger::Schedule("daily".into()),
                variables: vec![],
                actions: vec![Node {
                    source: "loop-test".into(),
                    action: Action::Command {
                        device: DeviceId::new("control4:210").unwrap(),
                        command: Command::Switch(true),
                    },
                }],
            }],
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();
        Some(
            ScheduledRules::new(
                rules,
                vec![homesense_core::calendar::Schedule {
                    id: "daily".into(),
                    weekdays: [true; 7],
                    hour: 0,
                    minute: 1,
                }],
                due - 1,
                1000,
            )
            .await
            .unwrap(),
        )
    } else {
        None
    };
    tokio::time::timeout(
        Duration::from_secs(2),
        homesensed::automation_drive::drive_with_calendar(
            &a,
            &mut owner,
            &mut panels,
            &mut scenes,
            calendar.as_mut(),
            Duration::from_millis(1),
            5000,
            receiver,
            || Ok(due),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        output.lock().unwrap().len(),
        if pre_stopped { 0 } else { 5 }
    );
    if let Some(calendar) = &mut calendar {
        assert_eq!(
            Store::open(&db.0)
                .unwrap()
                .calendar_watermark(1, None)
                .unwrap(),
            if pre_stopped { None } else { Some(due) }
        );
        assert!(calendar.poll(due + 1, None).await.is_err());
    }
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(a.boot_id(), 1)
            .unwrap(),
        if pre_stopped {
            None
        } else {
            Some(Status::Written)
        }
    );
    assert!(panels.advance(100).await.is_err());
    assert!(scenes.tick(100).await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn ninety_six_channel_scene_preserves_order_and_cancels_remaining_outputs() {
    use homesense_core::{
        Command, DeviceId,
        scene::{Status as SceneStatus, Step},
    };
    use homesensed::{scene_runtime::SceneRuntime, transport_owner::TransportOwner};
    use std::{
        io::{Read, Write},
        sync::{Arc, Mutex},
    };
    struct Wire(Arc<Mutex<Vec<u8>>>);
    impl Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for stop_after in [37, 96] {
        let db = Db(std::env::temp_dir().join(format!("scene96-{}.sqlite", Uuid::new_v4())));
        let modules:Vec<_>=(1..=12).map(|address|hap::serde_json::json!({"address":format!("{address:02X}"),"profile":"r8i8_din_pro_v2_7_7","verification":{"status":"verified","evidence":"synthetic 96-channel local test only","verified_at_unix_ms":1}})).collect();
        let devices:Vec<_>=(0..96).map(|i|hap::serde_json::json!({"id":format!("test:{i}"),"name":format!("test {i}"),"gateway":"test","address":format!("{:02X}",i/8+1),"channel":i%8+1,"enabled":true})).collect();
        let document=hap::serde_json::to_vec(&hap::serde_json::json!({"schema_version":1,"revision":1,"manual_degraded_enabled":false,"gateways":[{"id":"test","endpoint":"127.0.0.1:50007","modules":modules}],"devices":devices})).unwrap();
        Store::open(&db.0)
            .unwrap()
            .publish_configuration(None, &document, ExecutionContext::Simulation, &[])
            .unwrap();
        let controller = Uuid::new_v4();
        let a = actor(&db, controller).await;
        let output = Arc::new(Mutex::new(vec![]));
        let mut transport = TransportOwner::new(
            a.clone(),
            [(
                "test".into(),
                hb_transport::Connection::for_simulation(Wire(output.clone()), 1),
            )]
            .into(),
        );
        let steps = (0..96)
            .map(|i| Step {
                device: DeviceId::new(format!("test:{i}")).unwrap(),
                command: Command::Switch(i % 2 == 0),
                delay_ms: 0,
                source: format!("scene/member/{i}"),
            })
            .collect();
        let mut scene = SceneRuntime::new(a.clone(), controller, steps, 0, 1000)
            .await
            .unwrap();
        for i in 0..stop_after {
            let ids = scene.poll(0).await.unwrap().unwrap();
            let result = transport.step().await.unwrap().unwrap();
            assert_eq!(result.command_id, ids[0]);
            assert!(result.result.is_ok());
            assert_eq!(result.device_id.as_str(), format!("test:{i}"));
            scene.reconcile().await.unwrap();
        }
        if stop_after < 96 {
            assert_eq!(scene.cancel().await.unwrap(), 96 - stop_after);
            assert_eq!(scene.status(), SceneStatus::Partial);
        } else {
            assert_eq!(scene.status(), SceneStatus::CommandsWritten);
        }
        assert!(scene.poll(0).await.unwrap().is_none());
        assert!(transport.step().await.unwrap().is_none());
        let expected: Vec<u8> = (0..stop_after)
            .flat_map(|i| {
                [
                    0xae,
                    (i / 8 + 1) as u8,
                    (i % 8 + 1) as u8,
                    if i % 2 == 0 { 0xf1 } else { 0xf2 },
                    0xee,
                ]
            })
            .collect();
        assert_eq!(*output.lock().unwrap(), expected);
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn manual_change_stops_old_hold_targets_without_automatic_retry() {
    use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
    use homesense_hap::requests::{Batch, Intent};
    use homesense_state::{DeviceState, Quality};
    use homesensed::hold_dimming::{HoldDimming, ReleasePolicy};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let device = DeviceId::new("control4:308").unwrap();
    let panel = DeviceId::new("control4:63:key:1").unwrap();
    let state = DeviceState {
        kind: ModuleKind::Dimmer,
        desired: None,
        observed: Some(Observation::Level(Percent::new(30).unwrap())),
        quality: Quality::Confirmed,
        observed_at_ms: Some(0),
        last_command_id: None,
        pending_command: None,
        version: 1,
    };
    let mut hold = HoldDimming::new(a.clone(), controller, 0, 100, 5).unwrap();
    hold.start(
        panel.clone(),
        device.clone(),
        true,
        &state,
        0,
        ReleasePolicy::CancelPendingOnly,
    )
    .await
    .unwrap();
    a.submit(Batch {
        controller,
        intents: vec![Intent {
            device,
            command: Command::SetLevel {
                level: Percent::new(70).unwrap(),
                transition: LevelTransition::Immediate,
            },
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    assert!(hold.tick(100).await.is_err());
    assert!(hold.tick(200).await.unwrap().is_empty());
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert!(matches!(ticket.command.command,Command::SetLevel {level,..} if level.get()==70));
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    hold.release(&panel).await.unwrap();
    assert_eq!(hold.active_count(), 0);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_panel_long_press_starts_dimming_and_release_cancels_queued_sample() {
    use homesense_core::{Command, DeviceId, gesture::Settings};
    use homesensed::{
        feedback::Feedback,
        hold_dimming::ReleasePolicy,
        panel_rules::{HoldBinding, Key, Mode, PanelRules},
        transport_owner::GatewayRead,
    };
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&document, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    feedback
        .apply(&GatewayRead {
            gateway_id: "hopebeauty".into(),
            transport_epoch: 1,
            received_at_ms: 0,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x11,
                    command: 0xd5,
                    value: 30,
                }],
            },
        })
        .unwrap();
    let key = DeviceId::new("control4:63:key:1").unwrap();
    let mut panels = PanelRules::new(
        a.clone(),
        id,
        Mode::Execute,
        vec![Key {
            device: key.clone(),
            settings: Settings::default(),
            bindings: vec![],
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    panels
        .configure_holds(
            [(
                key.clone(),
                HoldBinding {
                    device: DeviceId::new("control4:308").unwrap(),
                    up: true,
                    release: ReleasePolicy::CancelPendingOnly,
                },
            )]
            .into(),
            0,
            100,
            5,
        )
        .unwrap();
    panels
        .edge_with_feedback(&key, true, 0, Some((&feedback, 5000)))
        .await
        .unwrap();
    panels
        .advance_with_feedback(600, Some((&feedback, 5000)))
        .await
        .unwrap();
    let first = panels.tick_holds(600).await.unwrap();
    assert_eq!(first.len(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert!(matches!(ticket.command.command,Command::SetLevel {level,..} if level.get()==35));
    a.finish_write(ticket.command.id, true).await.unwrap();
    let next = panels.tick_holds(700).await.unwrap();
    assert_eq!(next.len(), 1);
    panels
        .edge_with_feedback(&key, false, 710, Some((&feedback, 5000)))
        .await
        .unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(panels.tick_holds(800).await.unwrap().is_empty());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn hold_release_continues_other_channels_after_one_pause_is_rejected() {
    rejected_pause_does_not_prevent_other_stops(false).await;
}

#[tokio::test]
async fn hold_deadline_stops_other_channels_after_one_pause_is_rejected() {
    rejected_pause_does_not_prevent_other_stops(true).await;
}

async fn rejected_pause_does_not_prevent_other_stops(deadline: bool) {
    use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
    use homesense_hap::requests::{Batch, Intent};
    use homesense_state::{DeviceState, Quality};
    use homesensed::hold_dimming::{HoldDimming, ReleasePolicy};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let panel = DeviceId::new("control4:63:key:1").unwrap();
    let first = DeviceId::new("control4:308").unwrap();
    let second = DeviceId::new("control4:336").unwrap();
    let state = DeviceState {
        kind: ModuleKind::Dimmer,
        desired: None,
        observed: Some(Observation::Level(Percent::new(30).unwrap())),
        quality: Quality::Confirmed,
        observed_at_ms: Some(0),
        last_command_id: None,
        pending_command: None,
        version: 1,
    };
    let mut hold = HoldDimming::new(a.clone(), controller, 0, 100, 5).unwrap();
    hold.start(
        panel.clone(),
        first.clone(),
        true,
        &state,
        0,
        ReleasePolicy::PauseRampFieldVerified,
    )
    .await
    .unwrap();
    hold.start(
        panel.clone(),
        second,
        true,
        &state,
        0,
        ReleasePolicy::CancelPendingOnly,
    )
    .await
    .unwrap();
    hold.tick(0).await.unwrap();
    a.submit(Batch {
        controller,
        intents: vec![Intent {
            device: first.clone(),
            command: Command::SetLevel {
                level: Percent::new(70).unwrap(),
                transition: LevelTransition::Immediate,
            },
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    if deadline {
        let error = hold.tick(10000).await.unwrap_err();
        assert!(error.contains("hold deadline errors"));
    } else {
        let report = hold.release_detailed(&panel).await;
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, first);
        assert!(report.commands.is_empty());
        assert!(hold.tick(100).await.unwrap().is_empty());
    }
    assert_eq!(hold.active_count(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, first);
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_rules_cascade_committed_changes_and_isolate_cycles() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Expr, Node, Value, ValueType},
    };
    use homesensed::event_rules::{Binding, EventRules, Trigger};
    for cycle in [false, true] {
        let db = database();
        let controller = Uuid::new_v4();
        let a = actor(&db, controller).await;
        let generation = a.task_generation().await.unwrap();
        a.logical_variable(
            controller,
            generation.clone(),
            "flag".into(),
            Some(ValueType::Bool),
        )
        .await
        .unwrap();
        let set = |value| Node {
            source: "fixture:set".into(),
            action: Action::Set {
                variable: "flag".into(),
                value: Expr::Literal(Value::Bool(value)),
            },
        };
        let command = |on| Node {
            source: "fixture:light".into(),
            action: Action::Command {
                device: DeviceId::new("control4:210").unwrap(),
                command: Command::Switch(on),
            },
        };
        let mut router = EventRules::new(
            a.clone(),
            controller,
            vec![
                Binding {
                    id: "startup".into(),
                    trigger: Trigger::StartupSynchronized,
                    variables: vec!["flag".into()],
                    actions: vec![set(true)],
                },
                Binding {
                    id: "flag-rule".into(),
                    trigger: Trigger::VariableChanged("flag".into()),
                    variables: vec!["flag".into()],
                    actions: vec![set(!cycle), command(true)],
                },
                Binding {
                    id: "independent".into(),
                    trigger: Trigger::Schedule("fixture".into()),
                    variables: vec![],
                    actions: vec![command(false)],
                },
            ],
            Default::default(),
            Budget::default(),
        )
        .await
        .unwrap();
        let report = router.dispatch(Trigger::StartupSynchronized, None).await;
        assert_eq!(report.completed.len(), 2);
        let sql = rusqlite::Connection::open(&db.0).unwrap();
        let rows: i64 = sql
            .query_row(
                "SELECT count(*) FROM rule_admission WHERE boot_id=?1 AND correlation=?2",
                rusqlite::params![a.boot_id(), format!("{:032x}", report.correlation_id)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 2);
        let parent:i64=sql.query_row("SELECT parent_id FROM rule_admission WHERE boot_id=?1 AND correlation=?2 AND rule_id='flag-rule'",rusqlite::params![a.boot_id(),format!("{:032x}",report.correlation_id)],|r|r.get(0)).unwrap();
        assert_eq!(parent as u64, report.completed[0].cause.event_id());
        let linked: i64 = sql
            .query_row(
                "SELECT count(*) FROM rule_command WHERE boot_id=?1 AND correlation=?2",
                rusqlite::params![a.boot_id(), format!("{:032x}", report.correlation_id)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 1);
        if cycle {
            assert!(report.failure.as_ref().unwrap().contains("Cycle"));
        } else {
            assert!(report.failure.is_none());
            assert!(report.completed[1].changed_variables.is_empty());
        }
        assert_eq!(
            report.completed[1].causation_id,
            report.completed[0].cause.event_id()
        );
        assert!(
            report
                .completed
                .iter()
                .all(|r| r.cause.correlation_id() == report.correlation_id)
        );
        assert_eq!(
            a.logical_variable(controller, generation, "flag".into(), None)
                .await
                .unwrap()
                .unwrap()
                .value,
            Some(Value::Bool(!cycle))
        );
        let ticket = a.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.id, report.completed[1].commands[0]);
        a.finish_write(ticket.command.id, true).await.unwrap();
        assert!(a.dispatch().await.unwrap().is_none());
        let read_generation = a.task_generation().await.unwrap();
        let first_page = a
            .rule_chain(
                controller,
                read_generation.clone(),
                report.correlation_id,
                0,
                1,
            )
            .await
            .unwrap();
        assert_eq!(first_page.records.len(), 1);
        let terminal = first_page.outcome.as_ref().unwrap();
        assert_eq!(terminal.revision, 1);
        if cycle {
            assert!(terminal.error.as_ref().unwrap().contains("Cycle"));
        } else {
            assert!(terminal.error.is_none());
        }
        assert_eq!(first_page.records[0].admission.rule, "startup");
        assert!(first_page.records[0].commands.is_empty());
        let second_page = a
            .rule_chain(
                controller,
                read_generation.clone(),
                report.correlation_id,
                first_page.next_after.unwrap(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(
            second_page.records[0].admission.parent,
            first_page.records[0].admission.event
        );
        assert_eq!(second_page.records[0].commands[0].status, Status::Written);
        assert_eq!(
            second_page.records[0].commands[0].id,
            report.completed[1].commands[0]
        );
        assert!(
            a.rule_chain(
                controller,
                read_generation.clone(),
                report.correlation_id,
                second_page.next_after.unwrap(),
                1
            )
            .await
            .unwrap()
            .records
            .is_empty()
        );
        assert!(
            a.rule_chain(
                Uuid::new_v4(),
                read_generation.clone(),
                report.correlation_id,
                0,
                1
            )
            .await
            .is_err()
        );
        assert!(
            a.rule_chain(controller, read_generation, report.correlation_id, 0, 65)
                .await
                .is_err()
        );
        // A failed correlation does not disable unrelated roots or admission.
        let independent = router
            .dispatch(Trigger::Schedule("fixture".into()), None)
            .await;
        assert!(independent.failure.is_none());
        assert_eq!(independent.completed.len(), 1);
        assert_ne!(independent.correlation_id, report.correlation_id);
        let ticket = a.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.command, Command::Switch(false));
        a.finish_write(ticket.command.id, true).await.unwrap();
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn event_rules_failed_admission_emits_no_variable_child_or_partial_update() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Expr, Node, Value, ValueType},
    };
    use homesensed::event_rules::{Binding, EventRules, Trigger};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(
        controller,
        generation.clone(),
        "flag".into(),
        Some(ValueType::Bool),
    )
    .await
    .unwrap();
    let command = |device| Node {
        source: "fixture".into(),
        action: Action::Command {
            device: DeviceId::new(device).unwrap(),
            command: Command::Switch(true),
        },
    };
    let mut router = EventRules::new(
        a.clone(),
        controller,
        vec![
            Binding {
                id: "invalid".into(),
                trigger: Trigger::StartupSynchronized,
                variables: vec!["flag".into()],
                actions: vec![
                    Node {
                        source: "fixture".into(),
                        action: Action::Set {
                            variable: "flag".into(),
                            value: Expr::Literal(Value::Bool(true)),
                        },
                    },
                    command("missing-device"),
                ],
            },
            Binding {
                id: "must-not-run".into(),
                trigger: Trigger::VariableChanged("flag".into()),
                variables: vec![],
                actions: vec![command("control4:210")],
            },
        ],
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let report = router.dispatch(Trigger::StartupSynchronized, None).await;
    assert!(report.failure.is_some());
    assert!(report.completed.is_empty());
    assert_eq!(
        a.logical_variable(controller, generation, "flag".into(), None)
            .await
            .unwrap()
            .unwrap()
            .value,
        None
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_feedback_subscription_deduplicates_and_rejects_reconnected_history() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Node},
    };
    use homesensed::{
        event_rules::{Binding, EventRules, Trigger},
        feedback::Feedback,
        transport_owner::GatewayRead,
    };
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&document, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut router = EventRules::new(
        a.clone(),
        controller,
        vec![Binding {
            id: "actual-level".into(),
            trigger: Trigger::DeviceChanged(DeviceId::new("control4:308").unwrap()),
            variables: vec![],
            actions: vec![Node {
                source: "fixture".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            }],
        }],
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let apply = |feedback: &mut Feedback, level| {
        feedback
            .apply(&GatewayRead {
                gateway_id: "hopebeauty".into(),
                transport_epoch: 1,
                received_at_ms: 0,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 0x11,
                        command: 0xd5,
                        value: level,
                    }],
                },
            })
            .unwrap();
    };
    apply(&mut feedback, 30);
    router.bind_feedback(&feedback).unwrap();
    assert!(
        router
            .consume_feedback(&feedback, 5000)
            .await
            .unwrap()
            .is_empty()
    );
    apply(&mut feedback, 40);
    let reports = router.consume_feedback(&feedback, 5000).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].failure.is_none());
    assert_eq!(reports[0].completed.len(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, reports[0].completed[0].commands[0]);
    a.finish_write(ticket.command.id, true).await.unwrap();
    apply(&mut feedback, 40);
    assert!(
        router
            .consume_feedback(&feedback, 5000)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.transport_failed().await.unwrap();
    a.recover_synchronized(2, 1).await.unwrap();
    assert!(router.consume_feedback(&feedback, 5000).await.is_err());
    assert!(
        router
            .dispatch(
                Trigger::DeviceChanged(DeviceId::new("control4:308").unwrap()),
                None
            )
            .await
            .failure
            .is_some()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn calendar_dispatch_persists_before_rules_and_never_replays_after_restart() {
    use homesense_core::{
        Command, DeviceId,
        calendar::Schedule,
        rule::{Action, Budget, Node},
    };
    use homesensed::event_rules::{Binding, EventRules, ScheduledRules, Trigger};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let schedules = || {
        vec![Schedule {
            id: "daily".into(),
            weekdays: [true; 7],
            hour: 0,
            minute: 1,
        }]
    };
    let binding = || {
        vec![Binding {
            id: "scheduled-light".into(),
            trigger: Trigger::Schedule("daily".into()),
            variables: vec![],
            actions: vec![Node {
                source: "fixture".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            }],
        }]
    };
    let due = 946_828_860_000;
    let rules = EventRules::new(
        a.clone(),
        controller,
        binding(),
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let mut calendar = ScheduledRules::new(rules, schedules(), due - 1, 1000)
        .await
        .unwrap();
    let reports = calendar.poll(due, None).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].failure.is_none());
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .calendar_watermark(1, None)
            .unwrap(),
        Some(due)
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    a.shutdown().await.unwrap();
    let a = actor(&db, controller).await;
    let rules = EventRules::new(
        a.clone(),
        controller,
        binding(),
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let mut calendar = ScheduledRules::new(rules, schedules(), due - 1, 1000)
        .await
        .unwrap();
    assert!(calendar.poll(due, None).await.unwrap().is_empty());
    assert!(a.dispatch().await.unwrap().is_none());
    let rules = EventRules::new(
        a.clone(),
        controller,
        binding(),
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let mut failed = ScheduledRules::new(rules, schedules(), due + 86_400_000 - 1, 1000)
        .await
        .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_calendar BEFORE UPDATE ON calendar_watermark BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(failed.poll(due + 86_400_000, None).await.is_err());
    let diagnostics = failed.status();
    assert!(diagnostics.clock.stopped);
    assert_eq!(diagnostics.clock.watermark_unix_ms, due + 86_400_000);
    assert_eq!(diagnostics.persisted_watermark_unix_ms, Some(due));
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .calendar_watermark(1, None)
            .unwrap(),
        Some(due)
    );
    assert!(a.dispatch().await.is_err());
    sql.execute_batch("DROP TRIGGER fail_calendar;").unwrap();
    assert!(failed.poll(due + 86_400_000, None).await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn strict_rule_document_loads_into_persistent_runtime_without_enabling_drafts() {
    use homesense_core::rule::{Budget, Value, ValueType};
    use homesensed::event_rules::{EventRules, Trigger};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(
        controller,
        generation.clone(),
        "mode".into(),
        Some(ValueType::Integer),
    )
    .await
    .unwrap();
    let input = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[
        {"id":"startup","enabled":true,"review_evidence":"isolated fixture","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[
            {"source":"set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":3}}},
            {"source":"output","action":{"kind":"command","device":"control4:210","command":{"kind":"switch","on":true}}}
        ]},
        {"id":"draft","source":"fixture","trigger":{"kind":"schedule","schedule":"unresolved-draft"},"actions":[]}
    ]});
    let document = homesense_config::rule_document::Document::parse(
        &hap::serde_json::to_vec(&input).unwrap(),
        &config,
    )
    .unwrap();
    let mut runtime = EventRules::from_document(
        a.clone(),
        controller,
        &document,
        Default::default(),
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let report = runtime.dispatch(Trigger::StartupSynchronized, None).await;
    assert!(report.failure.is_none());
    assert_eq!(report.completed.len(), 1);
    assert_eq!(
        a.logical_variable(controller, generation, "mode".into(), None)
            .await
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    let report = runtime
        .dispatch(Trigger::Schedule("unresolved-draft".into()), None)
        .await;
    assert!(report.completed.is_empty());
    assert!(report.failure.is_none());
    assert!(a.dispatch().await.unwrap().is_none());
    let mut temporary = input.clone();
    temporary["variables"][0]["lifetime"] = "temporary".into();
    let document = homesense_config::rule_document::Document::parse(
        &hap::serde_json::to_vec(&temporary).unwrap(),
        &config,
    )
    .unwrap();
    assert!(
        EventRules::from_document(
            a.clone(),
            controller,
            &document,
            Default::default(),
            Default::default(),
            Budget::default()
        )
        .await
        .is_err()
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_panel_document_uses_explicit_double_click_gestures_and_admission() {
    use homesense_core::{Command, DeviceId};
    use homesensed::configured_rules::ConfiguredRules;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let make = |id, event, on| hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"fixture","source":"fixture","trigger":{"kind":"panel","device":"control4:63:key:1","event":event},"actions":[{"source":"fixture:output","action":{"kind":"command","device":"control4:210","command":{"kind":"switch","on":on}}}]});
    let mut value = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[make("single","click",false),make("double","double_click",true)]});
    value["gestures"] = hap::serde_json::json!([{"device":"control4:63:key:1","debounce_ms":30,"long_press_ms":600,"double_click_ms":300,"max_hold_ms":10000,"double_click_enabled":true}]);
    let document = homesense_config::rule_document::Document::parse(
        &hap::serde_json::to_vec(&value).unwrap(),
        &config,
    )
    .unwrap();
    assert!(
        ConfiguredRules::load(
            a.clone(),
            controller,
            &document,
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default()
        )
        .await
        .is_err()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    let key = DeviceId::new("control4:63:key:1").unwrap();
    let mut configured =
        ConfiguredRules::load_document(a.clone(), controller, &document, Default::default())
            .await
            .unwrap();
    configured.panels.edge(&key, true, 100).await.unwrap();
    configured.panels.edge(&key, false, 150).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    configured.panels.edge(&key, true, 200).await.unwrap();
    let result = configured.panels.edge(&key, false, 250).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].rule, "double");
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.command, Command::Switch(true));
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(configured.panels.advance(600).await.unwrap().is_empty());
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn temporary_variables_commit_with_persistent_state_and_reset_on_restart() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType},
    };
    use homesensed::rule_runtime::PreparedRule;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(
        controller,
        generation.clone(),
        "persistent".into(),
        Some(ValueType::Integer),
    )
    .await
    .unwrap();
    a.define_temporary_variable(
        controller,
        generation.clone(),
        "temporary".into(),
        ValueType::Integer,
    )
    .await
    .unwrap();
    assert!(
        a.logical_variable(
            controller,
            generation.clone(),
            "temporary".into(),
            Some(ValueType::Integer)
        )
        .await
        .is_err()
    );
    let names = || vec!["persistent".into(), "temporary".into()];
    let nodes = |value| {
        vec![
            Node {
                source: "p".into(),
                action: Action::Set {
                    variable: "persistent".into(),
                    value: Expr::Literal(Value::Integer(value)),
                },
            },
            Node {
                source: "t".into(),
                action: Action::Set {
                    variable: "temporary".into(),
                    value: Expr::Literal(Value::Integer(value)),
                },
            },
            Node {
                source: "output".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            },
        ]
    };
    let first = PreparedRule::prepare(&a, controller, names(), &nodes(1), Budget::default())
        .await
        .unwrap();
    let stale = PreparedRule::prepare(&a, controller, names(), &nodes(2), Budget::default())
        .await
        .unwrap();
    first.commit(&a).await.unwrap();
    assert!(stale.commit(&a).await.is_err());
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_mixed BEFORE UPDATE ON logical_variable BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let failure = PreparedRule::prepare(&a, controller, names(), &nodes(3), Budget::default())
        .await
        .unwrap();
    assert!(failure.commit(&a).await.is_err());
    sql.execute_batch("DROP TRIGGER fail_mixed;").unwrap();
    a.recover_synchronized(2, 1).await.unwrap();
    let generation = a.task_generation().await.unwrap();
    let snapshot = a
        .variable_snapshot(controller, generation, names())
        .await
        .unwrap();
    assert_eq!(snapshot["temporary"].lifetime, Lifetime::Temporary);
    assert_eq!(snapshot["temporary"].value, Some(Value::Integer(1)));
    assert_eq!(snapshot["persistent"].value, Some(Value::Integer(1)));
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "temporary")
            .unwrap()
            .is_none()
    );
    a.shutdown().await.unwrap();
    let a = actor(&db, controller).await;
    let generation = a.task_generation().await.unwrap();
    assert!(
        a.variable_snapshot(controller, generation.clone(), names())
            .await
            .is_err()
    );
    a.define_temporary_variable(
        controller,
        generation.clone(),
        "temporary".into(),
        ValueType::Integer,
    )
    .await
    .unwrap();
    let snapshot = a
        .variable_snapshot(controller, generation, names())
        .await
        .unwrap();
    assert_eq!(snapshot["temporary"].value, None);
    assert_eq!(snapshot["temporary"].version, 0);
    assert_eq!(snapshot["persistent"].value, Some(Value::Integer(1)));
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn variable_initialization_is_atomic_idempotent_and_does_not_restore_temporary_values() {
    use homesense_core::rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable};
    use homesensed::rule_runtime::PreparedRule;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let define = |scope| Variable {
        value_type: ValueType::Integer,
        lifetime: scope,
        value: None,
    };
    let definitions = || {
        [
            ("a".into(), define(Lifetime::Persistent)),
            ("b".into(), define(Lifetime::Persistent)),
            ("temp".into(), define(Lifetime::Temporary)),
        ]
        .into()
    };
    let generation = a.task_generation().await.unwrap();
    assert!(
        a.initialize_variables(Uuid::new_v4(), generation.clone(), definitions())
            .await
            .is_err()
    );
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_definition BEFORE INSERT ON logical_variable WHEN NEW.name='b' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        a.initialize_variables(controller, generation, definitions())
            .await
            .is_err()
    );
    assert!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "a")
            .unwrap()
            .is_none()
    );
    sql.execute_batch("DROP TRIGGER fail_definition;").unwrap();
    a.recover_synchronized(2, 1).await.unwrap();
    let generation = a.task_generation().await.unwrap();
    assert!(
        a.variable_snapshot(controller, generation.clone(), vec!["temp".into()])
            .await
            .is_err()
    );
    a.initialize_variables(controller, generation.clone(), definitions())
        .await
        .unwrap();
    let actions: Vec<_> = ["a", "temp"]
        .into_iter()
        .map(|name| Node {
            source: "fixture".into(),
            action: Action::Set {
                variable: name.into(),
                value: Expr::Literal(Value::Integer(7)),
            },
        })
        .collect();
    PreparedRule::prepare(
        &a,
        controller,
        vec!["a".into(), "temp".into()],
        &actions,
        Budget::default(),
    )
    .await
    .unwrap()
    .commit(&a)
    .await
    .unwrap();
    let before = a
        .variable_snapshot(
            controller,
            generation.clone(),
            vec!["a".into(), "temp".into()],
        )
        .await
        .unwrap();
    a.initialize_variables(controller, generation.clone(), definitions())
        .await
        .unwrap();
    let after = a
        .variable_snapshot(
            controller,
            generation.clone(),
            vec!["a".into(), "temp".into()],
        )
        .await
        .unwrap();
    assert_eq!(before, after);
    let mut invalid: std::collections::BTreeMap<String, Variable> = definitions();
    invalid.get_mut("temp").unwrap().value = Some(Value::Integer(99));
    assert!(
        a.initialize_variables(controller, generation.clone(), invalid)
            .await
            .is_err()
    );
    let conflict = [
        ("aa".into(), define(Lifetime::Persistent)),
        (
            "b".into(),
            Variable {
                value_type: ValueType::Bool,
                lifetime: Lifetime::Persistent,
                value: None,
            },
        ),
    ]
    .into();
    assert!(
        a.initialize_variables(controller, generation, conflict)
            .await
            .is_err()
    );
    assert!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "aa")
            .unwrap()
            .is_none()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_feedback_cache_uses_lease_and_is_never_persisted() {
    use homesense_core::rule::Budget;
    use homesensed::{
        configured_rules::ConfiguredRules, event_rules::Trigger, feedback::Feedback,
        rule_runtime::PreparedRule, transport_owner::GatewayRead,
    };
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let input = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"actual_level","kind":"integer","lifetime":"feedback_cache","feedback":{"kind":"level","device":"control4:308"}}],"rules":[{"id":"test","enabled":true,"review_evidence":"fixture","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"condition","action":{"kind":"if","condition":{"kind":"less","left":{"kind":"integer","value":30},"right":{"kind":"variable","id":"actual_level"}},"then_actions":[{"source":"output","action":{"kind":"command","device":"control4:210","command":{"kind":"switch","on":true}}}],"else_actions":[]}}]}]});
    let doc = homesense_config::rule_document::Document::parse(
        &hap::serde_json::to_vec(&input).unwrap(),
        &config,
    )
    .unwrap();
    let mut runtime = ConfiguredRules::load(
        a.clone(),
        controller,
        &doc,
        Default::default(),
        Default::default(),
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    assert!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "actual_level")
            .unwrap()
            .is_none()
    );
    let cold = runtime
        .events
        .dispatch(Trigger::StartupSynchronized, None)
        .await;
    assert!(cold.failure.is_none());
    assert!(cold.completed[0].commands.is_empty());
    assert_eq!(cold.completed[0].unknown_branches, ["condition"]);
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let sample = |level| GatewayRead {
        gateway_id: "hopebeauty".into(),
        transport_epoch: 1,
        received_at_ms: 0,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame {
                address: 0x11,
                command: 0xd5,
                value: level,
            }],
        },
    };
    feedback.apply(&sample(40)).unwrap();
    let linked = doc
        .linked_rules(&Default::default(), &Default::default())
        .unwrap();
    let stale = PreparedRule::prepare_with_feedback(
        &a,
        controller,
        vec![],
        &linked[0].actions,
        Budget::default(),
        &feedback,
        5000,
    )
    .await
    .unwrap();
    feedback.apply(&sample(20)).unwrap();
    assert!(stale.commit(&a).await.is_err());
    assert!(a.dispatch().await.unwrap().is_none());
    feedback.apply(&sample(40)).unwrap();
    let live = runtime
        .events
        .dispatch(Trigger::StartupSynchronized, Some((&feedback, 5000)))
        .await;
    assert!(live.failure.is_none());
    assert_eq!(live.completed[0].commands.len(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn cache_change_triggers_compare_properties_and_share_source_event_budget() {
    use homesensed::{
        configured_rules::ConfiguredRules, feedback::Feedback, transport_owner::GatewayRead,
    };
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let rule = |id, variable, device| hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"fixture","source":"fixture","trigger":{"kind":"variable_changed","variable":variable},"actions":[{"source":"output","action":{"kind":"command","device":device,"command":{"kind":"switch","on":true}}}]});
    let input = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[
        {"id":"on","kind":"bool","lifetime":"feedback_cache","feedback":{"kind":"on","device":"control4:308"}},
        {"id":"level","kind":"integer","lifetime":"feedback_cache","feedback":{"kind":"level","device":"control4:308"}}
    ],"rules":[rule("on-change","on","control4:210"),rule("level-change","level","control4:278")]});
    let doc = homesense_config::rule_document::Document::parse(
        &hap::serde_json::to_vec(&input).unwrap(),
        &config,
    )
    .unwrap();
    let mut runtime = ConfiguredRules::load(
        a.clone(),
        controller,
        &doc,
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let sample = |level| GatewayRead {
        gateway_id: "hopebeauty".into(),
        transport_epoch: 1,
        received_at_ms: 0,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame {
                address: 0x11,
                command: 0xd5,
                value: level,
            }],
        },
    };
    feedback.apply(&sample(10)).unwrap();
    runtime.events.bind_feedback(&feedback).unwrap();
    feedback.apply(&sample(20)).unwrap();
    let reports = runtime
        .events
        .consume_feedback(&feedback, 5000)
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].failure.is_none());
    assert_eq!(reports[0].completed.len(), 1);
    assert_eq!(reports[0].completed[0].rule, "level-change");
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    feedback.apply(&sample(0)).unwrap();
    let reports = runtime
        .events
        .consume_feedback(&feedback, 5000)
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].failure.is_none());
    assert_eq!(reports[0].completed.len(), 2);
    assert!(
        reports[0]
            .completed
            .iter()
            .all(
                |run| run.cause.correlation_id() == reports[0].correlation_id
                    && run.causation_id == 0
            )
    );
    for _ in 0..2 {
        let ticket = a.dispatch().await.unwrap().unwrap();
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    feedback.apply(&sample(0)).unwrap();
    assert!(
        runtime
            .events
            .consume_feedback(&feedback, 5000)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(a.dispatch().await.unwrap().is_none());
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    feedback.expire(1, 1).unwrap();
    let expired = runtime.events.consume_feedback(&feedback, 1).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].completed.len(), 2);
    assert!(expired[0].failure.is_none());
    for _ in 0..2 {
        let ticket = a.dispatch().await.unwrap().unwrap();
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn rule_audit_failure_rolls_back_variable_and_command_admission() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Budget, Expr, Node, Value, ValueType},
    };
    use homesensed::event_rules::{Binding, EventRules, Trigger};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let generation = a.task_generation().await.unwrap();
    a.logical_variable(controller, generation, "flag".into(), Some(ValueType::Bool))
        .await
        .unwrap();
    let mut router = EventRules::new(
        a.clone(),
        controller,
        vec![Binding {
            id: "audited".into(),
            trigger: Trigger::StartupSynchronized,
            variables: vec!["flag".into()],
            actions: vec![
                Node {
                    source: "set".into(),
                    action: Action::Set {
                        variable: "flag".into(),
                        value: Expr::Literal(Value::Bool(true)),
                    },
                },
                Node {
                    source: "command".into(),
                    action: Action::Command {
                        device: DeviceId::new("control4:210").unwrap(),
                        command: Command::Switch(true),
                    },
                },
            ],
        }],
        Default::default(),
        Budget::default(),
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_rule_audit BEFORE INSERT ON rule_admission BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let report = router.dispatch(Trigger::StartupSynchronized, None).await;
    assert!(report.failure.is_some());
    assert!(report.completed.is_empty());
    let count:i64=sql.query_row("SELECT (SELECT count(*) FROM rule_admission)+(SELECT count(*) FROM rule_command)+(SELECT count(*) FROM command_journal)",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .logical_variable(1, "flag")
            .unwrap()
            .unwrap()
            .value,
        None
    );
    assert!(a.dispatch().await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn outcome_write_failure_preserves_prior_admission_and_gates_new_dispatch() {
    use homesense_core::{
        Command, DeviceId,
        rule::{Action, Node},
    };
    use homesensed::event_rules::{Binding, EventRules, Trigger};
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let generation = a.task_generation().await.unwrap();
    let mut router = EventRules::new(
        a.clone(),
        controller,
        vec![Binding {
            id: "output".into(),
            trigger: Trigger::StartupSynchronized,
            variables: vec![],
            actions: vec![Node {
                source: "fixture".into(),
                action: Action::Command {
                    device: DeviceId::new("control4:210").unwrap(),
                    command: Command::Switch(true),
                },
            }],
        }],
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_outcome BEFORE INSERT ON rule_chain_outcome BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let report = router.dispatch(Trigger::StartupSynchronized, None).await;
    assert_eq!(report.completed.len(), 1);
    assert!(
        report
            .failure
            .as_ref()
            .unwrap()
            .contains("outcome persistence")
    );
    assert!(a.dispatch().await.is_err());
    let page = a
        .rule_chain(controller, generation, report.correlation_id, 0, 64)
        .await
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert!(page.outcome.is_none());
    assert_eq!(
        page.records[0].commands[0].id,
        report.completed[0].commands[0]
    );
    assert_eq!(page.records[0].commands[0].status, Status::Accepted);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_hold_first_sample_rejects_changed_feedback_and_pending_command() {
    use homesense_core::{Command, DeviceId, LevelTransition, Percent};
    use homesense_hap::requests::{Batch, Intent};
    use homesensed::{
        feedback::Feedback,
        hold_dimming::{HoldDimming, ReleasePolicy},
        transport_owner::GatewayRead,
    };
    for pending in [false, true] {
        let db = database();
        let controller = Uuid::new_v4();
        let a = actor(&db, controller).await;
        let payload = Store::open(&db.0)
            .unwrap()
            .active()
            .unwrap()
            .unwrap()
            .payload;
        let config =
            homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
        let mut feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let sample = |level| GatewayRead {
            gateway_id: "hopebeauty".into(),
            transport_epoch: 1,
            received_at_ms: 0,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x11,
                    command: 0xd5,
                    value: level,
                }],
            },
        };
        feedback.apply(&sample(30)).unwrap();
        let device = DeviceId::new("control4:308").unwrap();
        let key = DeviceId::new("control4:63:key:1").unwrap();
        let manual = if pending {
            Some(
                a.submit(Batch {
                    controller,
                    intents: vec![Intent {
                        device: device.clone(),
                        command: Command::SetLevel {
                            level: Percent::new(70).unwrap(),
                            transition: LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                })
                .await
                .unwrap()[0],
            )
        } else {
            None
        };
        let mut hold = HoldDimming::new(a.clone(), controller, 0, 100, 5).unwrap();
        hold.start_with_feedback(
            key.clone(),
            device,
            true,
            &feedback,
            0,
            5000,
            ReleasePolicy::CancelPendingOnly,
        )
        .await
        .unwrap();
        if !pending {
            feedback.apply(&sample(40)).unwrap();
        }
        assert!(hold.tick(0).await.is_err());
        assert!(hold.tick(100).await.unwrap().is_empty());
        if let Some(id) = manual {
            let ticket = a.dispatch().await.unwrap().unwrap();
            assert_eq!(ticket.command.id, id);
            a.finish_write(id, true).await.unwrap();
        }
        assert!(a.dispatch().await.unwrap().is_none());
        hold.release(&key).await.unwrap();
        assert_eq!(hold.active_count(), 0);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(
            hold.start_with_feedback(
                key.clone(),
                DeviceId::new("control4:308").unwrap(),
                true,
                &feedback,
                100,
                1,
                ReleasePolicy::CancelPendingOnly
            )
            .await
            .is_err()
        );
        assert_eq!(hold.active_count(), 0);
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn file_hold_binding_starts_dimming_and_release_cancels_queued_sample() {
    use homesense_core::{Command, DeviceId};
    use homesensed::{feedback::Feedback, transport_owner::GatewayRead};
    let clock = std::time::Instant::now();
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let document = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&document, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    feedback
        .apply(&GatewayRead {
            gateway_id: "hopebeauty".into(),
            transport_epoch: 1,
            received_at_ms: 0,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x11,
                    command: 0xd5,
                    value: 30,
                }],
            },
        })
        .unwrap();
    let key = DeviceId::new("control4:63:key:1").unwrap();
    let mut input = hap::serde_json::json!({
        "schema_version":1,"configuration_revision":1,"variables":[],"rules":[],
        "gestures":[{"device":"control4:63:key:1","debounce_ms":30,"long_press_ms":600,"double_click_ms":300,"max_hold_ms":10000,"double_click_enabled":false}],
        "holds":{"interval_ms":100,"increment":5,"bindings":[{"key":"control4:63:key:1","device":"control4:308","up":true,"release":"cancel_pending_only"}]}
    });
    let parse = |v: &hap::serde_json::Value| {
        homesense_config::rule_document::Document::parse(
            &hap::serde_json::to_vec(v).unwrap(),
            &config,
        )
    };
    assert!(!parse(&input).unwrap().holds.unwrap().bindings[0].enabled);
    input["holds"]["bindings"][0]["enabled"] = true.into();
    assert!(parse(&input).is_err());
    input["holds"]["bindings"][0]["review_evidence"] = "isolated fixture".into();
    let mut invalid = input.clone();
    invalid["holds"]["interval_ms"] = 1.into();
    assert!(parse(&invalid).is_err());
    invalid = input.clone();
    invalid["gestures"] = hap::serde_json::json!([]);
    assert!(parse(&invalid).is_err());
    invalid = input.clone();
    invalid["holds"]["bindings"][0]["device"] = "control4:210".into();
    assert!(parse(&invalid).is_err());
    let document = parse(&input).unwrap();
    let configured = homesensed::configured_rules::ConfiguredRules::load_document(
        a.clone(),
        id,
        &document,
        Default::default(),
    )
    .await
    .unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    let mut panels = configured.panels;
    let base = clock.elapsed().as_millis() as u64 + 1;
    panels
        .edge_with_feedback(&key, true, base, Some((&feedback, 5000)))
        .await
        .unwrap();
    panels
        .advance_with_feedback(base + 600, Some((&feedback, 5000)))
        .await
        .unwrap();
    let first = panels.tick_holds(base + 600).await.unwrap();
    assert_eq!(first.len(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert!(matches!(ticket.command.command,Command::SetLevel {level,..} if level.get()==35));
    a.finish_write(ticket.command.id, true).await.unwrap();
    let next = panels.tick_holds(base + 700).await.unwrap();
    assert_eq!(next.len(), 1);
    panels
        .edge_with_feedback(&key, false, base + 710, Some((&feedback, 5000)))
        .await
        .unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(panels.tick_holds(base + 800).await.unwrap().is_empty());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_timed_scene_preserves_offsets_and_cancels_future_output() {
    use homesensed::scene_scheduler::SceneScheduler;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let mut input = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"delayed","max_lateness_ms":100,"steps":[
        {"source":"first","offset_ms":100,"device":"control4:210","command":{"kind":"switch","on":true}},
        {"source":"later","offset_ms":200,"device":"control4:278","command":{"kind":"switch","on":true}}
    ]}]});
    let parse = |v: &hap::serde_json::Value| {
        homesense_config::rule_document::Document::parse(
            &hap::serde_json::to_vec(v).unwrap(),
            &config,
        )
    };
    let mut scheduler = SceneScheduler::new(a.clone(), controller, 2, 0).unwrap();
    assert!(
        scheduler
            .start_document(&parse(&input).unwrap(), "delayed", 0)
            .await
            .is_err()
    );
    input["timed_scenes"][0]["enabled"] = true.into();
    assert!(parse(&input).is_err());
    input["timed_scenes"][0]["review_evidence"] = "isolated simulation".into();
    let mut bad = input.clone();
    bad["timed_scenes"][0]["steps"][0]["offset_ms"] = 3600001.into();
    assert!(parse(&bad).is_err());
    bad = input.clone();
    bad["timed_scenes"][0]["steps"][0]["delay_ms"] = 10.into();
    assert!(parse(&bad).is_err());
    let mut document = parse(&input).unwrap();
    document.revision = 2;
    assert!(
        scheduler
            .start_document(&document, "delayed", 0)
            .await
            .is_err()
    );
    document.revision = 1;
    let run = scheduler
        .start_document(&document, "delayed", 0)
        .await
        .unwrap();
    scheduler.tick(99).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    scheduler.tick(100).await.unwrap();
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id.as_str(), "control4:210");
    a.finish_write(ticket.command.id, true).await.unwrap();
    scheduler.cancel(run).await.unwrap();
    scheduler.tick(200).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    assert_eq!(
        scheduler.status(run),
        Some(homesense_core::scene::Status::Partial)
    );
    let page = a
        .scene_history(
            controller,
            a.task_generation().await.unwrap(),
            homesense_storage::scene_run::Query::Latest {
                definition: "delayed".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.record.definition.as_deref(), Some("delayed"));
    assert_eq!(
        page.record.status,
        homesense_storage::scene_run::Status::Partial
    );
    assert_eq!(page.commands.len(), 1);
    scheduler.retire(run).unwrap();
    let results=scheduler.take_results();
    assert_eq!(results.len(),1);
    assert_eq!(results[0].run,run);
    assert_eq!(results[0].definition,"delayed");
    assert_eq!(results[0].status,homesense_core::scene::Status::Partial);
    assert!(!results[0].audit_id.is_empty());
    assert!(scheduler.take_results().is_empty());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_lifecycle_is_durable_and_restart_marks_unfinished_without_replay() {
    use homesense_core::{Command, DeviceId, scene::Step};
    use homesense_storage::scene_run::Status as Audit;
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let steps = || {
        vec![Step {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(true),
            delay_ms: 0,
            source: "receipt-test".into(),
        }]
    };
    let mut completed = SceneRuntime::new(a.clone(), controller, steps(), 0, 100)
        .await
        .unwrap();
    let boot = a.boot_id().to_owned();
    let id = completed.audit_id().to_owned();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.scene_run(&boot, &id).unwrap().unwrap().status,
        Audit::Pending
    );
    completed.poll(0).await.unwrap();
    assert_eq!(
        store.scene_run(&boot, &id).unwrap().unwrap().status,
        Audit::Running
    );
    let links = store.scene_commands(&boot, &id, 0, 1).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].step, 0);
    assert_eq!(links[0].source, "receipt-test");
    assert_eq!(
        links[0].status,
        homesense_storage::journal::Status::Accepted
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    completed.reconcile().await.unwrap();
    assert_eq!(
        store.scene_commands(&boot, &id, 0, 1).unwrap()[0].status,
        homesense_storage::journal::Status::Written
    );
    assert!(
        store
            .scene_commands(&boot, &id, ticket.command.id, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.scene_run(&boot, &id).unwrap().unwrap().status,
        Audit::CommandsWritten
    );
    let unfinished = SceneRuntime::new(a.clone(), controller, steps(), 0, 100)
        .await
        .unwrap();
    let unfinished_id = unfinished.audit_id().to_owned();
    a.shutdown().await.unwrap();
    let next = actor(&db, controller).await;
    let generation = next.task_generation().await.unwrap();
    let query = homesense_storage::scene_run::Query::Run {
        boot: boot.clone(),
        run: id.clone(),
        after: 0,
        limit: 1,
    };
    assert!(
        next.scene_history(Uuid::new_v4(), generation.clone(), query.clone())
            .await
            .is_err()
    );
    let page = next
        .scene_history(controller, generation.clone(), query)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.boot, boot);
    assert_eq!(page.run, id);
    assert_eq!(page.record.status, Audit::CommandsWritten);
    assert_eq!(page.commands.len(), 1);
    assert_eq!(page.next_after, Some(ticket.command.id));
    assert!(
        next.scene_history(
            controller,
            generation.clone(),
            homesense_storage::scene_run::Query::Run {
                boot: boot.clone(),
                run: id.clone(),
                after: 0,
                limit: 129
            }
        )
        .await
        .is_err()
    );

    assert_eq!(
        store.scene_run(&boot, &id).unwrap().unwrap().status,
        Audit::CommandsWritten
    );
    assert_eq!(
        store
            .scene_run(&boot, &unfinished_id)
            .unwrap()
            .unwrap()
            .status,
        Audit::Interrupted
    );
    assert!(next.dispatch().await.unwrap().is_none());
    next.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_checkpoint_failure_gates_dispatch_and_does_not_retry_the_run() {
    use homesense_core::{Command, DeviceId, scene::Step};
    use homesensed::scene_runtime::SceneRuntime;
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let mut scene = SceneRuntime::new(
        a.clone(),
        controller,
        vec![Step {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(true),
            delay_ms: 0,
            source: "failure-test".into(),
        }],
        0,
        100,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_scene_checkpoint BEFORE UPDATE ON scene_run BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(scene.poll(0).await.unwrap_err().contains("checkpoint"));
    assert!(a.dispatch().await.is_err());
    sql.execute_batch("DROP TRIGGER fail_scene_checkpoint;")
        .unwrap();
    assert!(
        scene
            .poll(1)
            .await
            .unwrap_err()
            .contains("audit unavailable")
    );
    let count: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store
            .scene_run(a.boot_id(), scene.audit_id())
            .unwrap()
            .unwrap()
            .status,
        homesense_storage::scene_run::Status::Pending
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn generic_scene_activity_uses_final_time_order_feedback_and_durable_pending() {
    use homesense_core::{Command, DeviceId, LevelTransition, Percent, scene::Step};
    use homesensed::{
        feedback::Feedback, scene_activity::SceneActivity as Activity, scene_runtime::SceneRuntime,
        transport_owner::GatewayRead,
    };
    let db = database();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let payload = Store::open(&db.0)
        .unwrap()
        .active()
        .unwrap()
        .unwrap()
        .payload;
    let config =
        homesense_config::Configuration::parse(&payload, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let steps = [(100, 30), (0, 50)]
        .into_iter()
        .map(|(delay_ms, level)| Step {
            device: DeviceId::new("control4:308").unwrap(),
            command: Command::SetLevel {
                level: Percent::new(level).unwrap(),
                transition: LevelTransition::Immediate,
            },
            delay_ms,
            source: "activity-test".into(),
        })
        .collect();
    let mut scene = SceneRuntime::new(a.clone(), controller, steps, 0, 1000)
        .await
        .unwrap();
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Unknown
    );
    let read = |value| GatewayRead {
        gateway_id: "hopebeauty".into(),
        transport_epoch: 1,
        received_at_ms: 0,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame {
                address: 0x11,
                command: 0xd5,
                value,
            }],
        },
    };
    feedback.apply(&read(30)).unwrap();
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Active
    );
    assert_eq!(
        scene.activity(&feedback, 0).await.unwrap(),
        Activity::Unknown
    );
    feedback.apply(&read(20)).unwrap();
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Inactive
    );
    feedback.apply(&read(30)).unwrap();
    let mut foreign = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::new_v4().as_u128(),
        1,
    )
    .unwrap();
    foreign.apply(&read(30)).unwrap();
    assert_eq!(
        scene.activity(&foreign, 5000).await.unwrap(),
        Activity::Unknown
    );
    scene.poll(0).await.unwrap();
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Pending
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id, true).await.unwrap();
    // A sample predating the write still cannot confirm the target.
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Pending
    );
    a.transport_failed().await.unwrap();
    assert_eq!(
        scene.activity(&feedback, 5000).await.unwrap(),
        Activity::Unknown
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_scene_activity_requires_relay_and_dimmer_but_off_retains_brightness() {
    use homesense_core::{Command, DeviceId, LevelTransition, Percent, scene::Step};
    use homesensed::{
        feedback::Feedback, scene_activity::SceneActivity as Activity, scene_runtime::SceneRuntime,
        transport_owner::GatewayRead,
    };
    let bytes=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"relay"},{"id":"relay","name":"Relay","gateway":"bus","address":"03","channel":4,"enabled":true}]}"#;
    let db = Db(std::env::temp_dir().join(format!("combined-scene-{}.sqlite", Uuid::new_v4())));
    let mut store = Store::open(&db.0).unwrap();
    store
        .publish_configuration(None, bytes, ExecutionContext::Simulation, &[])
        .unwrap();
    drop(store);
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let config =
        homesense_config::Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let step = |level| {
        vec![Step {
            device: DeviceId::new("light").unwrap(),
            command: Command::SetLevel {
                level: Percent::new(level).unwrap(),
                transition: LevelTransition::Immediate,
            },
            delay_ms: 0,
            source: "combined-scene".into(),
        }]
    };
    let on = SceneRuntime::new(a.clone(), controller, step(30), 0, 1000)
        .await
        .unwrap();
    let mut off = SceneRuntime::new(a.clone(), controller, step(0), 0, 1000)
        .await
        .unwrap();
    let read = |address, value| GatewayRead {
        gateway_id: "bus".into(),
        transport_epoch: 1,
        received_at_ms: 0,
        result: hb_transport::Receive::Frames {
            transport_epoch: 1,
            frames: vec![hb_protocol::RawFrame {
                address,
                command: 0xd4,
                value,
            }],
        },
    };
    feedback.apply(&read(0x12, 30)).unwrap();
    assert_eq!(
        on.activity(&feedback, 5000).await.unwrap(),
        Activity::Unknown
    );
    feedback.apply(&read(0x03, 0xf2)).unwrap();
    assert_eq!(
        on.activity(&feedback, 5000).await.unwrap(),
        Activity::Inactive
    );
    assert_eq!(
        off.activity(&feedback, 5000).await.unwrap(),
        Activity::Active
    );
    feedback.apply(&read(0x03, 0xf1)).unwrap();
    assert_eq!(
        on.activity(&feedback, 5000).await.unwrap(),
        Activity::Active
    );
    assert_eq!(
        off.activity(&feedback, 5000).await.unwrap(),
        Activity::Inactive
    );
    off.poll(0).await.unwrap();
    assert_eq!(
        on.activity(&feedback, 5000).await.unwrap(),
        Activity::Pending
    );
    assert_eq!(
        off.activity(&feedback, 5000).await.unwrap(),
        Activity::Pending
    );
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id.as_str(), "relay");
    a.finish_write(ticket.command.id, true).await.unwrap();
    // A pre-write relay sample cannot prove either state even with fresh dimmer data.
    assert_eq!(
        off.activity(&feedback, 5000).await.unwrap(),
        Activity::Pending
    );
    feedback.invalidate().unwrap();
    assert_eq!(
        off.activity(&feedback, 5000).await.unwrap(),
        Activity::Unknown
    );
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_feedback_events_follow_logical_light_and_share_cause() {
    use homesensed::{configured_rules::ConfiguredRules, feedback::Feedback, transport_owner::GatewayRead};
    let bytes=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"relay"},{"id":"relay","name":"Relay","gateway":"bus","address":"03","channel":4,"enabled":true}]}"#;
    let db = Db(std::env::temp_dir().join(format!("combined-events-{}.sqlite", Uuid::new_v4())));
    Store::open(&db.0).unwrap().publish_configuration(None, bytes, ExecutionContext::Simulation, &[]).unwrap();
    let controller = Uuid::new_v4();
    let a = actor(&db, controller).await;
    let config = homesense_config::Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
    let rule = |id, trigger| hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"simulation","source":"test","trigger":trigger,"actions":[]});
    let input = hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[
        {"id":"on","kind":"bool","lifetime":"feedback_cache","feedback":{"kind":"on","device":"light"}},
        {"id":"level","kind":"integer","lifetime":"feedback_cache","feedback":{"kind":"level","device":"light"}}
    ],"rules":[
        rule("on",hap::serde_json::json!({"kind":"variable_changed","variable":"on"})),
        rule("level",hap::serde_json::json!({"kind":"variable_changed","variable":"level"})),
        rule("device",hap::serde_json::json!({"kind":"device_changed","device":"light"}))
    ]});
    let doc = homesense_config::rule_document::Document::parse(&hap::serde_json::to_vec(&input).unwrap(), &config).unwrap();
    let mut runtime = ConfiguredRules::load_document(a.clone(), controller, &doc, Default::default()).await.unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, Uuid::parse_str(a.boot_id()).unwrap().as_u128(), 1).unwrap();
    let sample = |address, value| GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address,command:0xd4,value}]}};
    feedback.apply(&sample(0x12,30)).unwrap();
    runtime.events.bind_feedback(&feedback).unwrap();
    let expected = |reports: Vec<homesensed::event_rules::Report>, mut names: Vec<&str>| {
        assert_eq!(reports.len(),1);
        assert!(reports[0].failure.is_none(), "{:?}", reports[0].failure);
        let mut actual: Vec<_> = reports[0].completed.iter().map(|r|r.rule.as_str()).collect();
        actual.sort(); names.sort(); assert_eq!(actual,names);
        assert!(reports[0].completed.iter().all(|r|r.cause.correlation_id()==reports[0].correlation_id && r.causation_id==0));
    };
    feedback.apply(&sample(0x03,0xf2)).unwrap();
    expected(runtime.events.consume_feedback(&feedback,5000).await.unwrap(),vec!["on","level","device"]);
    // The unpowered dimmer changes its retained value; logical light stays off.
    feedback.apply(&sample(0x12,50)).unwrap();
    assert!(runtime.events.consume_feedback(&feedback,5000).await.unwrap().is_empty());
    feedback.apply(&sample(0x03,0xf1)).unwrap();
    expected(runtime.events.consume_feedback(&feedback,5000).await.unwrap(),vec!["on","level","device"]);
    feedback.apply(&sample(0x12,60)).unwrap();
    expected(runtime.events.consume_feedback(&feedback,5000).await.unwrap(),vec!["level","device"]);
    feedback.apply(&sample(0x12,60)).unwrap();
    assert!(runtime.events.consume_feedback(&feedback,5000).await.unwrap().is_empty());
    feedback.apply(&sample(0x03,0xf2)).unwrap();
    expected(runtime.events.consume_feedback(&feedback,5000).await.unwrap(),vec!["on","level","device"]);
    tokio::time::sleep(std::time::Duration::from_millis(12)).await;
    feedback.expire(10,10).unwrap();
    expected(runtime.events.consume_feedback(&feedback,10).await.unwrap(),vec!["on","level"]);
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn published_automation_assembly_binds_identity_revision_and_generation() {
    use homesensed::automation_runtime::AutomationRuntime;
    let db=Db(std::env::temp_dir().join(format!("assembly-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"manual","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":3}}}]}],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;
    assert!(AutomationRuntime::load(a.clone(),Uuid::new_v4(),1_800_000_000_000,1000,8,Default::default()).await.is_err());
    assert!(AutomationRuntime::load(a.clone(),controller,0,1000,8,Default::default()).await.is_err());
    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    assert_eq!(runtime.revision(),1);
    assert_eq!(Store::open(&db.0).unwrap().logical_variable(1,"mode").unwrap().unwrap().value,None);
    let (_,reports)=runtime.start_rule("manual",None).await.unwrap();
    assert_eq!(reports.len(),1);
    assert_eq!(Store::open(&db.0).unwrap().logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(3)));
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(runtime.start_scene("unknown").await.is_err());
    let run = runtime.start_scene("light").await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(runtime.retire_scene(run).is_err());
    assert_eq!(runtime.cancel_scene(run).await.unwrap(), 1);
    assert_eq!(runtime.scene_status(run), Some(homesense_core::scene::Status::Cancelled));
    assert_eq!(runtime.retire_scene(run).unwrap(), homesense_core::scene::Status::Cancelled);
    assert!(runtime.scene_status(run).is_none());
    assert!(runtime.cancel_scene(run).await.is_err());
    let generation = a.task_generation().await.unwrap();
    let history = a.scene_history(controller, generation, homesense_storage::scene_run::Query::Latest {definition: "light".into()}).await.unwrap().unwrap();
    assert_eq!(history.record.status, homesense_storage::scene_run::Status::Cancelled);
    assert!(history.commands.is_empty());
    runtime.start_scene("light").await.unwrap();
    let stopped = runtime.shutdown().await;
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].1.as_ref().unwrap(), &1);
    assert!(runtime.start_scene("light").await.is_err());
    a.transport_failed().await.unwrap();
    assert!(runtime.start_scene("light").await.is_err());
    drop(runtime);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn running_automation_control_cancels_before_output_and_closes_on_exit() {
    use homesensed::{automation_runtime::AutomationRuntime, automation_control::{Action,Reply}, feedback::Feedback, observed_transport::ObservedTransport, transport_owner::TransportOwner};
    let db=Db(std::env::temp_dir().join(format!("assembly-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;

    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    let control=runtime.control();
    // Abandoned requests queued before the loop starts must not create a run.
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1),control.request(Action::Start("light".into()))).await.is_err());

    let sample=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Wire(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl std::io::Read for Wire {fn read(&mut self,out:&mut [u8])->std::io::Result<usize>{
        if self.0.swap(false,std::sync::atomic::Ordering::SeqCst) {out[..5].copy_from_slice(&[0xae,0x15,0xd1,30,0xee]);Ok(5)} else {Err(std::io::ErrorKind::WouldBlock.into())}
    }}
    impl std::io::Write for Wire {fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("cancelled delayed scene wrote to bus")} fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let transport=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire(sample.clone()),1))].into());
    let mut owner=ObservedTransport::new(transport,feedback);
    let (stop,receive)=tokio::sync::watch::channel(false);
    let client=async {
        let Reply::CalendarStatus { boot, revision, status: Some(calendar) } = control.request(Action::CalendarStatus).await.unwrap() else { panic!("missing calendar status") };
        assert_eq!(boot, a.boot_id());
        assert_eq!(revision, 1);
        assert_eq!(calendar.clock.schedule_count, 0);
        assert!(!calendar.clock.stopped);
        // Querying diagnostics must not manufacture a schedule or scene action.
        assert!(control.subscribe("unknown".into()).await.is_err());
        let mut subscription=control.subscribe("light".into()).await.unwrap();
        let mut extra=Vec::new();
        for _ in 0..15 {extra.push(control.subscribe("light".into()).await.unwrap());}
        assert!(control.subscribe("light".into()).await.unwrap_err().contains("capacity"));
        drop(extra);
        drop(control.subscribe("light".into()).await.unwrap());
        assert_eq!(control.request(Action::LastRun("light".into())).await.unwrap(),Reply::LastRun(None));
        assert_eq!(control.request(Action::SceneActivity("light".into())).await.unwrap(),Reply::Activity(Some(homesensed::scene_activity::SceneActivity::Unknown)));
        assert!(control.request(Action::SceneActivity("unknown".into())).await.is_err());
        assert!(control.request(Action::LastRun("unknown".into())).await.is_err());
        let Reply::Started(run)=control.request(Action::Start("light".into())).await.unwrap() else {panic!("start response")};
        assert_eq!(control.request(Action::Activity(run)).await.unwrap(),Reply::Activity(Some(homesensed::scene_activity::SceneActivity::Unknown)));
        assert_eq!(control.request(Action::Cancel(run)).await.unwrap(),Reply::Cancelled(1));
        assert_eq!(control.request(Action::Status(run)).await.unwrap(),Reply::Status(Some(homesense_core::scene::Status::Cancelled)));
        sample.store(true,std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(1),async {
            loop {
                if control.request(Action::Activity(run)).await.unwrap()==Reply::Activity(Some(homesensed::scene_activity::SceneActivity::Active)){break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(control.request(Action::Status(run)).await.unwrap(),Reply::Status(Some(homesense_core::scene::Status::Cancelled)));
        assert_eq!(control.request(Action::Retire(run)).await.unwrap(),Reply::Retired(homesense_core::scene::Status::Cancelled));
        assert_eq!(control.request(Action::Activity(run)).await.unwrap(),Reply::Activity(None));
        assert_eq!(control.request(Action::SceneActivity("light".into())).await.unwrap(),Reply::Activity(Some(homesensed::scene_activity::SceneActivity::Active)));
        let Reply::LastRun(Some(last))=control.request(Action::LastRun("light".into())).await.unwrap() else {panic!("missing durable receipt")};
        assert_eq!(last.status,homesense_storage::scene_run::Status::Cancelled);
        assert_eq!(last.revision,1);
        assert_eq!(last.boot,a.boot_id());
        assert!(!last.run.is_empty());
        assert!(last.updated_ms>=last.started_ms);
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                let ready=subscription.borrow_and_update().as_ref().is_some_and(|result|result.as_ref().is_ok_and(|state|
                    state.boot==a.boot_id() && state.revision==1 && state.activity==homesensed::scene_activity::SceneActivity::Active && state.last_run.as_ref().is_some_and(|last|last.status==homesense_storage::scene_run::Status::Cancelled)));
                if ready {break;}
                subscription.changed().await.unwrap();
            }
        }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(!subscription.has_changed().unwrap(),"unchanged snapshots must not notify");
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if subscription.borrow_and_update().as_ref().is_some_and(|result|result.as_ref().is_ok_and(|state|state.activity==homesensed::scene_activity::SceneActivity::Unknown)) {break;}
                subscription.changed().await.unwrap();
            }
        }).await.unwrap();
        // A new verified sample revives Activity; execution history stays cancelled.
        sample.store(true,std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if subscription.borrow_and_update().as_ref().is_some_and(|result|result.as_ref().is_ok_and(|state|
                    state.activity==homesensed::scene_activity::SceneActivity::Active && state.last_run.as_ref().is_some_and(|last|last.status==homesense_storage::scene_run::Status::Cancelled))) {break;}
                subscription.changed().await.unwrap();
            }
        }).await.unwrap();
        stop.send(true).unwrap();
        subscription
    };
    let (result,subscription)=tokio::join!(runtime.drive(&mut owner,std::time::Duration::from_millis(1),500,receive,||Ok(1_800_000_000_000)),client);
    result.unwrap();
    assert!(subscription.borrow().as_ref().unwrap().as_ref().unwrap_err().contains("stopped"));
    assert!(control.request(Action::Start("light".into())).await.is_err());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_pauses_old_automation_without_closing_admission() {
    use homesensed::{automation_runtime::AutomationRuntime, automation_control::Action, feedback::Feedback, observed_transport::ObservedTransport, transport_owner::TransportOwner};
    for keypads in [false,true] {
    for responds in [false, true] {
    let db=Db(std::env::temp_dir().join(format!("assembly-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;

    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    let control=runtime.control();

    struct Wire;
    impl std::io::Read for Wire {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for Wire {fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("cancelled delayed scene wrote to bus")} fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let transport=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire,1))].into());
    let mut owner=ObservedTransport::new(transport,feedback);
    let (stop,receive)=tokio::sync::watch::channel(false);

    runtime.start_scene("light").await.unwrap();
    let mut next:hap::serde_json::Value=hap::serde_json::from_slice(devices).unwrap();
    next["revision"]=2.into();
    if keypads {
        next["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
        next["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"panel:1","name":"keypad","gateway":"simulation","address":"41","channel":1,"enabled":true}));
    }
    let policy=if keypads {homesensed::publication_recovery::Synchronization::OutputsAndKeypadEvents} else {homesensed::publication_recovery::Synchronization::CompleteFeedback};
    let mut next_rules:hap::serde_json::Value=hap::serde_json::from_slice(rules).unwrap();
    next_rules["configuration_revision"]=2.into();
    let pause=async {
        let subscription=control.subscribe("light".into()).await.unwrap();
        a.begin_automation_publication(hap::serde_json::to_vec(&next).unwrap(),hap::serde_json::to_vec(&next_rules).unwrap(),1,std::time::Duration::from_secs(3)).await.unwrap();
        stop.send(true).unwrap();
        subscription
    };
    let (result,mut subscription)=tokio::join!(runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)),pause);
    result.unwrap();
    assert!(runtime.publication_paused());
    assert!(subscription.borrow_and_update().as_ref().unwrap().as_ref().unwrap_err().contains("stopped"));
    assert!(subscription.changed().await.is_err(),"old stream must close at publication barrier");
    assert!(control.request(Action::Start("light".into())).await.is_err());
    assert!(control.request(Action::CalendarStatus).await.is_err());
    drop(runtime);
    assert!(owner.drain_publication(&a).await.unwrap().is_none());
    assert_eq!(a.commit_publication().await.unwrap(),2);
    assert!(owner.drain_publication(&a).await.is_err());
    struct QueryWire(Vec<u8>,bool);
    impl std::io::Read for QueryWire {
        fn read(&mut self, out:&mut [u8])->std::io::Result<usize>{
            if self.0.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into())}
            let n=out.len().min(self.0.len()); out[..n].copy_from_slice(&self.0[..n]); self.0.drain(..n); Ok(n)
        }
    }
    impl std::io::Write for QueryWire {
        fn write(&mut self, bytes:&[u8])->std::io::Result<usize>{
            assert_eq!(bytes, &[0xae,0x15,0xa1,0xf2,0xee]);
            if self.1 {self.0.extend([0xae,0x15,0xd1,30,0xee]);} Ok(bytes.len())
        }
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let settings=homesensed::publication_recovery::Settings{
        query_timeout_ms:100,max_age_ms:5000,wall_unix_ms:1_800_000_000_000,calendar_lateness_ms:1000,scene_capacity:8
    };
    // Invalid recovery attempts must not consume the committed publication or
    // reopen admission. The valid attempt below must still be possible.
    for (principal,epoch,complete) in [(Uuid::new_v4(),2,true),(controller,1,true),(controller,2,false)] {
        let streams=if complete {[("simulation".into(),hb_transport::Connection::for_simulation(QueryWire(vec![],true),epoch))].into()} else {std::collections::BTreeMap::new()};
        let rejected=homesensed::publication_recovery::rebuild_with_policy(a.clone(),principal,TransportOwner::new(a.clone(),streams),epoch,settings,policy).await;
        assert!(rejected.is_err());
        assert!(a.task_generation().await.is_err());
    }
    let fresh=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(QueryWire(vec![],responds),2))].into());
    let rebuilt=homesensed::publication_recovery::rebuild_with_policy(a.clone(),controller,fresh,2,homesensed::publication_recovery::Settings{
        query_timeout_ms:100,max_age_ms:5000,wall_unix_ms:1_800_000_000_000,calendar_lateness_ms:1000,scene_capacity:8
    },policy).await;
    if !responds {
        assert!(rebuilt.is_err());
        assert!(a.synchronized(2,2).await.is_err());
        assert!(a.dispatch().await.is_err());
        a.shutdown().await.unwrap();
        continue;
    }
    let (mut next_runtime,mut new_owner)=rebuilt.unwrap();
    if keypads {assert!(new_owner.snapshot().devices[&homesense_core::DeviceId::new("panel:1").unwrap()].observed.is_none());}
    assert_eq!(next_runtime.revision(),2);
    assert!(a.dispatch().await.unwrap().is_none());
    let fresh_control=next_runtime.control();
    let (stop, receive)=tokio::sync::watch::channel(false);
    let inspect=async {
        let homesensed::automation_control::Reply::CalendarStatus { boot, revision, status: Some(status) } = fresh_control.request(Action::CalendarStatus).await.unwrap() else { panic!("missing new calendar") };
        assert_eq!(boot,a.boot_id());
        assert_eq!(revision,2);
        assert!(!status.clock.stopped);
        // The old handle must remain closed even after a replacement is ready.
        assert!(control.request(Action::CalendarStatus).await.is_err());
        stop.send(true).unwrap();
    };
    let (driven,())=tokio::join!(next_runtime.drive(&mut new_owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)),inspect);
    driven.unwrap();
    assert!(fresh_control.request(Action::CalendarStatus).await.is_err());
    drop(next_runtime);
    a.shutdown().await.unwrap();
    }
}
}

#[tokio::test]
async fn automation_control_backpressure_and_shutdown_release_all_waiters() {
    use homesensed::{automation_runtime::AutomationRuntime,automation_control::Action};
    let db=Db(std::env::temp_dir().join(format!("assembly-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;

    let runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();

    let control=runtime.control();
    let mut pending=std::collections::VecDeque::new();
    // Poll each request once to deterministically fill the bounded channel,
    // without relying on task scheduling or sleeping until the timeout.
    for _ in 0..32 {
        let mut request=Box::pin(control.request(Action::Status(1)));
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(request.as_mut(),cx))).await.is_pending());
        pending.push_back(request);
    }
    let rejected=tokio::time::timeout(std::time::Duration::from_millis(100),control.request(Action::Status(1))).await.unwrap();
    assert!(rejected.unwrap_err().contains("busy or closed"));
    assert!(a.dispatch().await.unwrap().is_none());
    drop(runtime);
    while let Some(request)=pending.pop_front() {
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100),request).await.unwrap().is_err());
    }
    assert!(control.request(Action::Status(1)).await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_capacity_reservations_isolate_owners_and_release_unused_slots() {
    use homesensed::scene_scheduler::SceneScheduler;
    let db=database();let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut scheduler=SceneScheduler::new(a.clone(),controller,2,0).unwrap();
    let steps=||vec![homesense_core::scene::Step{device:homesense_core::DeviceId::new("control4:210").unwrap(),command:homesense_core::Command::Switch(true),delay_ms:100,source:"reservation".into()}];
    assert!(scheduler.reserve_slots(0).is_err());
    assert!(scheduler.reserve_slots(3).is_err());
    let mut slots=scheduler.reserve_slots(2).unwrap();
    assert!(scheduler.reserve_slots(1).is_err());
    assert!(scheduler.start(steps(),0,1000).await.is_err());
    let mut other=SceneScheduler::new(a.clone(),controller,2,0).unwrap();
    assert!(other.start_reserved(&mut slots,steps(),0,1000).await.is_err());
    assert_eq!(slots.remaining(),2);
    let run=scheduler.start_reserved(&mut slots,steps(),0,1000).await.unwrap();
    assert_eq!(slots.remaining(),1);
    assert!(scheduler.start(steps(),0,1000).await.is_err());
    drop(slots);
    let mut freed=scheduler.reserve_slots(1).unwrap();
    assert!(scheduler.reserve_slots(1).is_err());
    // Failure consumes only the attempted slot; no stale retry token survives.
    assert!(scheduler.start_reserved(&mut freed,vec![],0,1000).await.is_err());
    assert_eq!(freed.remaining(),0);
    assert!(scheduler.start_reserved(&mut freed,steps(),0,1000).await.is_err());
    let slots=scheduler.reserve_slots(1).unwrap();
    drop(slots);
    scheduler.cancel(run).await.unwrap();scheduler.retire(run).unwrap();
    let slots=scheduler.reserve_slots(2).unwrap();drop(slots);
    scheduler.stop_now();assert!(scheduler.reserve_slots(1).is_err());
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn deferred_admission_validates_definition_and_rolls_back_on_storage_fault() {
    use homesensed::admission::DeferredRule;
    for fault in [false,true] {
        let db=Db(std::env::temp_dir().join(format!("deferred-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
        Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let generation=a.task_generation().await.unwrap();
        let request=|controller,definition:&str|DeferredRule{
            batch:homesense_hap::requests::Batch{controller,intents:vec![],subscriptions:vec![]},
            variables:vec![homesense_storage::variables::Update{revision:1,name:"mode".into(),expected_version:0,value:Some(homesense_core::rule::Value::Integer(1))}],
            audit:homesense_storage::rule_audit::Admission{revision:1,correlation:1,event:1,parent:0,rule:"fixture".into()},
            starts:vec![homesense_storage::scene_run::Start{run:"run".into(),revision:1,definition:definition.into(),action_index:1,source:"fixture:call".into()}],
        };
        assert!(a.submit_deferred_rule(generation.clone(),request(Uuid::new_v4(),"light")).await.is_err());
        assert!(a.submit_deferred_rule(generation.clone(),request(controller,"missing")).await.is_err());
        let configuration=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let mut feedback=homesensed::feedback::Feedback::new(&configuration,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        let sample=|level|homesensed::transport_owner::GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:level}]}};
        feedback.apply(&sample(30)).unwrap();
        let condition=vec![homesense_core::rule::Node{source:"observed".into(),action:homesense_core::rule::Action::If{condition:homesense_core::rule::Condition::Equal(homesense_core::rule::Expr::DeviceLevel(homesense_core::DeviceId::new("simulation:light").unwrap()),homesense_core::rule::Expr::Literal(homesense_core::rule::Value::Integer(30))),then_actions:vec![],else_actions:vec![]}}];
        let (_,changed)=feedback.plan_rule(&condition,&Default::default(),Default::default(),0,5000).unwrap();
        feedback.apply(&sample(40)).unwrap();
        assert!(a.submit_deferred_rule_with_feedback(generation.clone(),request(controller,"light"),Some(changed)).await.is_err());
        let (_,lease)=feedback.plan_rule(&[],&Default::default(),Default::default(),0,5000).unwrap();
        feedback.invalidate().unwrap();
        assert!(a.submit_deferred_rule_with_feedback(generation.clone(),request(controller,"light"),Some(lease)).await.is_err());
        assert_eq!(Store::open(&db.0).unwrap().logical_variable(1,"mode").unwrap().unwrap().version,0);
        assert!(Store::open(&db.0).unwrap().scene_run(a.boot_id(),"run").unwrap().is_none());
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        if fault {sql.execute_batch("CREATE TRIGGER injected BEFORE INSERT ON scene_cause BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();}
        let result=a.submit_deferred_rule(generation,request(controller,"light")).await;
        let store=Store::open(&db.0).unwrap();
        if fault {
            assert!(result.is_err());
            assert!(store.scene_run(a.boot_id(),"run").unwrap().is_none());
            assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,None);
            assert!(a.task_generation().await.is_err());
        } else {
            let accepted=result.unwrap();
            assert!(accepted.command_ids.is_empty());
            assert_eq!(accepted.scenes.len(),1);
            let mut installed=homesensed::scene_runtime::SceneRuntime::from_accepted(a.clone(),accepted.scenes.into_iter().next().unwrap()).await.unwrap();
            assert_eq!(installed.audit_id(),"run");
            let started=store.scene_run(a.boot_id(),"run").unwrap().unwrap().started_ms;
            assert!(installed.poll(started).await.unwrap().is_none());
            assert_eq!(store.scene_run(a.boot_id(),"run").unwrap().unwrap().status,homesense_storage::scene_run::Status::Pending);
            assert_eq!(store.scene_cause(a.boot_id(),"run").unwrap().unwrap().event,1);
            assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
            assert!(a.dispatch().await.unwrap().is_none());
            assert_eq!(installed.poll(started+100).await.unwrap().unwrap().len(),1);
            let queued=a.dispatch().await.unwrap().unwrap();
            assert_eq!(queued.command.origin,Origin::Rule);
            assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),1);
            assert_eq!(sql.query_row("SELECT source FROM scene_step",[],|r|r.get::<_,String>(0)).unwrap(),"fixture:call -> fixture");
            a.finish_write(queued.command.id,true).await.unwrap();
            let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,2,0).unwrap();
            let mut slots=scheduler.reserve_slots(2).unwrap();
            let mut batch=request(controller,"light");batch.variables.clear();batch.audit.event=2;
            batch.starts[0].run="two".into();
            let mut second=batch.starts[0].clone();second.run="three".into();second.action_index=2;batch.starts.push(second);
            let accepted=a.submit_deferred_rule(a.task_generation().await.unwrap(),batch).await.unwrap();
            let now=store.scene_run(a.boot_id(),"two").unwrap().unwrap().started_ms;
            let ids=scheduler.install_accepted(&mut slots,accepted.scenes,now).await.unwrap();
            assert_eq!(ids.len(),2);assert_eq!(slots.remaining(),0);
            assert_eq!(scheduler.status(ids[0]),Some(homesense_core::scene::Status::Cancelled));
            assert_eq!(store.scene_run(a.boot_id(),"two").unwrap().unwrap().status,homesense_storage::scene_run::Status::Cancelled);

            assert!(scheduler.reserve_slots(1).is_err());
            assert!(scheduler.tick(now).await.unwrap().iter().all(|t|t.result.as_ref().unwrap().is_empty()));
            assert!(a.dispatch().await.unwrap().is_none());
            scheduler.tick(now+100).await.unwrap();
            let only=a.dispatch().await.unwrap().unwrap();
            a.finish_write(only.command.id,true).await.unwrap();
            assert!(a.dispatch().await.unwrap().is_none(),"superseded batch member must not emit");
            for id in ids {scheduler.cancel(id).await.unwrap();scheduler.retire(id).unwrap();}
            scheduler.take_results();
            let mut slots=scheduler.reserve_slots(1).unwrap();
            let mut batch=request(controller,"light");batch.variables.clear();batch.audit.event=3;batch.starts[0].run="expired".into();
            let accepted=a.submit_deferred_rule(a.task_generation().await.unwrap(),batch).await.unwrap();
            a.transport_failed().await.unwrap();
            assert!(scheduler.install_accepted(&mut slots,accepted.scenes,now).await.is_err());
            assert!(scheduler.status(3).is_none());
        }
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn deferred_rule_adapter_reserves_before_variables_and_preserves_chain() {
    let db=Db(std::env::temp_dir().join(format!("deferred-adapter-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"trigger","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":1}}},{"source":"call","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"step","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    let mut document:hap::serde_json::Value=hap::serde_json::from_slice(rules).unwrap();
    document["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"after","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"scene_result","scene":"later"},"actions":[{"source":"done","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":2}}}]}));
    document["variables"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"marker","kind":"integer","lifetime":"persistent"}));
    document["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"mode-changed","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"variable_changed","variable":"mode"},"actions":[{"source":"copy","action":{"kind":"set","variable":"marker","value":{"kind":"variable","id":"mode"}}}]}));
    let rules=hap::serde_json::to_vec(&document).unwrap();
    Store::open(&db.0).unwrap().publish_automation(None,devices,&rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,1,0).unwrap();
    let full=scheduler.reserve_slots(1).unwrap();
    let chain=||homesense_core::rule_chain::Chain::new(77,0,60000,Default::default(),32).unwrap();
    let mut first=chain();let root=first.root();
    assert!(matches!(homesensed::deferred_rule::execute_classified(&a,controller,"trigger",&mut first,&root,&mut scheduler,None).await,Err(homesensed::deferred_rule::ExecutionFailure::Runtime(_))));
    let store=Store::open(&db.0).unwrap();
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,None);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(full);
    let mut limited=homesense_core::rule_chain::Chain::new(78,0,60000,homesense_core::rule::Budget{commands:1,..Default::default()},32).unwrap();
    let root=limited.root();
    assert!(matches!(homesensed::deferred_rule::execute_classified(&a,controller,"trigger",&mut limited,&root,&mut scheduler,None).await,Err(homesensed::deferred_rule::ExecutionFailure::Planning(_))));
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,None);
    let mut chain=chain();let root=chain.root();
    let result=homesensed::deferred_rule::execute(&a,controller,"trigger",&mut chain,&root,&mut scheduler,None).await.unwrap();
    assert_eq!(result.cause.correlation_id(),77);assert_eq!(result.causation_id,0);
    assert_eq!(result.changed_variables,vec!["mode"]);assert!(result.command_ids.is_empty());
    assert_eq!(result.scene_runs.len(),1);assert!(a.dispatch().await.unwrap().is_none());
    let page=store.query_scene(1,&homesense_storage::scene_run::Query::Latest{definition:"later".into()}).unwrap().unwrap();
    let cause=store.scene_cause(&page.boot,&page.run).unwrap().unwrap();
    assert_eq!(cause.correlation,77);assert_eq!(cause.event,result.cause.event_id());
    scheduler.tick(page.record.started_ms+100).await.unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin,Origin::Rule);a.finish_write(ticket.command.id,true).await.unwrap();
    scheduler.tick(page.record.started_ms+101).await.unwrap();
    let terminal=scheduler.take_results().pop().unwrap();
    assert!(homesensed::deferred_rule::resume_scene(&a,controller,&mut chain,&root,&terminal,&mut scheduler,None).await.is_err());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
    let mut exhausted=homesense_core::rule_chain::Chain::new(77,0,60000,homesense_core::rule::Budget{nodes:1,..Default::default()},32).unwrap();
    let root=exhausted.root();
    let parent=exhausted.plan(&root,"trigger",&[],&Default::default(),0).unwrap().cause;
    assert!(matches!(homesensed::deferred_rule::resume_scene_classified(&a,controller,&mut exhausted,&parent,&terminal,&mut scheduler,None).await,Err(homesensed::deferred_rule::ExecutionFailure::Planning(homesense_core::rule_chain::Error::Budget))));
    let resumed=homesensed::deferred_rule::resume_scene(&a,controller,&mut chain,&result.cause,&terminal,&mut scheduler,None).await.unwrap();
    assert_eq!(resumed.len(),1);assert_eq!(resumed[0].cause.correlation_id(),77);
    assert_eq!(resumed[0].causation_id,result.cause.event_id());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(2)));
    let version=store.logical_variable(1,"mode").unwrap().unwrap().version;
    assert!(homesensed::deferred_rule::resume_scene(&a,controller,&mut chain,&result.cause,&terminal,&mut scheduler,None).await.unwrap().is_empty());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().version,version);
    assert!(homesensed::deferred_rule::execute(&a,controller,"trigger",&mut chain,&result.cause,&mut scheduler,None).await.is_err(),"same-rule causal cycle must remain rejected");
    scheduler.retire(result.scene_runs[0]).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let mut session=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,Default::default(),60000).await.unwrap();
    let started=session.start("trigger",&mut scheduler,None).await.unwrap();
    assert_eq!(started.len(),2);
    assert_eq!(started[1].causation_id,started[0].cause.event_id());
    assert_eq!(started[1].cause.correlation_id(),started[0].cause.correlation_id());
    assert_eq!(store.logical_variable(1,"marker").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(session.pending_count(),1);assert!(!session.is_closed());
    assert!(session.resume(&terminal,&mut scheduler,None).await.is_err());
    let next=store.query_scene(1,&homesense_storage::scene_run::Query::Latest{definition:"later".into()}).unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    scheduler.tick(next.record.started_ms+100).await.unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();a.finish_write(ticket.command.id,true).await.unwrap();
    scheduler.tick(next.record.started_ms+101).await.unwrap();
    let complete=scheduler.take_results().pop().unwrap();
    assert_eq!(session.resume(&complete,&mut scheduler,None).await.unwrap().len(),2);
    assert_eq!(store.logical_variable(1,"marker").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(2)));
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error IS NULL",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(session.pending_count(),0);assert!(session.is_closed());
    assert!(session.resume(&complete,&mut scheduler,None).await.is_err());
    drop(session);assert!(a.task_generation().await.is_ok());
    scheduler.retire(complete.run).unwrap();
    let mut cancelled=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,Default::default(),60000).await.unwrap();
    let launched=cancelled.start("trigger",&mut scheduler,None).await.unwrap();
    let run=launched[0].scene_runs[0];
    assert_eq!(cancelled.cancel(&mut scheduler).await.unwrap(),1);
    assert!(cancelled.is_closed());assert_eq!(cancelled.pending_count(),0);
    assert!(cancelled.cancel(&mut scheduler).await.is_err());
    drop(cancelled);
    assert!(a.task_generation().await.is_ok());
    assert!(a.dispatch().await.unwrap().is_none());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error = 'causal session cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    scheduler.retire(run).unwrap();
    let cancelled_result=scheduler.take_results().pop().unwrap();
    assert_eq!(cancelled_result.status,homesense_core::scene::Status::Cancelled);
    let mut router=homesensed::deferred_router::Router::new(a.clone(),controller,&scheduler,1).unwrap();
    let (id,_)=router.start("trigger",&mut scheduler,None,Default::default(),60000).await.unwrap();
    assert_eq!(router.retained_count(),1);
    assert!(router.start("trigger",&mut scheduler,None,Default::default(),60000).await.is_err());
    assert_eq!(router.cancel(id,&mut scheduler).await.unwrap(),1);
    assert!(router.cancel(id,&mut scheduler).await.is_err());
    assert!(router.start("trigger",&mut scheduler,None,Default::default(),60000).await.is_err(),"cancelled results retain bounded capacity");
    let terminal=scheduler.take_results().pop().unwrap();
    assert!(router.route(&terminal,&mut scheduler,None).await.unwrap().unwrap().is_empty());
    assert_eq!(router.retained_count(),0);
    assert!(router.route(&terminal,&mut scheduler,None).await.unwrap().is_none());
    scheduler.retire(terminal.run).unwrap();
    assert!(a.task_generation().await.is_ok());
    let (_,outcomes)=router.start("trigger",&mut scheduler,None,Default::default(),60000).await.unwrap();
    let run=outcomes[0].scene_runs[0];
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let (reports,unowned)=router.poll(&mut scheduler,None).await.unwrap();
    assert!(reports.is_empty());assert!(unowned.is_empty());
    let ticket=a.dispatch().await.unwrap().unwrap();a.finish_write(ticket.command.id,true).await.unwrap();
    let (resumed,unowned)=router.poll(&mut scheduler,None).await.unwrap();
    assert!(unowned.is_empty());
    assert_eq!(resumed.len(),2);
    assert_eq!(resumed[0].cause.correlation_id(),outcomes[0].cause.correlation_id());
    assert_eq!(router.retained_count(),0);
    assert!(scheduler.status(run).is_none());
    assert!(scheduler.reserve_slots(1).is_ok());
    router.start("trigger",&mut scheduler,None,Default::default(),50).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let (expired,unowned)=router.poll(&mut scheduler,None).await.unwrap();
    assert!(expired.is_empty());assert!(unowned.is_empty());assert_eq!(router.retained_count(),0);
    assert!(a.dispatch().await.unwrap().is_none(),"expired chain must cancel before overdue scene output");
    assert!(a.task_generation().await.is_ok());
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error='causal deadline exceeded'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    let (_,outcomes)=router.start("trigger",&mut scheduler,None,Default::default(),60000).await.unwrap();
    let closing_run=outcomes[0].scene_runs[0];
    router.close(&mut scheduler).await.unwrap();
    router.close(&mut scheduler).await.unwrap();
    assert!(router.start("trigger",&mut scheduler,None,Default::default(),60000).await.is_err());
    assert_eq!(scheduler.status(closing_run),Some(homesense_core::scene::Status::Cancelled));
    assert!(a.dispatch().await.unwrap().is_none());
    let terminal=scheduler.take_results().pop().unwrap();
    assert!(router.route(&terminal,&mut scheduler,None).await.unwrap().unwrap().is_empty());
    assert_eq!(router.retained_count(),0);
    scheduler.retire(closing_run).unwrap();
    drop(router);assert!(a.task_generation().await.is_ok());
    let mut abandoned=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,Default::default(),60000).await.unwrap();
    abandoned.start("trigger",&mut scheduler,None).await.unwrap();
    drop(abandoned);assert!(a.task_generation().await.is_err());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn deferred_session_records_cycle_failure_and_reports_audit_failure() {
    for fault in [false, true] {
        let db=Db(std::env::temp_dir().join(format!("session-failure-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"cycle","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"variable_changed","variable":"mode"},"actions":[{"source":"set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":1}}}]}]}"#;
        Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,1,0).unwrap();
        let mut session=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,Default::default(),60000).await.unwrap();
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        if fault {sql.execute_batch("CREATE TRIGGER injected_outcome BEFORE INSERT ON rule_chain_outcome BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();}
        let error=session.start("cycle",&mut scheduler,None).await.err().expect("cycle rejected");
        assert_eq!(error.contains("failure audit unavailable"),fault,"{error}");
        assert!(session.is_closed());
        assert_eq!(a.task_generation().await.is_err(),fault);
        assert_eq!(session.failure_isolated(),!fault);
        let store=Store::open(&db.0).unwrap();
        assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
        assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),if fault {0}else{1});
        assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn causal_root_batch_shares_identity_and_node_budget() {
    for limit in [1, 2] {
        let db=Db(std::env::temp_dir().join(format!("causal-batch-{}.sqlite",Uuid::new_v4())));
        let document=hap::serde_json::json!({
            "schema_version":1,"configuration_revision":1,
            "schedules":[{"id":"daily","timezone":"Asia/Shanghai","weekdays":[true,true,true,true,true,true,true],"hour":0,"minute":1}],
            "variables":[{"id":"a","kind":"integer","lifetime":"persistent"},{"id":"b","kind":"integer","lifetime":"persistent"}],
            "rules":(["a","b"].map(|id|hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"schedule","schedule":"daily"},"actions":[{"source":"set","action":{"kind":"set","variable":id,"value":{"kind":"integer","value":1}}}]})))
        });
        Store::open(&db.0).unwrap().publish_automation(None,include_bytes!("../../../config/simulation.json"),&hap::serde_json::to_vec(&document).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,2,0).unwrap();
        let mut router=homesensed::deferred_router::Router::new(a.clone(),controller,&scheduler,2).unwrap();
        assert!(router.start_rules(&[],&mut scheduler,None,Default::default(),60000).await.is_err());
        assert!(a.task_generation().await.is_ok());
        let config=homesense_config::Configuration::parse(include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation).unwrap();
        let document=homesense_config::rule_document::Document::parse(&hap::serde_json::to_vec(&document).unwrap(),&config).unwrap();
        let configured=homesensed::configured_rules::ConfiguredRules::load_document(a.clone(),controller,&document,homesense_core::rule::Budget{nodes:3*limit,..Default::default()}).await.unwrap();
        let due=946_828_860_000;
        let mut calendar=homesensed::event_rules::ScheduledRules::new(configured.events,document.schedules.clone(),due-1,1000).await.unwrap();
        let result=calendar.poll_causal(due,None,&mut router,&mut scheduler).await;
        let store=Store::open(&db.0).unwrap();
        assert_eq!(store.logical_variable(1,"a").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)),"{:?}",result.as_ref().err());
        if limit==1 {
            assert!(result.unwrap().is_empty());
            assert_eq!(store.logical_variable(1,"b").unwrap().unwrap().value,None);
            assert!(a.task_generation().await.is_ok());
        } else {
            let reports=result.unwrap();assert_eq!(reports.len(),2);
            assert_eq!(reports[0].cause.correlation_id(),reports[1].cause.correlation_id());
            assert_eq!(reports[0].causation_id,0);assert_eq!(reports[1].causation_id,0);
            assert_ne!(reports[0].cause.event_id(),reports[1].cause.event_id());
            assert_eq!(store.logical_variable(1,"b").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
            assert_eq!(router.retained_count(),0);assert!(a.task_generation().await.is_ok());
            assert!(calendar.poll_causal(due,None,&mut router,&mut scheduler).await.unwrap().is_empty());
        }
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn panel_feedback_uses_shared_causal_rules_without_duplicate_edges() {
    let source=database();
    let payload=Store::open(&source.0).unwrap().active().unwrap().unwrap().payload;
    let db=Db(std::env::temp_dir().join(format!("panel-causal-{}.sqlite",Uuid::new_v4())));
    let document=hap::serde_json::json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],
        "gestures":[{"device":"control4:63:key:1","debounce_ms":30,"long_press_ms":600,"double_click_ms":300,"max_hold_ms":10000,"double_click_enabled":false}],
        "rules":([("first",1),("second",2)].map(|(id,value)|hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"panel","device":"control4:63:key:1","event":"pressed"},"actions":[{"source":"set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":value}}}]})))
    });
    let bytes=hap::serde_json::to_vec(&document).unwrap();
    Store::open(&db.0).unwrap().publish_automation(None,&payload,&bytes,ExecutionContext::Simulation,&[]).unwrap();
    let config=homesense_config::Configuration::parse(&payload,ExecutionContext::Simulation).unwrap();
    let document=homesense_config::rule_document::Document::parse(&bytes,&config).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut configured=homesensed::configured_rules::ConfiguredRules::load_document(a.clone(),controller,&document,Default::default()).await.unwrap();
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,2,0).unwrap();
    let mut router=homesensed::deferred_router::Router::new(a.clone(),controller,&scheduler,2).unwrap();
    let mut feedback=homesensed::feedback::Feedback::new(&config,ExecutionContext::Live,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let sample=|time,value|homesensed::transport_owner::GatewayRead{gateway_id:"hopebeauty".into(),transport_epoch:1,received_at_ms:time,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x41,command:0xd1,value}]}};
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    feedback.apply(&sample(100,0xf1)).unwrap();configured.panels.bind_feedback(&feedback).unwrap();
    assert!(configured.panels.consume_feedback_causal(&feedback,100,1000,&mut router,&mut scheduler).await.unwrap().is_empty());
    feedback.apply(&sample(101,0xf2)).unwrap();configured.panels.consume_feedback_causal(&feedback,101,1000,&mut router,&mut scheduler).await.unwrap();
    feedback.apply(&sample(140,0xf1)).unwrap();
    let fired=configured.panels.consume_feedback_causal(&feedback,140,1000,&mut router,&mut scheduler).await.unwrap();
    assert_eq!(fired.len(),2);
    assert_eq!(fired[0].cause.as_ref().unwrap().correlation_id(),fired[1].cause.as_ref().unwrap().correlation_id());
    assert!(configured.panels.consume_feedback_causal(&feedback,140,1000,&mut router,&mut scheduler).await.unwrap().is_empty());
    assert_eq!(Store::open(&db.0).unwrap().logical_variable(1,"mode").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(2)));
    assert!(a.dispatch().await.unwrap().is_none());a.shutdown().await.unwrap();
}

#[tokio::test]
async fn successor_partial_failure_preserves_accepted_report_and_root_tracking() {
    for continuation in [false,true] {
    let db=Db(std::env::temp_dir().join(format!("partial-cause-{}.sqlite",Uuid::new_v4())));
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"done","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"begin","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"call","action":{"kind":"scene","id":"later","activate":true}}]},{"id":"first","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"scene_result","scene":"later"},"actions":[{"source":"done","action":{"kind":"set","variable":"done","value":{"kind":"integer","value":1}}}]},{"id":"second","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"scene_result","scene":"later"},"actions":[]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"step","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,include_bytes!("../../../config/simulation.json"),rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,2,0).unwrap();
    let mut chain=homesense_core::rule_chain::Chain::new(42,0,60000,homesense_core::rule::Budget{nodes:6,..Default::default()},32).unwrap();
    let root=chain.root();
    let outcome=homesensed::deferred_rule::execute_classified(&a,controller,"begin",&mut chain,&root,&mut scheduler,None).await.unwrap();
    let store=Store::open(&db.0).unwrap();
    let mut observer=homesensed::deferred_router::Router::new(a.clone(),controller,&scheduler,2).unwrap();
    observer.poll(&mut scheduler,None).await.unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();a.finish_write(ticket.command.id,true).await.unwrap();
    let (_,mut unowned)=observer.poll(&mut scheduler,None).await.unwrap();
    let terminal=unowned.pop().unwrap();
    let failure=homesensed::deferred_rule::resume_scene_classified(&a,controller,&mut chain,&outcome.cause,&terminal,&mut scheduler,None).await.unwrap_err();
    match failure {
        homesensed::deferred_rule::ExecutionFailure::Partial{completed,error}=>{
            assert_eq!(completed.len(),1);assert_eq!(completed[0].rule,"first");
            assert_eq!(completed[0].cause.correlation_id(),42);
            assert!(matches!(*error,homesensed::deferred_rule::ExecutionFailure::Planning(homesense_core::rule_chain::Error::Budget)));
        },other=>panic!("unexpected: {other:?}"),
    }
    assert_eq!(store.logical_variable(1,"done").unwrap().unwrap().value,Some(homesense_core::rule::Value::Integer(1)));
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='second'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    scheduler.retire(terminal.run).unwrap();
    let mut isolated=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,homesense_core::rule::Budget{nodes:2,..Default::default()},60000).await.unwrap();
    assert!(isolated.start_rules(&["begin".into(),"first".into()],&mut scheduler,None).await.is_err());
    assert!(isolated.failure_isolated());assert_eq!(isolated.pending_count(),1);
    let cancelled=scheduler.take_results().pop().unwrap();
    assert_eq!(cancelled.status,homesense_core::scene::Status::Cancelled);
    scheduler.retire(cancelled.run).unwrap();drop(isolated);
    assert!(a.task_generation().await.is_ok());assert!(a.dispatch().await.unwrap().is_none());
    let reports=observer.dispatch_rules(&["begin".into(),"first".into()],&mut scheduler,None,homesense_core::rule::Budget{nodes:2,..Default::default()},60000).await.unwrap();
    assert!(reports.is_empty());assert_eq!(observer.retained_count(),1);
    let (reports,unowned)=observer.poll(&mut scheduler,None).await.unwrap();
    assert!(reports.is_empty());assert!(unowned.is_empty());assert_eq!(observer.retained_count(),0);
    assert!(a.dispatch().await.unwrap().is_none());
    observer.start("begin",&mut scheduler,None,homesense_core::rule::Budget{nodes:2,..Default::default()},60000).await.unwrap();
    observer.poll(&mut scheduler,None).await.unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();a.finish_write(ticket.command.id,true).await.unwrap();
    let (reports,unowned)=observer.poll(&mut scheduler,None).await.unwrap();
    assert!(reports.is_empty());assert!(unowned.is_empty());assert_eq!(observer.retained_count(),0);
    assert!(a.task_generation().await.is_ok());
    assert_eq!(observer.dispatch_rules(&["first".into()],&mut scheduler,None,Default::default(),60000).await.unwrap().len(),1);
    let budget=if continuation {homesense_core::rule::Budget{nodes:6,..Default::default()}} else {Default::default()};
    let mut session=homesensed::deferred_session::Session::new(a.clone(),controller,&scheduler,budget,60000).await.unwrap();
    if continuation {
        session.start("begin",&mut scheduler,None).await.unwrap();
        observer.poll(&mut scheduler,None).await.unwrap();
        let ticket=a.dispatch().await.unwrap().unwrap();a.finish_write(ticket.command.id,true).await.unwrap();
        let (_,mut unowned)=observer.poll(&mut scheduler,None).await.unwrap();
        let terminal=unowned.pop().unwrap();
        assert!(session.resume(&terminal,&mut scheduler,None).await.unwrap_err().contains("1 accepted successor"));
        assert_eq!(session.pending_count(),0,"consumed parent must not remain pending after partial continuation");
    } else {
        assert!(session.start_rules(&["begin".into(),"missing".into()],&mut scheduler,None).await.is_err());
        assert_eq!(session.pending_count(),1,"first accepted root must be tracked before the next root fails");
    }
    assert_eq!(a.task_generation().await.is_err(),!continuation);
    assert_eq!(session.failure_isolated(),continuation);
    a.shutdown().await.unwrap();
}
}

#[tokio::test]
async fn causal_deadline_bounds_immediate_and_delayed_queued_commands() {
    for delayed in [false,true] {
        let db=Db(std::env::temp_dir().join(format!("deadline-queue-{}.sqlite",Uuid::new_v4())));
        let command=hap::serde_json::json!({"kind":"level","value":30,"ramp":false});
        let action=if delayed {hap::serde_json::json!({"kind":"scene","id":"later","activate":true})} else {hap::serde_json::json!({"kind":"command","device":"simulation:light","command":command})};
        let rules=hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"root","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":action}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"step","offset_ms":0,"device":"simulation:light","command":command}]}]});
        Store::open(&db.0).unwrap().publish_automation(None,include_bytes!("../../../config/simulation.json"),&hap::serde_json::to_vec(&rules).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,2,0).unwrap();
        let mut chain=homesense_core::rule_chain::Chain::new(42,0,200,Default::default(),32).unwrap();let root=chain.root();
        homesensed::deferred_rule::execute(&a,controller,"root",&mut chain,&root,&mut scheduler,None).await.unwrap();
        if delayed {
            let mut observer=homesensed::deferred_router::Router::new(a.clone(),controller,&scheduler,2).unwrap();
            observer.poll(&mut scheduler,None).await.unwrap();
        }
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        assert_eq!(sql.query_row("SELECT deadline_ms FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),200);
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        assert!(a.dispatch().await.unwrap().is_none(),"expired output must not dispatch without another scene poll");
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn assembly_can_suppress_startup_without_disabling_manual_scenes() {
    use homesensed::{automation_runtime::{AutomationRuntime,StartupPolicy},automation_control::{Action,Reply},feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let db=Db(std::env::temp_dir().join(format!("startup-policy-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"booted","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"boot","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"set","action":{"kind":"set","variable":"booted","value":{"kind":"integer","value":7}}}]}],"timed_scenes":[{"id":"manual","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":10000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    struct Wire;
    impl std::io::Read for Wire {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for Wire {fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("unexpected output")} fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    for policy in [StartupPolicy::Run,StartupPolicy::Suppress] {
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let mut runtime=AutomationRuntime::load_with_startup(a.clone(),controller,1_800_000_000_000,1000,8,Default::default(),policy).await.unwrap();
        let control=runtime.control();
        let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        let transport=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire,1))].into());
        let mut owner=ObservedTransport::new(transport,feedback);
        let (stop,receive)=tokio::sync::watch::channel(false);
        let client=async {
            let Reply::Started(run)=control.request(Action::Start("manual".into())).await.unwrap() else {panic!("manual unavailable")};
            control.request(Action::Cancel(run)).await.unwrap();
            stop.send(true).unwrap();
        };
        let (result,())=tokio::join!(runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)),client);
        result.unwrap();drop(runtime);a.shutdown().await.unwrap();
    }
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='boot'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(sql.query_row("SELECT value FROM logical_variable WHERE name='booted'",[],|r|r.get::<_,i64>(0)).unwrap(),7);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run WHERE definition='manual'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
}

#[tokio::test]
async fn explicit_disconnect_handoff_keeps_boot_variables_but_retires_old_runtime() {
    use homesensed::{automation_runtime::{AutomationRuntime,StartupPolicy},feedback::Feedback,observed_transport::ObservedTransport,transport_owner::TransportOwner};
    let db=Db(std::env::temp_dir().join(format!("handoff-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"temporary","kind":"integer","lifetime":"temporary"}],"rules":[{"id":"boot","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"set","action":{"kind":"set","variable":"temporary","value":{"kind":"integer","value":7}}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    struct Eof;
    impl std::io::Read for Eof {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Ok(0)}}
    impl std::io::Write for Eof {fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("unexpected write")} fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let transport=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Eof,1))].into());
    let mut owner=ObservedTransport::new(transport,feedback);
    let (_stop,receive)=tokio::sync::watch::channel(false);
    let outcome=runtime.drive_recoverable(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)).await.unwrap();
    assert_eq!(outcome,homesensed::automation_drive::Exit::Disconnected{gateway:"simulation".into()});
    assert!(runtime.start_rule("boot",None).await.is_err());
    drop(runtime);
    // Simulation supervisor supplies the new synchronization boundary explicitly.
    a.recover_synchronized(2,1).await.unwrap();
    let generation=a.task_generation().await.unwrap();
    let value=a.variable_snapshot(controller,generation,vec!["temporary".into()]).await.unwrap();
    assert_eq!(value["temporary"].value,Some(homesense_core::rule::Value::Integer(7)));
    let replacement=AutomationRuntime::load_with_startup(a.clone(),controller,1_800_000_000_000,1000,8,Default::default(),StartupPolicy::Suppress).await.unwrap();
    drop(replacement);a.shutdown().await.unwrap();
}

#[tokio::test]
async fn held_dimming_does_not_resume_old_samples_after_transport_recovery() {
    use homesense_core::{DeviceId,ModuleKind,Observation,Percent};
    use homesense_state::{DeviceState,Quality};
    use homesensed::hold_dimming::{HoldDimming,ReleasePolicy};
    for policy in [ReleasePolicy::CancelPendingOnly,ReleasePolicy::PauseRampFieldVerified] {
        let db=database();let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let key=DeviceId::new("control4:63:key:1").unwrap();let light=DeviceId::new("control4:336").unwrap();
        let state=|level,time|DeviceState{kind:ModuleKind::Dimmer,desired:None,observed:Some(Observation::Level(Percent::new(level).unwrap())),quality:Quality::Confirmed,observed_at_ms:Some(time),last_command_id:None,pending_command:None,version:1};
        let mut hold=HoldDimming::new(a.clone(),controller,0,100,5).unwrap();
        hold.start(key.clone(),light.clone(),true,&state(50,0),0,policy).await.unwrap();
        hold.tick(0).await.unwrap();
        let first=a.dispatch().await.unwrap().unwrap();a.finish_write(first.command.id,true).await.unwrap();
        let pending=hold.tick(100).await.unwrap();assert_eq!(pending.len(),1);
        a.transport_failed().await.unwrap();hold.invalidate();
        a.recover_synchronized(2,1).await.unwrap();
        assert!(hold.tick(200).await.unwrap().is_empty());assert_eq!(hold.active_count(),0);
        assert!(a.dispatch().await.unwrap().is_none(),"neither old dimming nor pause may replay");
        assert_eq!(Store::open(&db.0).unwrap().command_status(a.boot_id(),pending[0]).unwrap(),Some(Status::Cancelled));
        hold.start(key.clone(),light,true,&state(30,200),200,policy).await.unwrap();
        hold.tick(200).await.unwrap();
        let fresh=a.dispatch().await.unwrap().unwrap();
        assert!(matches!(fresh.command.command,homesense_core::Command::SetLevel{level,..} if level.get()==35));
        a.finish_write(fresh.command.id,true).await.unwrap();
        hold.invalidate();a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn rejected_lossy_migration_preserves_old_runtime_and_delayed_scene() {
    use homesensed::automation_runtime::AutomationRuntime;
    let db=Db(std::env::temp_dir().join(format!("migration-preflight-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"fixture","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":1000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    sql.execute("UPDATE logical_variable SET value=17 WHERE name='mode'",[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;
    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    let run=runtime.start_scene("light").await.unwrap();
    let mut next:hap::serde_json::Value=hap::serde_json::from_slice(devices).unwrap();next["revision"]=2.into();
    let mut candidate:hap::serde_json::Value=hap::serde_json::from_slice(rules).unwrap();
    candidate["configuration_revision"]=2.into();candidate["variables"][0]["kind"]="bool".into();
    candidate["variable_migrations"]=hap::serde_json::json!([{"variable":"mode","source_revision":1,"source_type":"integer","conversion":"integer_zero_one_to_bool","review_evidence":"fixture"}]);
    assert!(a.begin_automation_publication(hap::serde_json::to_vec(&next).unwrap(),hap::serde_json::to_vec(&candidate).unwrap(),1,std::time::Duration::from_secs(3)).await.is_err());
    // Rejection must neither invalidate this runtime nor cancel its pending work.
    assert_eq!(runtime.cancel_scene(run).await.unwrap(),1);
    assert!(runtime.start_scene("light").await.is_ok());
    assert_eq!(sql.query_row("SELECT value FROM logical_variable WHERE revision=1 AND name='mode'",[],|r|r.get::<_,i64>(0)).unwrap(),17);
    assert!(Store::open(&db.0).unwrap().revision(2).unwrap().is_none());
    drop(runtime);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn software_fade_requires_feedback_bounds_queue_and_cancels_pending_sample() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,200,50,100,4,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    // No transport is driven: a second tick must not build a queue of samples.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    let (count,deadline):(i64,i64)=sql.query_row("SELECT count(*),max(deadline_ms-updated_ms) FROM command_journal",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(count,1);assert!((1..=300).contains(&deadline));
    fade.cancel().await.unwrap();
    assert!(fade.tick().await.unwrap());
    assert_eq!(sql.query_row("SELECT status FROM command_journal",[],|r|r.get::<_,String>(0)).unwrap(),"cancelled");
    drop(fade);a.shutdown().await.unwrap();
}

#[tokio::test]
async fn software_fade_writes_bounded_monotonic_targets_and_finishes_after_transport() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::{GatewayRead,TransportOwner}};
    for audited in [false, true] {
    let db=Db(std::env::temp_dir().join(format!("fade-wire-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let wire=std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Read for Wire {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for Wire {
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().extend_from_slice(bytes);Ok(bytes.len())}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let mut owner=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire(wire.clone()),1))].into());
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device.clone(),homesense_core::Percent::new(0).unwrap(),200,50,200,4,&feedback,2000).await.unwrap();
    if audited {
        Store::open(&db.0).unwrap().begin_scene_run(a.boot_id(),"fade-scene",1,None,0).unwrap();
        fade.bind_scene("fade-scene".into(),3,"test:fade".into()).unwrap();
        assert!(fade.bind_scene("other".into(),0,"other".into()).is_err());
    }
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if fade.tick().await.unwrap(){break;}
            if let Some(outcome)=owner.step().await.unwrap(){outcome.result.unwrap();}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let bytes=wire.lock().unwrap().clone();
    assert_eq!(bytes.len()%5,0);
    let frames=bytes.chunks_exact(5).collect::<Vec<_>>();
    assert!(!frames.is_empty() && frames.len()<=4);
    assert!(frames.iter().all(|f|f[0]==0xae && f[1]==0x15 && f[2]==1 && f[4]==0xee));
    assert_eq!(frames.last().unwrap()[3],0);
    assert!(frames.windows(2).all(|w|w[0][3]>w[1][3]));
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),frames.len() as i64);
    assert_eq!(feedback.snapshot().devices[&device].observed,Some(homesense_core::Observation::Level(homesense_core::Percent::new(100).unwrap())));
    let sample_count:i64=sql.query_row("SELECT count(*) FROM scene_sample",[],|r|r.get(0)).unwrap();
    assert_eq!(sample_count,if audited {frames.len() as i64} else {0});
    if audited {
        let store=Store::open(&db.0).unwrap();
        let rows=store.scene_commands(a.boot_id(),"fade-scene",0,128).unwrap();
        assert_eq!(rows.len(),frames.len());
        assert!(rows.iter().all(|r|r.step==3 && r.source=="test:fade"));
        assert_eq!(store.scene_run(a.boot_id(),"fade-scene").unwrap().unwrap().status,homesense_storage::scene_run::Status::Pending);
        assert!(fade.bind_scene("late".into(),0,"late".into()).is_err());
    }
    drop(fade);
    assert!(a.task_generation().await.is_ok(),"completed fade must not stop admission");
    a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn software_fade_partial_wire_failure_does_not_admit_successor() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::{GatewayRead,TransportOwner}};
    let db=Db(std::env::temp_dir().join(format!("fade-partial-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let wire=std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Read for Wire {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for Wire {
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let mut output=self.0.lock().unwrap();if output.is_empty(){output.extend_from_slice(&bytes[..2]);Ok(2)}else{Err(std::io::ErrorKind::BrokenPipe.into())}}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let mut owner=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire(wire.clone()),1))].into());
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device.clone(),homesense_core::Percent::new(0).unwrap(),200,50,200,4,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    let outcome=owner.step().await.unwrap().unwrap();
    assert!(outcome.result.is_err());
    assert!(fade.tick().await.is_err());
    assert!(fade.tick().await.is_err());
    fade.cancel().await.unwrap();
    assert!(fade.tick().await.is_err(),"cleanup must not turn failure into success");
    assert_eq!(*wire.lock().unwrap(),[0xae,0x15]);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    let status:String=sql.query_row("SELECT status FROM command_journal",[],|r|r.get(0)).unwrap();
    assert!(matches!(status.as_str(),"failed"|"uncertain"),"{status}");
    drop(fade);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_unfinished_software_fade_closes_output_admission() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-drop-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,200,50,100,4,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    // No transport is driven: a second tick must not build a queue of samples.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    let (count,deadline):(i64,i64)=sql.query_row("SELECT count(*),max(deadline_ms-updated_ms) FROM command_journal",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(count,1);assert!((1..=300).contains(&deadline));
    drop(fade);
    struct NoWrite;
    impl std::io::Read for NoWrite {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for NoWrite {
        fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("abandoned fade reached transport")}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let mut owner=homesensed::transport_owner::TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(NoWrite,1))].into());
    assert!(owner.step().await.is_err());
    a.shutdown().await.unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[tokio::test]
async fn fade_group_reserves_budget_and_releases_cancelled_device() {
    use homesensed::{fade_group::{FadeGroup,Limits,Request},feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut group=FadeGroup::new(a.clone(),controller,Limits{tasks:2,samples:8,targets_per_second:10}).unwrap();
    let request=Request{target:zero,duration_ms:300,interval_ms:100,lateness_ms:100};
    assert!(group.start(device.clone(),Request{interval_ms:50,..request},&feedback,2000).await.unwrap_err().contains("allocation"));
    assert_eq!(group.active_count(),0);
    group.start(device.clone(),request,&feedback,2000).await.unwrap();
    assert!(group.start(device.clone(),request,&feedback,2000).await.unwrap_err().contains("duplicate"));
    assert!(group.start(homesense_core::DeviceId::new("other").unwrap(),request,&feedback,2000).await.unwrap_err().contains("allocation"));
    assert_eq!(group.active_count(),1);
    group.cancel(&device).await.unwrap();
    assert_eq!(group.active_count(),0);
    group.start(device.clone(),request,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    assert!(group.tick().await.unwrap().completed.is_empty());
    group.cancel_all().await.unwrap();
    assert_eq!(group.active_count(),0);
    assert!(group.tick().await.is_err());
    assert!(group.start(device,request,&feedback,2000).await.is_err());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert!(a.task_generation().await.is_ok());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn software_fade_expiry_remains_failed_after_cleanup() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,50,50,0,1,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(70)).await;
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("DeadlineMissed"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();
    assert_eq!(fade.tick().await.unwrap_err(),error);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(fade);a.shutdown().await.unwrap();
}

#[tokio::test]
async fn fade_group_partial_failure_cancels_other_device_without_replay() {
    use homesensed::{fade_group::{FadeGroup,Limits,Request},feedback::Feedback,transport_owner::{GatewayRead,TransportOwner}};
    let db=Db(std::env::temp_dir().join(format!("fade-partial-{}.sqlite",Uuid::new_v4())));
    let devices=br#"{"schema_version":1,"revision":1,"manual_degraded_enabled":false,
      "gateways":[{"id":"simulation","endpoint":"127.0.0.1:50007","modules":[{"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]}],
      "devices":[
        {"id":"simulation:light","name":"First","gateway":"simulation","address":"15","channel":1,"enabled":true},
        {"id":"simulation:second","name":"Second","gateway":"simulation","address":"15","channel":2,"enabled":true}
      ]}"#;
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100},hb_protocol::RawFrame{address:0x15,command:0xd2,value:100}]}}).unwrap();
    let wire=std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Read for Wire {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for Wire {
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let mut output=self.0.lock().unwrap();if output.is_empty(){output.extend_from_slice(&bytes[..2]);Ok(2)}else{Err(std::io::ErrorKind::BrokenPipe.into())}}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let mut owner=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(Wire(wire.clone()),1))].into());
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let mut group=FadeGroup::new(a.clone(),controller,Limits{tasks:2,samples:8,targets_per_second:20}).unwrap();
    let request=Request{target:homesense_core::Percent::new(0).unwrap(),duration_ms:400,interval_ms:100,lateness_ms:200};
    group.start(device.clone(),request,&feedback,2000).await.unwrap();
    group.start(homesense_core::DeviceId::new("simulation:second").unwrap(),request,&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    assert!(group.tick().await.unwrap().completed.is_empty());
    let outcome=owner.step().await.unwrap().unwrap();
    assert!(outcome.result.is_err());
    assert!(group.tick().await.is_err());
    assert_eq!(group.active_count(),0);
    assert!(group.tick().await.is_err());
    assert!(group.start(device,request,&feedback,2000).await.is_err());
    group.cancel_all().await.unwrap();
    assert_eq!(*wire.lock().unwrap(),[0xae,0x15]);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing','written')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(group);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn software_fade_manual_override_before_first_sample_preserves_manual_output() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,500,100,100,5,&feedback,2000).await.unwrap();
    a.submit(homesense_hap::requests::Batch {
        controller,
        intents:vec![homesense_hap::requests::Intent {
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel { level:homesense_core::Percent::new(70).unwrap(), transition:homesense_core::LevelTransition::Immediate },
        }],
        subscriptions:vec![],
    }).await.unwrap();
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("superseded by manual"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();
    assert_eq!(fade.tick().await.unwrap_err(),error);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(fade);
    let ticket=a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin,Origin::Hap);
    a.finish_write(ticket.command.id,true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn fade_group_manual_override_keeps_other_device_running() {
    use homesensed::{fade_group::{FadeGroup,Limits,Request},feedback::Feedback,transport_owner::{GatewayRead}};
    for queued in [false, true] {
    let db=Db(std::env::temp_dir().join(format!("fade-partial-{}.sqlite",Uuid::new_v4())));
    let devices=br#"{"schema_version":1,"revision":1,"manual_degraded_enabled":false,
      "gateways":[{"id":"simulation","endpoint":"127.0.0.1:50007","modules":[{"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]}],
      "devices":[
        {"id":"simulation:light","name":"First","gateway":"simulation","address":"15","channel":1,"enabled":true},
        {"id":"simulation:second","name":"Second","gateway":"simulation","address":"15","channel":2,"enabled":true}
      ]}"#;
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100},hb_protocol::RawFrame{address:0x15,command:0xd2,value:100}]}}).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let mut group=FadeGroup::new(a.clone(),controller,Limits{tasks:2,samples:8,targets_per_second:20}).unwrap();
    let request=Request{target:homesense_core::Percent::new(0).unwrap(),duration_ms:400,interval_ms:100,lateness_ms:200};
    group.start(device.clone(),request,&feedback,2000).await.unwrap();
    group.start(homesense_core::DeviceId::new("simulation:second").unwrap(),request,&feedback,2000).await.unwrap();
    if queued {
        tokio::time::sleep(std::time::Duration::from_millis(110)).await;
        let tick=group.tick().await.unwrap();
        assert!(tick.completed.is_empty());
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='accepted'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    a.submit(homesense_hap::requests::Batch {
        controller,intents:vec![homesense_hap::requests::Intent {
            device:device.clone(),
            command:homesense_core::Command::SetLevel {level:homesense_core::Percent::new(70).unwrap(),transition:homesense_core::LevelTransition::Immediate}
        }],subscriptions:vec![],
    }).await.unwrap();
    let result=group.tick().await.unwrap();
    assert_eq!(result.manually_cancelled,vec![device.clone()]);
    assert!(result.completed.is_empty());
    assert_eq!(group.active_count(),1);
    let manual=a.dispatch().await.unwrap().unwrap();
    assert_eq!(manual.command.origin,Origin::Hap);
    a.finish_write(manual.command.id,true).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let result=group.tick().await.unwrap();
    assert!(result.manually_cancelled.is_empty());
    let remaining=a.dispatch().await.unwrap().unwrap();
    assert_eq!(remaining.command.device_id.as_str(),"simulation:second");
    assert_eq!(remaining.command.origin,Origin::Rule);
    a.finish_write(remaining.command.id,true).await.unwrap();
    group.cancel_all().await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),if queued {1} else {0});
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn software_fade_missing_scene_rejects_output_without_replay() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,200,50,100,4,&feedback,2000).await.unwrap();
    fade.bind_scene("missing".into(),0,"test:fade".into()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("scene no longer active"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();
    assert_eq!(fade.tick().await.unwrap_err(),error);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(fade);a.shutdown().await.unwrap();
}

#[tokio::test]
async fn software_fade_terminal_scene_cancels_queued_sample_without_stopping_admission() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,200,50,100,4,&feedback,2000).await.unwrap();
    Store::open(&db.0).unwrap().begin_scene_run(a.boot_id(),"scene",1,None,0).unwrap();
    fade.bind_scene("scene".into(),0,"test:fade".into()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!fade.tick().await.unwrap());
    Store::open(&db.0).unwrap().checkpoint_scene_run(a.boot_id(),"scene",0,homesense_storage::scene_run::Status::Cancelled,100).unwrap();
    // The transport boundary must reject this command even before the fade polls.
    assert!(a.dispatch().await.unwrap().is_none());
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("scene no longer active"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();
    assert_eq!(fade.tick().await.unwrap_err(),error);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='cancelled'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(fade);
    assert!(a.dispatch().await.unwrap().is_none());
    assert!(a.task_generation().await.is_ok());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn delayed_fade_inherits_scene_manual_fence_instead_of_reacquiring_control() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,vec![homesense_core::scene::Step {
        device:device.clone(),command:homesense_core::Command::SetLevel {level:zero,transition:homesense_core::LevelTransition::Immediate},
        delay_ms:0,source:"delayed-fade".into(),
    }],0,1000).await.unwrap();
    a.submit(homesense_hap::requests::Batch {
        controller,
        intents:vec![homesense_hap::requests::Intent {
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel { level:homesense_core::Percent::new(70).unwrap(), transition:homesense_core::LevelTransition::Immediate },
        }],
        subscriptions:vec![],
    }).await.unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,500,100,100,5,&feedback,2000).await.unwrap();
    scene.bind_fade(0,&mut fade).unwrap();
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("superseded by manual"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();
    assert_eq!(fade.tick().await.unwrap_err(),error);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(fade);
    let ticket=a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin,Origin::Hap);
    a.finish_write(ticket.command.id,true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_owned_fade_completes_or_cancels_with_durable_partial_status() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead,scene_runtime::SceneRuntime};
    use homesense_core::{DeviceId,Percent,Command,LevelTransition,scene::{Step,Status as SceneStatus}};
    for cancel_after in [None,Some(0),Some(1),Some(2)] {
        let db=Db(std::env::temp_dir().join(format!("owned-fade-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
        let device=DeviceId::new("simulation:light").unwrap();
        let target=Percent::new(0).unwrap();
        let mut scene=SceneRuntime::new(a.clone(),controller,vec![Step{device:device.clone(),command:Command::SetLevel{level:target,transition:LevelTransition::Immediate},delay_ms:0,source:"owned".into()}],0,1000).await.unwrap();
        let fade=SoftwareFade::start(a.clone(),controller,device,target,200,50,200,4,&feedback,2000).await.unwrap();
        scene.install_due_fade(fade,0).await.unwrap();
        let mut written=0;
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if cancel_after==Some(written) {scene.cancel().await.unwrap();break;}
                scene.reconcile().await.unwrap();
                if scene.status()==SceneStatus::CommandsWritten {break;}
                if let Some(ticket)=a.dispatch().await.unwrap() {
                    if cancel_after==Some(2) {
                        assert!(scene.cancel().await.unwrap_err().contains("still in flight"));
                        assert_eq!(scene.status(),SceneStatus::Running);
                        a.finish_write(ticket.command.id,true).await.unwrap();
                        written+=1;
                        scene.cancel().await.unwrap();
                        break;
                    }
                    a.finish_write(ticket.command.id,true).await.unwrap();
                    written+=1;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let expected=match cancel_after {None=>SceneStatus::CommandsWritten,Some(0)=>SceneStatus::Cancelled,_=>SceneStatus::Partial};
        assert_eq!(scene.status(),expected);
        let store=Store::open(&db.0).unwrap();
        let audit=store.scene_run(a.boot_id(),scene.audit_id()).unwrap().unwrap();
        assert_eq!(audit.status,match cancel_after {None=>homesense_storage::scene_run::Status::CommandsWritten,Some(0)=>homesense_storage::scene_run::Status::Cancelled,_=>homesense_storage::scene_run::Status::Partial});
        assert_eq!(store.scene_commands(a.boot_id(),scene.audit_id(),0,128).unwrap().len(),written);
        assert!(a.dispatch().await.unwrap().is_none());
        drop(scene);
        assert!(a.task_generation().await.is_ok());
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn scene_owned_fade_sequences_following_step_and_cancels_remaining_work() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead,scene_runtime::SceneRuntime};
    use homesense_core::{DeviceId,Percent,Command,LevelTransition,scene::{Step,Status as SceneStatus}};
    for cancel_after in [None,Some(0),Some(1),Some(2)] {
        let db=Db(std::env::temp_dir().join(format!("owned-fade-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
        let device=DeviceId::new("simulation:light").unwrap();
        let target=Percent::new(0).unwrap();
        let mut scene=SceneRuntime::new(a.clone(),controller,vec![Step{device:device.clone(),command:Command::SetLevel{level:target,transition:LevelTransition::Immediate},delay_ms:0,source:"owned".into()},
            Step{device:device.clone(),command:Command::SetLevel{level:Percent::new(80).unwrap(),transition:LevelTransition::Immediate},delay_ms:0,source:"after-fade".into()}
        ],0,1000).await.unwrap();
        let fade=SoftwareFade::start(a.clone(),controller,device,target,200,50,200,4,&feedback,2000).await.unwrap();
        scene.install_due_fade(fade,0).await.unwrap();
        let mut written=0;
        let started=std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if cancel_after==Some(written) {scene.cancel().await.unwrap();break;}
                if let Err(error)=scene.poll(started.elapsed().as_millis() as u64).await {
                    assert!(error.contains("still in flight"),"{error}");
                }
                if scene.status()==SceneStatus::CommandsWritten {break;}
                if let Some(ticket)=a.dispatch().await.unwrap() {
                    if cancel_after==Some(2) {
                        assert!(scene.cancel().await.unwrap_err().contains("still in flight"));
                        assert_eq!(scene.status(),SceneStatus::Running);
                        a.finish_write(ticket.command.id,true).await.unwrap();
                        written+=1;
                        scene.cancel().await.unwrap();
                        break;
                    }
                    a.finish_write(ticket.command.id,true).await.unwrap();
                    written+=1;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let expected=match cancel_after {None=>SceneStatus::CommandsWritten,Some(0)=>SceneStatus::Cancelled,_=>SceneStatus::Partial};
        assert_eq!(scene.status(),expected);
        let store=Store::open(&db.0).unwrap();
        let audit=store.scene_run(a.boot_id(),scene.audit_id()).unwrap().unwrap();
        assert_eq!(audit.status,match cancel_after {None=>homesense_storage::scene_run::Status::CommandsWritten,Some(0)=>homesense_storage::scene_run::Status::Cancelled,_=>homesense_storage::scene_run::Status::Partial});
        assert_eq!(store.scene_commands(a.boot_id(),scene.audit_id(),0,128).unwrap().len(),written);
        let rows=store.scene_commands(a.boot_id(),scene.audit_id(),0,128).unwrap();
        if cancel_after.is_none() {
            assert!(rows.len()>=2);
            assert!(rows[..rows.len()-1].iter().all(|r|r.step==0 && r.sample.is_some()));
            let last=rows.last().unwrap();
            assert_eq!(last.step,1);
            assert_eq!(last.source,"after-fade");
            assert!(last.sample.is_none());
        } else {
            assert!(rows.iter().all(|r|r.step==0),"cancelled scene admitted following step");
        }

        assert!(a.dispatch().await.unwrap().is_none());
        drop(scene);
        assert!(a.task_generation().await.is_ok());
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn scheduler_drives_owned_fade_and_following_step_with_cancellation() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead,scene_scheduler::SceneScheduler};
    use homesense_core::{DeviceId,Percent,Command,LevelTransition,scene::{Step,Status as SceneStatus}};
    for cancel_after in [None,Some(0),Some(1),Some(2)] {
        let db=Db(std::env::temp_dir().join(format!("owned-fade-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
        let device=DeviceId::new("simulation:light").unwrap();
        let target=Percent::new(0).unwrap();
        let mut scheduler=SceneScheduler::new(a.clone(),controller,4,0).unwrap();
        let run=scheduler.start(vec![Step{device:device.clone(),command:Command::SetLevel{level:target,transition:LevelTransition::Immediate},delay_ms:0,source:"owned".into()},
            Step{device:device.clone(),command:Command::SetLevel{level:Percent::new(80).unwrap(),transition:LevelTransition::Immediate},delay_ms:0,source:"after-fade".into()}
        ],0,1000).await.unwrap();
        let fade=SoftwareFade::start(a.clone(),controller,device,target,200,50,200,4,&feedback,2000).await.unwrap();
        scheduler.install_due_fade(run,fade,0).await.unwrap();
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        let audit_id:String=sql.query_row("SELECT run_id FROM scene_run",[],|r|r.get(0)).unwrap();
        let mut written=0;
        let started=std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if cancel_after==Some(written) {scheduler.cancel(run).await.unwrap();break;}
                for tick in scheduler.tick(started.elapsed().as_millis() as u64).await.unwrap() {
                    tick.result.unwrap();
                }
                if scheduler.status(run).unwrap()==SceneStatus::CommandsWritten {break;}
                if let Some(ticket)=a.dispatch().await.unwrap() {
                    if cancel_after==Some(2) {
                        assert!(scheduler.cancel(run).await.unwrap_err().contains("still in flight"));
                        assert_eq!(scheduler.status(run).unwrap(),SceneStatus::Running);
                        a.finish_write(ticket.command.id,true).await.unwrap();
                        written+=1;
                        scheduler.cancel(run).await.unwrap();
                        break;
                    }
                    a.finish_write(ticket.command.id,true).await.unwrap();
                    written+=1;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let expected=match cancel_after {None=>SceneStatus::CommandsWritten,Some(0)=>SceneStatus::Cancelled,_=>SceneStatus::Partial};
        assert_eq!(scheduler.status(run).unwrap(),expected);
        let store=Store::open(&db.0).unwrap();
        let audit=store.scene_run(a.boot_id(),&audit_id).unwrap().unwrap();
        assert_eq!(audit.status,match cancel_after {None=>homesense_storage::scene_run::Status::CommandsWritten,Some(0)=>homesense_storage::scene_run::Status::Cancelled,_=>homesense_storage::scene_run::Status::Partial});
        assert_eq!(store.scene_commands(a.boot_id(),&audit_id,0,128).unwrap().len(),written);
        let rows=store.scene_commands(a.boot_id(),&audit_id,0,128).unwrap();
        if cancel_after.is_none() {
            assert!(rows.len()>=2);
            assert!(rows[..rows.len()-1].iter().all(|r|r.step==0 && r.sample.is_some()));
            let last=rows.last().unwrap();
            assert_eq!(last.step,1);
            assert_eq!(last.source,"after-fade");
            assert!(last.sample.is_none());
        } else {
            assert!(rows.iter().all(|r|r.step==0),"cancelled scene admitted following step");
        }

        assert!(a.dispatch().await.unwrap().is_none());
        drop(scheduler);
        assert!(a.task_generation().await.is_ok());
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn scheduler_reserves_fade_capacity_across_scenes_and_releases_on_cancel() {
    use homesensed::{scene_scheduler::SceneScheduler,software_fade::SoftwareFade,feedback::Feedback,transport_owner::{GatewayRead}};
    let db=Db(std::env::temp_dir().join(format!("fade-partial-{}.sqlite",Uuid::new_v4())));
    let devices=br#"{"schema_version":1,"revision":1,"manual_degraded_enabled":false,
      "gateways":[{"id":"simulation","endpoint":"127.0.0.1:50007","modules":[{"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]}],
      "devices":[
        {"id":"simulation:light","name":"First","gateway":"simulation","address":"15","channel":1,"enabled":true},
        {"id":"simulation:second","name":"Second","gateway":"simulation","address":"15","channel":2,"enabled":true}
      ]}"#;
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100},hb_protocol::RawFrame{address:0x15,command:0xd2,value:100}]}}).unwrap();
    let mut scheduler=SceneScheduler::new(a.clone(),controller,4,0).unwrap();
    let mut runs=vec![];
    let devices=["simulation:light","simulation:second"].map(|s|homesense_core::DeviceId::new(s).unwrap());
    let zero=homesense_core::Percent::new(0).unwrap();
    for device in &devices {
        runs.push(scheduler.start(vec![homesense_core::scene::Step {
            device:device.clone(),command:homesense_core::Command::SetLevel{level:zero,transition:homesense_core::LevelTransition::Immediate},
            delay_ms:0,source:"budget".into(),
        }],0,1000).await.unwrap());
    }
    let first=SoftwareFade::start(a.clone(),controller,devices[0].clone(),zero,300,50,100,6,&feedback,2000).await.unwrap();
    scheduler.install_due_fade(runs[0],first,0).await.unwrap();
    assert!(matches!(SoftwareFade::start(a.clone(),controller,devices[1].clone(),zero,300,50,100,6,&feedback,2000).await, Err(e) if e.contains("allocation")));
    assert_eq!(scheduler.status(runs[0]),Some(homesense_core::scene::Status::Running));
    assert_eq!(scheduler.status(runs[1]),Some(homesense_core::scene::Status::Pending));
    let mut group=homesensed::fade_group::FadeGroup::new(a.clone(),controller,homesensed::fade_group::Limits{tasks:2,samples:20,targets_per_second:20}).unwrap();
    let request=homesensed::fade_group::Request{target:zero,duration_ms:300,interval_ms:50,lateness_ms:100};
    assert!(group.start(devices[1].clone(),request,&feedback,2000).await.unwrap_err().contains("allocation"));
    assert_eq!(group.active_count(),0);
    assert!(a.task_generation().await.is_ok());
    scheduler.cancel(runs[0]).await.unwrap();
    group.start(devices[1].clone(),request,&feedback,2000).await.unwrap();
    group.cancel(&devices[1]).await.unwrap();
    let second=SoftwareFade::start(a.clone(),controller,devices[1].clone(),zero,300,50,100,6,&feedback,2000).await.unwrap();
    scheduler.install_due_fade(runs[1],second,0).await.unwrap();
    scheduler.cancel(runs[1]).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_rejects_early_or_duplicate_fade_without_closing_admission() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,vec![homesense_core::scene::Step {
        device:device.clone(),command:homesense_core::Command::SetLevel{level:zero,transition:homesense_core::LevelTransition::Immediate},
        delay_ms:100,source:"late".into(),
    }],0,1000).await.unwrap();
    let early=SoftwareFade::start(a.clone(),controller,device.clone(),zero,400,100,100,4,&feedback,2000).await.unwrap();
    assert!(scene.install_due_fade(early,0).await.unwrap_err().contains("no scene step due"));
    assert!(a.task_generation().await.is_ok());
    let fade=SoftwareFade::start(a.clone(),controller,device.clone(),zero,400,100,100,4,&feedback,2000).await.unwrap();
    scene.install_due_fade(fade,100).await.unwrap();
    let duplicate=SoftwareFade::start(a.clone(),controller,device,zero,400,100,100,4,&feedback,2000).await.unwrap();
    assert!(scene.install_due_fade(duplicate,100).await.unwrap_err().contains("pending work"));
    assert!(a.task_generation().await.is_ok());
    assert_eq!(scene.status(),homesense_core::scene::Status::Running);
    scene.cancel().await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_fade_prepares_from_feedback_and_never_falls_back() {
    for fresh in [false,true] {
    let db=Db(std::env::temp_dir().join(format!("fade-config-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":0,"ramp":false},"fade":{"duration_ms":200,"interval_ms":50,"lateness_ms":100}}]}]}"#;
    let document=homesense_config::rule_document::Document::parse(rules,&config).unwrap();
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,4,0).unwrap();
    let run=scheduler.start_document(&document,"fade",0).await.unwrap();
    let mut feedback=homesensed::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    if fresh {
        feedback.apply(&homesensed::transport_owner::GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    }
    let ticks=scheduler.tick_with_feedback(0,Some((&feedback,2000))).await.unwrap();
    assert_eq!(ticks.len(),1);
    assert_eq!(ticks[0].result.is_ok(),fresh);
    let mut written=0;
    if fresh {
        let started=std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            while scheduler.status(run)==Some(homesense_core::scene::Status::Running) {
                for tick in scheduler.tick_with_feedback(started.elapsed().as_millis() as u64,Some((&feedback,2000))).await.unwrap() {tick.result.unwrap();}
                if let Some(ticket)=a.dispatch().await.unwrap() {
                    a.finish_write(ticket.command.id,true).await.unwrap();written+=1;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!((1..=4).contains(&written));
        assert_eq!(scheduler.status(run),Some(homesense_core::scene::Status::CommandsWritten));
    } else {
        assert_eq!(scheduler.status(run),Some(homesense_core::scene::Status::Cancelled));
    }
    assert!(a.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),written);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_sample",[],|r|r.get::<_,i64>(0)).unwrap(),written);
    a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn rejected_fade_snapshot_does_not_disable_independent_manual_control() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-reject-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let sample=|level| GatewayRead {gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:level}]}};
    feedback.apply(&sample(100)).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device.clone(),homesense_core::Percent::new(0).unwrap(),200,50,100,4,&feedback,2000).await.unwrap();
    feedback.apply(&sample(80)).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let error=fade.tick().await.unwrap_err();
    assert!(error.contains("InvalidValueInRequest"));
    assert_eq!(fade.tick().await.unwrap_err(),error);
    fade.cancel().await.unwrap();drop(fade);
    assert!(a.task_generation().await.is_ok());
    assert!(a.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    let ids=a.submit(homesense_hap::requests::Batch {controller,intents:vec![homesense_hap::requests::Intent {device,command:homesense_core::Command::SetLevel {level:homesense_core::Percent::new(40).unwrap(),transition:homesense_core::LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();assert_eq!(ticket.command.id,ids[0]);
    a.finish_write(ids[0],true).await.unwrap();
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_fade_reserves_expanded_frames_in_group_and_scheduler() {
    use homesensed::{fade_group::{FadeGroup,Limits,Request},feedback::Feedback,software_fade::SoftwareFade,scene_scheduler::SceneScheduler,transport_owner::GatewayRead};
    use homesense_core::{DeviceId,Percent,Command,LevelTransition,scene::Step};
    let devices=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"12","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},{"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"relay"},{"id":"relay","name":"Relay","gateway":"bus","address":"03","channel":4,"enabled":true}]}"#;
    let db=Db(std::env::temp_dir().join(format!("fade-expanded-{}.sqlite",Uuid::new_v4())));
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x12,command:0xd4,value:100},hb_protocol::RawFrame{address:0x03,command:0xd4,value:0xf1}]}}).unwrap();
    let device=DeviceId::new("light").unwrap();let target=Percent::new(50).unwrap();
    let request=Request{target,duration_ms:300,interval_ms:50,lateness_ms:200};
    let mut group=FadeGroup::new(a.clone(),controller,Limits{tasks:2,samples:20,targets_per_second:20}).unwrap();
    assert!(group.start(device.clone(),request,&feedback,2000).await.unwrap_err().contains("allocation"));
    assert_eq!(group.active_count(),0);
    let mut scheduler=SceneScheduler::new(a.clone(),controller,2,0).unwrap();
    let run=scheduler.start(vec![Step{device:device.clone(),command:Command::SetLevel{level:target,transition:LevelTransition::Immediate},delay_ms:0,source:"synthetic".into()}],0,1000).await.unwrap();
    assert!(matches!(SoftwareFade::start(a.clone(),controller,device.clone(),target,300,50,200,6,&feedback,2000).await,Err(e) if e.contains("allocation")));
    assert!(a.task_generation().await.is_ok());
    // At 100 ms, two commands per sample fit the 20-command/s allocation.
    group.start(device.clone(),Request{interval_ms:100,..request},&feedback,2000).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    group.tick().await.unwrap();
    let first=a.dispatch().await.unwrap().unwrap();
    assert!(matches!(first.command.command,Command::SetLevel{transition:LevelTransition::Immediate,..}));
    a.finish_write(first.command.id,true).await.unwrap();
    let second=a.dispatch().await.unwrap().unwrap();
    assert_eq!(second.command.command,Command::Switch(true));
    a.finish_write(second.command.id,true).await.unwrap();
    group.cancel(&device).await.unwrap();
    scheduler.cancel(run).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn ad8_software_fade_rejects_before_output_without_disabling_admission() {
    let db=Db(std::env::temp_dir().join(format!("ad8-fade-{}.sqlite",Uuid::new_v4())));
    let mut data:hap::serde_json::Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    data["gateways"][0]["modules"][0]["profile"]="ad8_din_rs485_v2_2_1".into();
    let bytes=hap::serde_json::to_vec(&data).unwrap();
    Store::open(&db.0).unwrap().publish_configuration(None,&bytes,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
    let feedback=homesensed::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let result=homesensed::software_fade::SoftwareFade::start(a.clone(),controller,homesense_core::DeviceId::new("simulation:light").unwrap(),homesense_core::Percent::new(50).unwrap(),300,100,100,3,&feedback,2000).await;
    assert!(matches!(result,Err(ref e) if e.contains("AD8")));
    assert!(a.task_generation().await.is_ok());
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_batch_overlap_preserves_other_devices_and_gates_audit_failure() {
    use hap::serde_json::json;
    use homesensed::{admission::DeferredRule, scene_scheduler::SceneScheduler};
    for fault in [false, true] {
        let db=Db(std::env::temp_dir().join(format!("batch-overlap-{}.sqlite",Uuid::new_v4())));
        let mut devices:hap::serde_json::Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        let mut second=devices["devices"][0].clone();
        second["id"]="simulation:second".into(); second["name"]="second".into(); second["channel"]=2.into();
        devices["devices"].as_array_mut().unwrap().push(second);
        let step=|device:&str,level:u8|json!({"source":"synthetic","offset_ms":0,"device":device,"command":{"kind":"level","value":level,"ramp":false}});
        let rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[
            {"id":"early","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[step("simulation:light",20),step("simulation:second",40)]},
            {"id":"late","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[step("simulation:light",60)]}
        ]});
        Store::open(&db.0).unwrap().publish_automation(None,&hap::serde_json::to_vec(&devices).unwrap(),&hap::serde_json::to_vec(&rules).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4(); let a=actor(&db,controller).await;
        let mut scheduler=SceneScheduler::new(a.clone(),controller,2,0).unwrap();
        let mut slots=scheduler.reserve_slots(2).unwrap();
        let accepted=a.submit_deferred_rule(a.task_generation().await.unwrap(),DeferredRule {
            batch:homesense_hap::requests::Batch{controller,intents:vec![],subscriptions:vec![]},variables:vec![],
            audit:homesense_storage::rule_audit::Admission{revision:1,correlation:1,event:1,parent:0,rule:"synthetic".into()},
            starts:["early","late"].into_iter().enumerate().map(|(index,id)|homesense_storage::scene_run::Start{run:id.into(),revision:1,definition:id.into(),action_index:index,source:"synthetic".into()}).collect(),
        }).await.unwrap();
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        if fault {sql.execute_batch("CREATE TRIGGER fail_overlap BEFORE INSERT ON scene_cancellation BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();}
        let now=Store::open(&db.0).unwrap().scene_run(a.boot_id(),"late").unwrap().unwrap().started_ms;
        let installed=scheduler.install_accepted(&mut slots,accepted.scenes,now).await;
        if fault {
            assert!(installed.is_err());
            assert!(a.task_generation().await.is_err());
            assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        } else {
            installed.unwrap();
            assert_eq!(sql.query_row("SELECT reason FROM scene_cancellation WHERE run_id='early'",[],|r|r.get::<_,String>(0)).unwrap(),"scene_replacement");
            assert_eq!(sql.query_row("SELECT count(*) FROM scene_cancellation WHERE run_id='late'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            let mut written=Vec::new();
            for _ in 0..4 {
                for tick in scheduler.tick(now).await.unwrap() {tick.result.unwrap();}
                while let Some(ticket)=a.dispatch().await.unwrap() {
                    written.push((ticket.command.device_id.as_str().to_owned(),ticket.command.command));
                    a.finish_write(ticket.command.id,true).await.unwrap();
                }
            }
            assert_eq!(written.len(),2);
            assert!(written.iter().any(|(id,command)|id=="simulation:second" && matches!(command,homesense_core::Command::SetLevel{level,..} if level.get()==40)));
            assert!(written.iter().any(|(id,command)|id=="simulation:light" && matches!(command,homesense_core::Command::SetLevel{level,..} if level.get()==60)));
            let store=Store::open(&db.0).unwrap();
            assert_eq!(store.scene_run(a.boot_id(),"early").unwrap().unwrap().status,homesense_storage::scene_run::Status::Partial);
            assert_eq!(store.scene_run(a.boot_id(),"late").unwrap().unwrap().status,homesense_storage::scene_run::Status::CommandsWritten);
        }
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn slow_bus_rejects_excess_fade_rate_without_output_and_releases_capacity() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("slow-fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=Admission::open(db.0.clone(),Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(200),..Default::default()},move |id|id==controller).await.unwrap();
    a.synchronized(1,1).await.unwrap();
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:0}]}}).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let target=homesense_core::Percent::new(50).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),target,1000,100,100,10,&feedback,5000).await.is_err());
    let mut allowed=SoftwareFade::start(a.clone(),controller,device.clone(),target,1000,200,100,5,&feedback,5000).await.unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),target,1000,200,100,5,&feedback,5000).await.is_err());
    allowed.cancel().await.unwrap();
    let mut replacement=SoftwareFade::start(a.clone(),controller,device,target,1000,200,100,5,&feedback,5000).await.unwrap();
    replacement.cancel().await.unwrap();
    assert!(a.task_generation().await.is_ok());
    assert!(a.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn owned_fade_manual_fence_persists_reason_without_cancelling_manual_output() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,vec![homesense_core::scene::Step {
        device:device.clone(),command:homesense_core::Command::SetLevel {level:zero,transition:homesense_core::LevelTransition::Immediate},
        delay_ms:0,source:"delayed-fade".into(),
    }],0,1000).await.unwrap();
    a.submit(homesense_hap::requests::Batch {
        controller,
        intents:vec![homesense_hap::requests::Intent {
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel { level:homesense_core::Percent::new(70).unwrap(), transition:homesense_core::LevelTransition::Immediate },
        }],
        subscriptions:vec![],
    }).await.unwrap();
    let fade=SoftwareFade::start(a.clone(),controller,device,zero,500,100,100,5,&feedback,2000).await.unwrap();
    scene.install_due_fade(fade,0).await.unwrap();
    let error=scene.reconcile().await.unwrap_err();
    assert!(error.contains("superseded by manual"));
    scene.reconcile().await.unwrap();
    let history=a.scene_history(controller,a.task_generation().await.unwrap(),homesense_storage::scene_run::Query::Cancellations{boot:a.boot_id().into(),run:scene.audit_id().into(),after:0,limit:128}).await.unwrap().unwrap();
    assert_eq!(history.cancellations.len(),1);
    assert_eq!(history.cancellations[0].cancellation.reason,homesense_storage::scene_run::CancellationReason::ManualOverride);
    assert_eq!(history.record.status,homesense_storage::scene_run::Status::Cancelled);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    let ticket=a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin,Origin::Hap);
    a.finish_write(ticket.command.id,true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn delayed_scene_manual_override_cancels_only_affected_device() {
    use homesense_core::{DeviceId, Command, Percent, LevelTransition};
    use homesense_hap::requests::{Batch, Intent};
    use homesense_storage::scene_run::{Query, CancellationReason};
    let db=Db(std::env::temp_dir().join(format!("manual-scene-{}.sqlite",Uuid::new_v4())));
    let mut config:hap::serde_json::Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    let mut other=config["devices"][0].clone();
    other["id"]="simulation:other".into();other["channel"]=2.into();
    config["devices"].as_array_mut().unwrap().push(other);
    Store::open(&db.0).unwrap().publish_configuration(None,&hap::serde_json::to_vec(&config).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let device=DeviceId::new("simulation:light").unwrap();
    let other=DeviceId::new("simulation:other").unwrap();
    let command=Command::SetLevel{level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate};
    let steps=vec![(device.clone(),0),(device.clone(),10),(other.clone(),10)].into_iter().map(|(device,delay_ms)|homesense_core::scene::Step{device,command:command.clone(),delay_ms,source:"synthetic".into()}).collect();
    let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,steps,0,1000).await.unwrap();
    a.submit(Batch{controller,intents:vec![Intent{device:device.clone(),command:Command::SetLevel{level:Percent::new(70).unwrap(),transition:LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();
    assert!(scene.poll(0).await.unwrap().is_none());
    assert!(scene.poll(10).await.unwrap().is_some());
    let page=a.scene_history(controller,a.task_generation().await.unwrap(),Query::Cancellations{boot:a.boot_id().into(),run:scene.audit_id().into(),after:0,limit:128}).await.unwrap().unwrap();
    assert_eq!(page.cancellations.len(),1);
    assert_eq!(page.cancellations[0].cancellation.device,device);
    assert_eq!(page.cancellations[0].cancellation.reason,CancellationReason::ManualOverride);
    let first=a.dispatch().await.unwrap().unwrap();
    assert_eq!(first.command.origin,Origin::Hap);
    a.finish_write(first.command.id,true).await.unwrap();
    let second=a.dispatch().await.unwrap().unwrap();
    assert_eq!(second.command.device_id,other);
    a.finish_write(second.command.id,true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    scene.reconcile().await.unwrap();
    assert!(scene.poll(20).await.unwrap().is_none());
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn scene_uncertain_output_cancels_remaining_without_restart_replay() {
    use homesense_core::{DeviceId,Command,Percent,LevelTransition,scene::{Step,Status as SceneStatus}};
    let db=Db(std::env::temp_dir().join(format!("scene-wire-failure-{}.sqlite",Uuid::new_v4())));
    Store::open(&db.0).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let steps=[30,70].into_iter().enumerate().map(|(i,level)|Step{
        device:DeviceId::new("simulation:light").unwrap(),
        command:Command::SetLevel{level:Percent::new(level).unwrap(),transition:LevelTransition::Immediate},
        delay_ms:i as u64*10,source:"synthetic".into(),
    }).collect();
    let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,steps,0,1000).await.unwrap();
    scene.poll(0).await.unwrap().unwrap();
    let ticket=a.dispatch().await.unwrap().unwrap();
    a.finish_write(ticket.command.id,false).await.unwrap();
    scene.reconcile().await.unwrap();
    assert_eq!(scene.status(),SceneStatus::Partial);
    a.shutdown().await.unwrap();
    let reopened=actor(&db,controller).await;
    assert!(reopened.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    let status:String=sql.query_row("SELECT status FROM scene_run WHERE run_id=?1",[scene.audit_id()],|r|r.get(0)).unwrap();
    assert_eq!(status,"partial");
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_scene_cancel_records_intent_once_before_settlement() {
    use homesense_core::{DeviceId,Command,Percent,LevelTransition,scene::Step};
    use homesense_storage::scene_run::{Query,CancellationReason};
    for phase in 0..3 {
        let db=Db(std::env::temp_dir().join(format!("scene-cancel-request-{}.sqlite",Uuid::new_v4())));
        Store::open(&db.0).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller).await;
        let device=DeviceId::new("simulation:light").unwrap();
        let steps=[0,10].into_iter().map(|delay_ms|Step{device:device.clone(),command:Command::SetLevel{level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate},delay_ms,source:"synthetic".into()}).collect();
        let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,steps,0,1000).await.unwrap();
        if phase>0 {scene.poll(0).await.unwrap().unwrap();}
        let writing=if phase==2 {a.dispatch().await.unwrap()} else {None};
        scene.cancel().await.unwrap();
        scene.cancel().await.unwrap();
        let page=a.scene_history(controller,a.task_generation().await.unwrap(),Query::Cancellations{boot:a.boot_id().into(),run:scene.audit_id().into(),after:0,limit:128}).await.unwrap().unwrap();
        assert_eq!(page.cancellations.len(),1);
        assert_eq!(page.cancellations[0].cancellation.reason,CancellationReason::CancelRequested);
        assert_eq!(page.cancellations[0].cancellation.device,device);
        if let Some(ticket)=writing {
            a.finish_write(ticket.command.id,true).await.unwrap();
            scene.reconcile().await.unwrap();
            assert_eq!(scene.status(),homesense_core::scene::Status::Partial);
        } else {assert_eq!(scene.status(),homesense_core::scene::Status::Cancelled);}
        assert!(a.dispatch().await.unwrap().is_none());
        assert!(scene.poll(20).await.unwrap().is_none());
        a.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn explicit_cancel_audit_failure_blocks_output_and_restart_replay() {
    use homesense_core::{DeviceId,Command,Percent,LevelTransition,scene::Step};
    let db=Db(std::env::temp_dir().join(format!("scene-cancel-failure-{}.sqlite",Uuid::new_v4())));
    Store::open(&db.0).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let steps=[0,10].into_iter().map(|delay_ms|Step{device:DeviceId::new("simulation:light").unwrap(),command:Command::SetLevel{level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate},delay_ms,source:"synthetic".into()}).collect();
    let mut scene=homesensed::scene_runtime::SceneRuntime::new(a.clone(),controller,steps,0,1000).await.unwrap();
    scene.poll(0).await.unwrap().unwrap();
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_explicit_cancel BEFORE INSERT ON scene_cancellation WHEN NEW.reason='cancel_requested' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(scene.cancel().await.is_err());
    assert!(a.dispatch().await.is_err());
    assert!(scene.poll(20).await.is_err());
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_cancellation",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='writing'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    a.shutdown().await.unwrap();
    sql.execute_batch("DROP TRIGGER fail_explicit_cancel;").unwrap();
    let reopened=actor(&db,controller).await;
    assert!(reopened.dispatch().await.unwrap().is_none());
    assert_eq!(sql.query_row("SELECT status FROM scene_run WHERE run_id=?1",[scene.audit_id()],|r|r.get::<_,String>(0)).unwrap(),"interrupted");
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn scheduler_stop_barrier_preserves_cancellation_request_for_cleanup() {
    use homesense_core::{DeviceId,Command,Percent,LevelTransition,scene::Step};
    let db=Db(std::env::temp_dir().join(format!("scheduler-stop-audit-{}.sqlite",Uuid::new_v4())));
    Store::open(&db.0).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller).await;
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,1,0).unwrap();
    scheduler.start(vec![Step{device:DeviceId::new("simulation:light").unwrap(),command:Command::SetLevel{level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate},delay_ms:10,source:"synthetic".into()}],0,1000).await.unwrap();
    scheduler.stop_now();
    assert!(scheduler.tick(20).await.is_err());
    for _ in 0..2 {
        assert!(scheduler.stop().await.into_iter().all(|(_,r)|r.is_ok()));
        assert!(a.dispatch().await.unwrap().is_none());
    }
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_cancellation WHERE reason='cancel_requested'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(sql.query_row("SELECT status FROM scene_run",[],|r|r.get::<_,String>(0)).unwrap(),"cancelled");
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    a.shutdown().await.unwrap();
}
