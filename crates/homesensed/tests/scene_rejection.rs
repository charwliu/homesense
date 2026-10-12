use homesense_core::{
    Command, DeviceId, ExecutionContext, LevelTransition, Percent,
    scene::{Status, Step},
};
use homesensed::{
    admission::{Admission, Options},
    scene_runtime::SceneRuntime,
};
use uuid::Uuid;

const CONFIG: &[u8] = br#"{
  "schema_version":1,"revision":1,
  "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
    {"address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
    {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],
  "devices":[
    {"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"supply"},
    {"id":"supply","name":"Supply","gateway":"bus","address":"03","channel":4,"enabled":true}]}
"#;
fn step(device: &str, command: Command) -> Step {
    Step {
        device: DeviceId::new(device).unwrap(),
        command,
        delay_ms: 0,
        source: "synthetic".into(),
    }
}
fn level(value: u8) -> Command {
    Command::SetLevel {
        level: Percent::new(value).unwrap(),
        transition: LevelTransition::Immediate,
    }
}

#[tokio::test]
async fn rejected_scene_stops_remaining_steps_but_keeps_other_control_available() {
    let root = std::env::temp_dir().join(format!("scene-rejection-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_configuration(None, CONFIG, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let a = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    // Raw timelines are also public APIs: rejection must be isolated even when
    // document parsing was bypassed. A prior accepted step may already be written.
    for prior_output in [false, true] {
        let mut steps = vec![];
        if prior_output {
            steps.push(step("light", level(50)));
        }
        steps.extend([
            step("supply", Command::Switch(false)),
            step("light", level(70)),
        ]);
        let mut scene = SceneRuntime::new(a.clone(), controller, steps, 0, 1000)
            .await
            .unwrap();
        if prior_output {
            assert_eq!(scene.poll(0).await.unwrap().unwrap().len(), 2);
            while let Some(ticket) = a.dispatch().await.unwrap() {
                a.finish_write(ticket.command.id, true).await.unwrap();
            }
        }
        assert!(scene.poll(1).await.unwrap().is_none());
        assert_eq!(
            scene.status(),
            if prior_output {
                Status::Partial
            } else {
                Status::Failed
            }
        );
        assert!(scene.poll(2).await.unwrap().is_none());
        assert!(
            a.dispatch().await.unwrap().is_none(),
            "rejected and later steps cannot write"
        );
        let history = a
            .scene_history(
                controller,
                a.task_generation().await.unwrap(),
                homesense_storage::scene_run::Query::Run {
                    boot: a.boot_id().into(),
                    run: scene.audit_id().into(),
                    after: 0,
                    limit: 128,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            history.record.status,
            if prior_output {
                homesense_storage::scene_run::Status::Partial
            } else {
                homesense_storage::scene_run::Status::Failed
            }
        );
    }
    // A valid logical all-off operation still works in the same actor/boot.
    let mut valid = SceneRuntime::new(
        a.clone(),
        controller,
        vec![step("light", level(0))],
        0,
        1000,
    )
    .await
    .unwrap();
    assert_eq!(valid.poll(0).await.unwrap().unwrap().len(), 1);
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id.as_str(), "supply");
    assert_eq!(ticket.command.command, Command::Switch(false));
    a.finish_write(ticket.command.id, true).await.unwrap();
    valid.reconcile().await.unwrap();
    assert_eq!(valid.status(), Status::CommandsWritten);
    a.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn document_preflight_rejects_physical_supply_and_unnormalized_combined_commands() {
    let config =
        homesense_config::Configuration::parse(CONFIG, ExecutionContext::Simulation).unwrap();
    for (device, command) in [
        ("supply", serde_json::json!({"kind":"switch","on":false})),
        (
            "light",
            serde_json::json!({"kind":"level","value":30,"ramp":true}),
        ),
        ("light", serde_json::json!({"kind":"pause_ramp"})),
    ] {
        let document = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],
            "timed_scenes":[{"id":"all-off","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[
                {"source":"first","offset_ms":0,"device":"light","command":{"kind":"level","value":50,"ramp":false}},
                {"source":"invalid","offset_ms":0,"device":device,"command":command}]}]});
        assert!(
            homesense_config::rule_document::Document::parse(
                &serde_json::to_vec(&document).unwrap(),
                &config
            )
            .is_err()
        );
        let rule = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],
            "rules":[{"id":"nested","enabled":true,"review_evidence":"synthetic","source":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[
                {"source":"branch","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"integer","value":1},"right":{"kind":"integer","value":1}},"then_actions":[],"else_actions":[
                    {"source":"unreachable","action":{"kind":"command","device":device,"command":command}}]}}]}]});
        assert!(
            homesense_config::rule_document::Document::parse(
                &serde_json::to_vec(&rule).unwrap(),
                &config
            )
            .is_err()
        );
    }
    let mut document = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],
        "timed_scenes":[{"id":"all-off","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[
            {"source":"logical","offset_ms":0,"device":"light","command":{"kind":"level","value":0,"ramp":false}}]}]});
    assert!(
        homesense_config::rule_document::Document::parse(
            &serde_json::to_vec(&document).unwrap(),
            &config
        )
        .is_ok()
    );
    for value in [0, 30, 100] {
        document["timed_scenes"][0]["steps"][0]["command"]["value"] = value.into();
        assert!(
            homesensed::configuration_publish::validate_automation(
                &config,
                &serde_json::to_vec(&document).unwrap(),
                ExecutionContext::Simulation
            )
            .is_ok()
        );
    }
}

#[tokio::test]
async fn failure_to_persist_rejected_scene_remains_fatal() {
    let root = std::env::temp_dir().join(format!("scene-rejection-audit-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_configuration(None, CONFIG, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let a = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    let mut scene = SceneRuntime::new(
        a.clone(),
        controller,
        vec![step("supply", Command::Switch(false))],
        0,
        1000,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON scene_run BEGIN SELECT RAISE(ABORT, 'synthetic storage failure'); END;").unwrap();
    assert!(
        scene
            .poll(0)
            .await
            .unwrap_err()
            .contains("ServiceCommunicationFailure")
    );
    assert!(
        a.task_generation().await.is_err(),
        "storage failure must still gate the actor"
    );
    drop(sql);
    a.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
