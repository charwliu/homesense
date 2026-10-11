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
async fn scheduler_stop_invalidates_all_runs_even_when_first_durable_cancel_fails() {
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
    assert!(results.iter().find(|r| r.0 == first).unwrap().1.is_err());
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
    // Deliberately omit scheduler.manual_override: actor must close the race.
    assert!(old.poll(100).await.is_err());
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
    feedback.expire(0, 5000).unwrap();
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
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    Store::open(&db.0).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();
    let a=actor(&db,controller).await;
    assert!(AutomationRuntime::load(a.clone(),Uuid::new_v4(),1_800_000_000_000,1000,8,Default::default()).await.is_err());
    assert!(AutomationRuntime::load(a.clone(),controller,0,1000,8,Default::default()).await.is_err());
    let mut runtime=AutomationRuntime::load(a.clone(),controller,1_800_000_000_000,1000,8,Default::default()).await.unwrap();
    assert_eq!(runtime.revision(),1);
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
        stop.send(true).unwrap();
    };
    let (result,())=tokio::join!(runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)),client);
    result.unwrap();
    assert!(control.request(Action::Start("light".into())).await.is_err());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    a.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_pauses_old_automation_without_closing_admission() {
    use homesensed::{automation_runtime::AutomationRuntime, automation_control::Action, feedback::Feedback, observed_transport::ObservedTransport, transport_owner::TransportOwner};
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
    let mut next_rules:hap::serde_json::Value=hap::serde_json::from_slice(rules).unwrap();
    next_rules["configuration_revision"]=2.into();
    let pause=async {
        a.begin_automation_publication(hap::serde_json::to_vec(&next).unwrap(),hap::serde_json::to_vec(&next_rules).unwrap(),1,std::time::Duration::from_secs(3)).await.unwrap();
        stop.send(true).unwrap();
    };
    let (result,())=tokio::join!(runtime.drive(&mut owner,std::time::Duration::from_millis(1),5000,receive,||Ok(1_800_000_000_000)),pause);
    result.unwrap();
    assert!(runtime.publication_paused());
    assert!(control.request(Action::Start("light".into())).await.is_err());
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
        let rejected=homesensed::publication_recovery::rebuild(a.clone(),principal,TransportOwner::new(a.clone(),streams),epoch,settings).await;
        assert!(rejected.is_err());
        assert!(a.task_generation().await.is_err());
    }
    let fresh=TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(QueryWire(vec![],responds),2))].into());
    let rebuilt=homesensed::publication_recovery::rebuild(a.clone(),controller,fresh,2,homesensed::publication_recovery::Settings{
        query_timeout_ms:100,max_age_ms:5000,wall_unix_ms:1_800_000_000_000,calendar_lateness_ms:1000,scene_capacity:8
    }).await;
    if !responds {
        assert!(rebuilt.is_err());
        assert!(a.synchronized(2,2).await.is_err());
        assert!(a.dispatch().await.is_err());
        a.shutdown().await.unwrap();
        continue;
    }
    let (next_runtime,_new_owner)=rebuilt.unwrap();
    assert_eq!(next_runtime.revision(),2);
    assert!(a.dispatch().await.unwrap().is_none());
    drop(next_runtime);
    a.shutdown().await.unwrap();
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
