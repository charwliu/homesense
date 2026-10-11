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
    automation["variables"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"second","kind":"integer","lifetime":"persistent"}));
    automation["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"also-after-demo","enabled":true,"review_evidence":"simulation","source":"fixture","trigger":{"kind":"scene_result","scene":"demo"},"actions":[{"source":"second","action":{"kind":"set","variable":"second","value":{"kind":"integer","value":9}}}]}));
    automation["variables"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"booted","kind":"integer","lifetime":"persistent"}));
    for (id,value) in [("boot-first",1),("boot-second",2)] {
        automation["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"simulation","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"startup","action":{"kind":"set","variable":"booted","value":{"kind":"integer","value":value}}}]}));
    }
    automation["variables"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"observed","kind":"integer","lifetime":"persistent"}));
    automation["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"light-observed","enabled":true,"review_evidence":"simulation","source":"fixture","trigger":{"kind":"device_changed","device":"simulation:light"},"actions":[{"source":"observed","action":{"kind":"set","variable":"observed","value":{"kind":"integer","value":30}}}]}));
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
    let correlations:i64=sql.query_row("SELECT count(DISTINCT correlation) FROM rule_admission WHERE rule_id IN ('after-demo','also-after-demo')",[],|r|r.get(0)).unwrap();
    assert_eq!(correlations,2,"both matching rules share one root per service run");
    let second:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='second'",[],|r|r.get(0)).unwrap();
    assert_eq!(second,9);
    let startup:(i64,i64)=sql.query_row("SELECT count(*),count(DISTINCT correlation) FROM rule_admission WHERE rule_id IN ('boot-first','boot-second')",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(startup,(4,2),"one shared startup root per run, not per tick");
    let booted:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='booted'",[],|r|r.get(0)).unwrap();
    assert_eq!(booted,2);
    let observed:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='observed'",[],|r|r.get(0)).unwrap();
    assert_eq!(observed,30);
    let feedback_runs:i64=sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='light-observed'",[],|r|r.get(0)).unwrap();
    assert!(feedback_runs>=2,"new physical feedback reaches rule router on each boot");



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
    // Durable active bytes govern restart: old inputs cannot silently revert it.
    assert!(homesensed::service::serve_automation_simulation(devices,&old_rules,&root,Duration::from_millis(40),None).await.is_err());
    homesensed::service::serve_automation_simulation(&next,&next_rules,&root,Duration::from_millis(40),None).await.unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),2,"restart must not replay scenes");
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
