use homesense_core::{
    Command, DeviceId, ExecutionContext, Observation, Percent,
    queue::{Epochs, Origin, QueuedCommand},
    rule::{Value, ValueType},
};
use homesense_storage::{Store, journal::Status, scene_run::ManualStart, variables::Update};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    indicator_sync::Synchronizer,
    scene_scheduler::SceneScheduler,
    transport_owner::GatewayRead,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
fn id(s: &str) -> DeviceId {
    DeviceId::new(s).unwrap()
}
fn devices() -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
    {"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},
    {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
    {"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}},
    {"address":"42","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[
    {"id":"light","name":"Dimmer","gateway":"bus","address":"15","channel":1,"enabled":true},
    {"id":"switch","name":"Switch","gateway":"bus","address":"03","channel":1,"enabled":true},
    {"id":"ledA","name":"Panel A","gateway":"bus","address":"41","channel":1,"enabled":true},
    {"id":"ledB","name":"Panel B","gateway":"bus","address":"42","channel":3,"enabled":true}]})).unwrap()
}
fn rules() -> serde_json::Value {
    let scenes=[("reading",50),("daily",100),("off",0)].map(|(name,level)|json!({"id":name,"enabled":true,"review_evidence":"synthetic only","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"light","command":{"kind":"level","value":level,"ramp":false}},{"source":"fixture","offset_ms":60000,"device":"switch","command":{"kind":"switch","on":name=="reading"}}]}));
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":scenes,
    "scene_switch_groups":[{"id":"room","variable":"mode","review_evidence":"synthetic group","members":[{"scene":"reading","value":1,"off_scene":"off"},{"scene":"daily","value":2,"off_scene":"off"}]}],
    "indicator_links":[{"id":"reading-off","enabled":true,"review_evidence":"synthetic fanout to two panels","lights":["light","switch"],"leds":["ledA","ledB"],"scene":"reading"}]})
}
fn sample(feedback: &mut Feedback, level: u8, on: bool) {
    sample_at(feedback, level, on, 0);
}
fn sample_at(feedback: &mut Feedback, level: u8, on: bool, at: u64) {
    feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: level,
                    },
                    hb_protocol::RawFrame {
                        address: 3,
                        command: 0xd1,
                        value: if on { 0xf1 } else { 0xf2 },
                    },
                ],
            },
        })
        .unwrap();
}
fn record(store: &mut Store, boot: &str, level: u8) {
    let command = QueuedCommand {
        id: 1,
        device_id: id("light"),
        command: Command::SetLevel {
            level: Percent::new(level).unwrap(),
            transition: homesense_core::LevelTransition::Immediate,
        },
        origin: Origin::Hap,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        deadline_ms: 1000,
        run_id: None,
    };
    store.accept_commands(boot, &[command], 0).unwrap();
    store
        .transition_command(boot, 1, Status::Accepted, Status::Writing, 1)
        .unwrap();
    store
        .transition_command(boot, 1, Status::Writing, Status::Written, 2)
        .unwrap();
}
#[test]
fn validation_requires_reviewed_unique_logical_lights_and_explicit_led_mode() {
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let parse = |v: &serde_json::Value| {
        homesense_config::rule_document::Document::parse(&serde_json::to_vec(v).unwrap(), &config)
    };
    assert!(parse(&rules()).is_ok());
    for (field, value) in [
        ("review_evidence", json!("")),
        ("lights", json!([])),
        ("lights", json!(["ledA"])),
        ("leds", json!(["light"])),
        ("leds", json!(["ledA", "ledA"])),
        ("scene", json!("off")),
    ] {
        let mut v = rules();
        v["indicator_links"][0][field] = value;
        assert!(parse(&v).is_err(), "{field}");
    }
    let mut v = rules();
    v["indicator_links"][0]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    v["indicator_links"][0]["review_evidence"] = json!(null);
    assert!(!parse(&v).unwrap().indicator_links[0].enabled);
    let mut raw: serde_json::Value = serde_json::from_slice(&devices()).unwrap();
    raw["gateways"][0]["modules"][2]["led_mode"] = "self_reset".into();
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&raw).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        homesense_config::rule_document::Document::parse(
            &serde_json::to_vec(&rules()).unwrap(),
            &config
        )
        .is_err()
    );
}
#[test]
fn measured_switches_and_recorded_dimmers_preserve_raw_feedback_and_quality() {
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 1, 1).unwrap();
    sample(&mut feedback, 36, false);
    let raw = feedback.snapshot();
    let recorded = BTreeMap::from([(id("light"), Percent::new(30).unwrap())]);
    let published = homesensed::light_state::project(&config, &raw, &recorded).unwrap();
    assert_eq!(
        raw.devices[&id("light")].observed,
        Some(Observation::Level(Percent::new(36).unwrap()))
    );
    assert_eq!(
        published.devices[&id("light")].observed,
        Some(Observation::Level(Percent::new(30).unwrap()))
    );
    assert_eq!(
        published.devices[&id("switch")].observed,
        Some(Observation::Switch(false))
    );
    assert_eq!(
        published.devices[&id("light")].quality,
        raw.devices[&id("light")].quality
    );
    let mut combined: serde_json::Value = serde_json::from_slice(&devices()).unwrap();
    combined["devices"][0]["supply_relay"] = "switch".into();
    combined["devices"][0]["default_on_level"] = 100.into();
    let combined = homesense_config::Configuration::parse(
        &serde_json::to_vec(&combined).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(
        homesensed::light_state::project(&combined, &raw, &recorded)
            .unwrap()
            .devices[&id("light")]
            .observed,
        Some(Observation::Level(Percent::new(0).unwrap()))
    );
    let mut missing = raw.clone();
    missing.devices.get_mut(&id("switch")).unwrap().observed = None;
    assert_eq!(
        homesensed::light_state::project(&combined, &missing, &recorded)
            .unwrap()
            .devices[&id("light")]
            .observed,
        None
    );
    feedback.invalidate().unwrap();
    assert_eq!(
        homesensed::light_state::project(&config, &feedback.snapshot(), &recorded)
            .unwrap()
            .devices[&id("light")]
            .quality,
        homesense_state::Quality::Stale
    );
}
#[tokio::test]
async fn one_expected_on_light_off_fans_out_once_and_clears_only_its_selected_scene() {
    let root = std::env::temp_dir().join(format!("indicator-sync-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let document = homesense_config::rule_document::Document::parse(
        &serde_json::to_vec(&rules()).unwrap(),
        &config,
    )
    .unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &devices(),
            &serde_json::to_vec(&rules()).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("mode".into(), ValueType::Integer)])
        .unwrap();
    store
        .begin_manual_scene_batch_with_variables(
            "old",
            1,
            &[ManualStart {
                run: "initial".into(),
                definition: "reading".into(),
            }],
            0,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 0,
                value: Some(Value::Integer(1)),
            }],
        )
        .unwrap();
    record(&mut store, "setting", 50);
    let controller = uuid::Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |c| c == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut scenes = SceneScheduler::new(admission.clone(), controller, 8, 0).unwrap();
    let mut sync = Synchronizer::default();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    // A zero intermediate sample cannot override the stored 50% setting.
    sample(&mut feedback, 0, true);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(1))
    );
    sample(&mut feedback, 99, true);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    // Actual switch off cancels reading even while recorded dimmer is 50%.
    sample(&mut feedback, 99, false);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(0))
    );
    let mut leds = vec![];
    for _ in 0..2 {
        loop {
            if let Some(ticket) = admission.dispatch().await.unwrap() {
                assert_eq!(ticket.command.command, Command::PanelLed(false));
                leds.push(ticket.command.device_id.clone());
                admission
                    .finish_write(ticket.command.id, true)
                    .await
                    .unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    leds.sort();
    assert_eq!(leds, [id("ledA"), id("ledB")]);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    let mode = store.logical_variable(1, "mode").unwrap().unwrap();
    store
        .begin_manual_scene_batch_with_variables(
            "outside",
            1,
            &[ManualStart {
                run: "daily".into(),
                definition: "daily".into(),
            }],
            0,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: mode.version,
                value: Some(Value::Integer(2)),
            }],
        )
        .unwrap();
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(2))
    );
    assert!(admission.dispatch().await.unwrap().is_none());
    // Delayed scene output defers feedback-driven shutdown until settled.
    let run = scenes.start_document(&document, "daily", 0).await.unwrap();
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    scenes.cancel(run).await.unwrap();
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn fresh_full_off_keeps_selection_unknown_and_clears_only_reviewed_leds() {
    let root = std::env::temp_dir().join(format!("indicator-bootstrap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let document = homesense_config::rule_document::Document::parse(
        &serde_json::to_vec(&rules()).unwrap(),
        &config,
    )
    .unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &devices(),
            &serde_json::to_vec(&rules()).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("mode".into(), ValueType::Integer)])
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |c| c == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let scenes = SceneScheduler::new(admission.clone(), controller, 8, 0).unwrap();
    let mut sync = Synchronizer::default();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    sample(&mut feedback, 50, true);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        None
    );
    assert!(admission.dispatch().await.unwrap().is_none());
    sample(&mut feedback, 0, false);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        None
    );
    let before = store.logical_variable(1, "mode").unwrap().unwrap();
    assert_eq!(
        before.version, 0,
        "LED output cannot establish a logical baseline"
    );
    let mut leds = vec![];
    for _ in 0..2 {
        loop {
            if let Some(ticket) = admission.dispatch().await.unwrap() {
                assert_eq!(ticket.command.command, Command::PanelLed(false));
                leds.push(ticket.command.device_id.clone());
                admission
                    .finish_write(ticket.command.id, true)
                    .await
                    .unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    leds.sort();
    assert_eq!(leds, [id("ledA"), id("ledB")]);
    assert!(admission.dispatch().await.unwrap().is_none());
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(
        admission.dispatch().await.unwrap().is_none(),
        "LED cleanup is not replayed"
    );
    assert_eq!(store.logical_variable(1, "mode").unwrap().unwrap(), before);
    let sql = rusqlite::Connection::open(root.join("runtime.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queue_pressure_defers_fanout_without_losing_selection_reconciliation() {
    let root = std::env::temp_dir().join(format!("indicator-pressure-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let mut raw: serde_json::Value = serde_json::from_slice(&devices()).unwrap();
    raw["devices"].as_array_mut().unwrap().push(json!({"id":"ledC","name":"Unrelated LED","gateway":"bus","address":"41","channel":2,"enabled":true}));
    let device_bytes = serde_json::to_vec(&raw).unwrap();
    let config =
        homesense_config::Configuration::parse(&device_bytes, ExecutionContext::Simulation)
            .unwrap();
    let rules_bytes = serde_json::to_vec(&rules()).unwrap();
    let document = homesense_config::rule_document::Document::parse(&rules_bytes, &config).unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &device_bytes,
            &rules_bytes,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("mode".into(), ValueType::Integer)])
        .unwrap();
    store
        .begin_manual_scene_batch_with_variables(
            "old",
            1,
            &[ManualStart {
                run: "initial".into(),
                definition: "reading".into(),
            }],
            0,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 0,
                value: Some(Value::Integer(1)),
            }],
        )
        .unwrap();
    record(&mut store, "setting", 50);
    let controller = uuid::Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            command_capacity: 2,
            ..Options::default()
        },
        move |c| c == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let scenes = SceneScheduler::new(admission.clone(), controller, 8, 0).unwrap();
    let mut sync = Synchronizer::default();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    sample(&mut feedback, 99, false);
    admission
        .submit_panel_led(controller, id("ledC"), true)
        .await
        .unwrap();
    let before = store.logical_variable(1, "mode").unwrap().unwrap();
    for _ in 0..2 {
        sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
            .await
            .unwrap();
        assert_eq!(store.logical_variable(1, "mode").unwrap().unwrap(), before);
        assert_eq!(
            store.command_status(admission.boot_id(), 1).unwrap(),
            Some(Status::Accepted)
        );
        assert_eq!(store.command_status(admission.boot_id(), 2).unwrap(), None);
    }
    let ticket = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, id("ledC"));
    admission
        .finish_write(ticket.command.id, true)
        .await
        .unwrap();
    // Same off sample after capacity is available must be reconsidered, not
    // suppressed as a previously accepted or uncertain indicator command.
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(0))
    );
    assert_eq!(
        store.command_status(admission.boot_id(), 2).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        store.command_status(admission.boot_id(), 3).unwrap(),
        Some(Status::Accepted)
    );
    let mut outputs = vec![];
    for _ in 0..2 {
        loop {
            if let Some(ticket) = admission.dispatch().await.unwrap() {
                assert_eq!(ticket.command.command, Command::PanelLed(false));
                outputs.push(ticket.command.device_id.clone());
                admission
                    .finish_write(ticket.command.id, true)
                    .await
                    .unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    outputs.sort();
    assert_eq!(outputs, [id("ledA"), id("ledB")]);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    assert_eq!(store.command_status(admission.boot_id(), 4).unwrap(), None);
    assert_eq!(
        admission
            .submit_panel_led(controller, id("ledC"), false)
            .await
            .unwrap(),
        vec![4]
    );
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn expiring_feedback_precheck_does_not_exit_or_admit_output() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let root = std::env::temp_dir().join(format!("indicator-lease-busy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let rule_bytes = serde_json::to_vec(&rules()).unwrap();
    let document = homesense_config::rule_document::Document::parse(&rule_bytes, &config).unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &devices(),
            &rule_bytes,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("mode".into(), ValueType::Integer)])
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            request_ttl: Duration::from_secs(5),
            ..Options::default()
        },
        move |c| {
            // Recorded settings and variable snapshot succeed; the feedback
            // precheck then waits beyond its lease, simulating a busy owner.
            if counted.fetch_add(1, Ordering::SeqCst) == 2 {
                std::thread::sleep(Duration::from_millis(1200));
            }
            c == controller
        },
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    calls.store(0, Ordering::SeqCst);
    let scenes = SceneScheduler::new(admission.clone(), controller, 8, 0).unwrap();
    let mut sync = Synchronizer::default();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    sample_at(&mut feedback, 0, false, 0);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 1000)
        .await
        .unwrap();
    assert!(calls.load(Ordering::SeqCst) >= 3);
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        None
    );
    assert_eq!(store.command_status(admission.boot_id(), 1).unwrap(), None);
    sample_at(&mut feedback, 0, false, 1000);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        None
    );
    assert_eq!(
        store.command_status(admission.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        store.command_status(admission.boot_id(), 2).unwrap(),
        Some(Status::Accepted)
    );
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cas_rejection_replans_led_cleanup_without_clearing_another_scene() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let root = std::env::temp_dir().join(format!("indicator-cas-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    let rule_bytes = serde_json::to_vec(&rules()).unwrap();
    let document = homesense_config::rule_document::Document::parse(&rule_bytes, &config).unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &devices(),
            &rule_bytes,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("mode".into(), ValueType::Integer)])
        .unwrap();
    store
        .begin_manual_scene_batch_with_variables(
            "outside",
            1,
            &[ManualStart {
                run: "initial".into(),
                definition: "daily".into(),
            }],
            0,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 0,
                value: Some(Value::Integer(2)),
            }],
        )
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let race_path = path.clone();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |c| {
            // Concurrent durable selection update after planning, before its CAS.
            if counted.fetch_add(1, Ordering::SeqCst) == 2 {
                let mut other = Store::open(&race_path).unwrap();
                other
                    .begin_manual_scene_batch_with_variables(
                        "outside",
                        1,
                        &[ManualStart {
                            run: "concurrent".into(),
                            definition: "daily".into(),
                        }],
                        0,
                        &[Update {
                            revision: 1,
                            name: "mode".into(),
                            expected_version: 1,
                            value: Some(Value::Integer(2)),
                        }],
                    )
                    .unwrap();
            }
            c == controller
        },
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    calls.store(0, Ordering::SeqCst);
    let scenes = SceneScheduler::new(admission.clone(), controller, 8, 0).unwrap();
    let mut sync = Synchronizer::default();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    sample(&mut feedback, 50, false);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    assert!(calls.load(Ordering::SeqCst) >= 3);
    let raced = store.logical_variable(1, "mode").unwrap().unwrap();
    assert_eq!(raced.version, 2);
    assert_eq!(raced.value, Some(Value::Integer(2)));
    assert_eq!(store.command_status(admission.boot_id(), 1).unwrap(), None);
    sync.tick(&admission, controller, &document, &scenes, &feedback, 60000)
        .await
        .unwrap();
    let reconciled = store.logical_variable(1, "mode").unwrap().unwrap();
    assert_eq!(reconciled.value, Some(Value::Integer(2)));
    // Condition-only reconciliation preserves the newer selection version.
    assert_eq!(reconciled, raced);
    assert_eq!(
        store.command_status(admission.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        store.command_status(admission.boot_id(), 2).unwrap(),
        Some(Status::Accepted)
    );
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
