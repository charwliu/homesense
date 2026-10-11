use std::process::Command;
#[test]
fn isolated_automation_cli_runs_scene_restarts_and_preserves_live_files() {
    let root = std::env::temp_dir().join(format!("automation-service-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("identity")).unwrap();
    std::fs::write(root.join("runtime.sqlite"), b"live runtime sentinel").unwrap();
    std::fs::write(root.join("identity/sentinel"), b"live pairing sentinel").unwrap();
    let devices = root.join("devices.json");
    let rules = root.join("rules.json");
    std::fs::write(&devices, include_bytes!("../../../config/simulation.json")).unwrap();
    std::fs::write(&rules,br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"isolated simulation test","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#).unwrap();
    let mut automation: hap::serde_json::Value = hap::serde_json::from_slice(&std::fs::read(&rules).unwrap()).unwrap();
    automation["variables"] = hap::serde_json::json!([{"id":"completed","kind":"integer","lifetime":"persistent"}]);
    automation["rules"] = hap::serde_json::json!([{"id":"after-demo","enabled":true,"review_evidence":"simulation","source":"fixture","trigger":{"kind":"scene_result","scene":"demo"},"actions":[{"source":"result","action":{"kind":"set","variable":"completed","value":{"kind":"integer","value":7}}}]}]);
    std::fs::write(&rules,hap::serde_json::to_vec(&automation).unwrap()).unwrap();
    let run = |seconds: &str, scene: &str| {
        Command::new(env!("CARGO_BIN_EXE_homesensed"))
            .arg("serve-automation-simulation")
            .arg(&devices)
            .arg(&rules)
            .arg(&root)
            .arg(seconds)
            .arg(scene)
            .output()
            .unwrap()
    };
    assert!(!run("0", "demo").status.success());
    assert!(!run("1", "missing").status.success());
    assert!(!root.join("automation-simulation.sqlite").exists());
    for _ in 0..2 {
        let output = run("1", "demo");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let count: i64 = sql
        .query_row(
            "SELECT count(*) FROM scene_run WHERE status='commands_written'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let count: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status='written'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    // Restart preserves the earlier write as uncertain, never replaying it.
    let old: i64 = sql.query_row("SELECT count(*) FROM command_journal WHERE status='uncertain'",[],|r|r.get(0)).unwrap();
    assert_eq!(old,1);
    let rules_run:i64=sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='after-demo'",[],|r|r.get(0)).unwrap();
    assert_eq!(rules_run,2,"one result rule per run, never every tick");
    let value:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='completed'",[],|r|r.get(0)).unwrap();
    assert_eq!(value,7);
    std::fs::write(
        &rules,
        br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_homesensed"))
        .arg("serve-automation-simulation")
        .arg(&devices)
        .arg(&rules)
        .arg(&root)
        .arg("1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(root.join("runtime.sqlite")).unwrap(),
        b"live runtime sentinel"
    );
    assert_eq!(
        std::fs::read(root.join("identity/sentinel")).unwrap(),
        b"live pairing sentinel"
    );
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn simulation_publication_cancels_old_delays_and_runs_new_mapping() {
    use homesensed::service::{SimulationPublication,serve_automation_simulation_with_publication};
    use std::time::Duration;
    let root=std::env::temp_dir().join(format!("automation-reload-{}",uuid::Uuid::new_v4()));
    let devices=include_bytes!("../../../config/simulation.json");
    let mut next:hap::serde_json::Value=hap::serde_json::from_slice(devices).unwrap();
    next["revision"]=2.into();
    next["devices"][0]["channel"]=2.into();
    let next=hap::serde_json::to_vec(&next).unwrap();
    let rules=|revision, delayed| {
        let mut steps=vec![hap::serde_json::json!({"source":"immediate","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}})];
        if delayed {steps.push(hap::serde_json::json!({"source":"must cancel","offset_ms":10000,"device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}));}
        hap::serde_json::to_vec(&hap::serde_json::json!({"schema_version":1,"configuration_revision":revision,"variables":[],"rules":[],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":steps}]})).unwrap()
    };
    let old_rules=rules(1,true);
    let next_rules=rules(2,false);
    serve_automation_simulation_with_publication(devices,&old_rules,&root,Duration::from_millis(400),Some("demo"),Some(SimulationPublication{
        devices:&next,rules:&next_rules,after:Duration::from_millis(200),scene:Some("demo"),
    })).await.unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT revision FROM active_configuration",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    for revision in [1,2] {
        assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE revision=?1",[revision],|r|r.get::<_,i64>(0)).unwrap(),1,"old delayed output must not survive publication");
    }
    assert_eq!(sql.query_row("SELECT transport_epoch FROM command_journal WHERE revision=2",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE revision=2 AND status IN ('written','observed')",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert!(!root.join("runtime.sqlite").exists());
    assert!(!root.join("identity").exists());
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
