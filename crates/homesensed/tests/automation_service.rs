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
    automation["variables"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"actual-level","kind":"integer","lifetime":"feedback_cache","feedback":{"kind":"level","device":"simulation:light"}}));
    automation["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"light-observed","enabled":true,"review_evidence":"simulation","source":"fixture","trigger":{"kind":"device_changed","device":"simulation:light"},"actions":[{"source":"observed","action":{"kind":"set","variable":"observed","value":{"kind":"variable","id":"actual-level"}}}]}));
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
    let mut first_backup = None;
    for _ in 0..2 {
        let output = run("1", "demo");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    let backup_dir = root.join("daily-backups/automation-simulation.sqlite");
    let files: Vec<_> = std::fs::read_dir(&backup_dir).unwrap().map(|v|v.unwrap().path()).collect();
    assert_eq!(files.len(), 1);
    homesense_storage::backup::inspect(&files[0], homesense_core::ExecutionContext::Simulation).unwrap();
    let bytes = std::fs::read(&files[0]).unwrap();
    if let Some(previous) = &first_backup { assert_eq!(previous, &bytes, "restart must reuse the daily snapshot"); }
    first_backup = Some(bytes);
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
    let observed:Option<i64>=sql.query_row("SELECT value FROM logical_variable WHERE name='observed'",[],|r|r.get(0)).unwrap();
    // Depending on scheduling, feedback can share a millisecond with the write.
    // It may be rejected, but must never bring down the service.
    assert!(matches!(observed, None | Some(0) | Some(30)));
    let rejected:i64=sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error LIKE '%ResourceBusy%'",[],|r|r.get(0)).unwrap();

    let cached:i64=sql.query_row("SELECT count(*) FROM logical_variable WHERE name='actual-level'",[],|r|r.get(0)).unwrap();
    assert_eq!(cached,0,"feedback alias must not be persisted as a logical variable");
    let feedback_runs:i64=sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='light-observed'",[],|r|r.get(0)).unwrap();
    assert!(feedback_runs+rejected>=2,"feedback roots must be accepted or durably rejected on each boot");



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

#[test]
fn startup_calls_delayed_scene_and_resumes_original_cause_in_service() {
    let root=std::env::temp_dir().join(format!("deferred-service-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let devices=root.join("devices.json");let rules=root.join("rules.json");
    std::fs::write(&devices,include_bytes!("../../../config/simulation.json")).unwrap();
    std::fs::write(&rules,br#"{"schema_version":1,"configuration_revision":1,"variables":[{"id":"done","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"begin","enabled":true,"review_evidence":"simulation","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"call","action":{"kind":"scene","id":"later","activate":true}}]},{"id":"after","enabled":true,"review_evidence":"simulation","source":"test","trigger":{"kind":"scene_result","scene":"later"},"actions":[{"source":"done","action":{"kind":"set","variable":"done","value":{"kind":"integer","value":1}}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"delayed","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#).unwrap();
    for _ in 0..2 {
        let output=Command::new(env!("CARGO_BIN_EXE_homesensed")).arg("serve-automation-simulation").arg(&devices).arg(&rules).arg(&root).arg("1").output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    }
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let counts:(i64,i64)=sql.query_row("SELECT count(*),count(DISTINCT correlation) FROM rule_admission",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counts,(4,2));
    let children:i64=sql.query_row("SELECT count(*) FROM rule_admission child JOIN rule_admission parent ON child.boot_id=parent.boot_id AND child.correlation=parent.correlation AND child.parent_id=parent.event_id WHERE child.rule_id='after' AND parent.rule_id='begin'",[],|r|r.get(0)).unwrap();
    assert_eq!(children,2,"delayed results inherit original root across scheduler ticks");
    let scenes:i64=sql.query_row("SELECT count(*) FROM scene_cause",[],|r|r.get(0)).unwrap();assert_eq!(scenes,2);
    let done:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='done'",[],|r|r.get(0)).unwrap();assert_eq!(done,1);
    let complete:i64=sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error IS NULL",[],|r|r.get(0)).unwrap();assert_eq!(complete,2);
    assert!(!root.join("runtime.sqlite").exists());drop(sql);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn isolated_startup_cycle_does_not_stop_manual_scene_or_successor() {
    let root=std::env::temp_dir().join(format!("isolated-rule-service-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let devices=root.join("devices.json");let rules=root.join("rules.json");
    std::fs::write(&devices,include_bytes!("../../../config/simulation.json")).unwrap();
    let action=|variable:&str,value:i32|hap::serde_json::json!({"source":"test","action":{"kind":"set","variable":variable,"value":{"kind":"integer","value":value}}});
    let rule=|id:&str,trigger:hap::serde_json::Value,actions:Vec<hap::serde_json::Value>|hap::serde_json::json!({"id":id,"enabled":true,"review_evidence":"simulation","source":"test","trigger":trigger,"actions":actions});
    let document=hap::serde_json::json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"mode","kind":"integer","lifetime":"persistent"},{"id":"done","kind":"integer","lifetime":"persistent"}],
        "rules":[rule("start",hap::serde_json::json!({"kind":"startup_synchronized"}),vec![action("mode",1)]),rule("cycle",hap::serde_json::json!({"kind":"variable_changed","variable":"mode"}),vec![action("mode",2)]),rule("after",hap::serde_json::json!({"kind":"scene_result","scene":"manual"}),vec![action("done",1)])],
        "timed_scenes":[{"id":"manual","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"step","offset_ms":100,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]
    });
    std::fs::write(&rules,hap::serde_json::to_vec(&document).unwrap()).unwrap();
    let output=Command::new(env!("CARGO_BIN_EXE_homesensed")).arg("serve-automation-simulation").arg(&devices).arg(&rules).arg(&root).arg("1").arg("manual").output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let failed:i64=sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error IS NOT NULL",[],|r|r.get(0)).unwrap();assert_eq!(failed,1);
    let succeeded:i64=sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error IS NULL",[],|r|r.get(0)).unwrap();assert_eq!(succeeded,1);
    let done:i64=sql.query_row("SELECT value FROM logical_variable WHERE name='done'",[],|r|r.get(0)).unwrap();assert_eq!(done,1);
    let written:i64=sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get(0)).unwrap();assert_eq!(written,1);
    drop(sql);std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn local_write_feedback_inherits_original_chain_and_stops_cycle() {
    use std::time::Duration;
    for (delayed,startup_fault) in [(false,false),(true,false),(false,true)] {
        let root=std::env::temp_dir().join(format!("inferred-feedback-service-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let devices=root.join("devices.json");let rules=root.join("rules.json");
        std::fs::write(&devices,include_bytes!("../../../config/simulation.json")).unwrap();
        let command=|value|hap::serde_json::json!({"kind":"level","value":value,"ramp":false});
        let output=|value|hap::serde_json::json!({"source":"synthetic:output","action":{"kind":"command","device":"simulation:light","command":command(value)}});
        let begin=if delayed {hap::serde_json::json!({"source":"synthetic:scene","action":{"kind":"scene","id":"later","activate":true}})} else {output(30)};
        let mut document=hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],
            "rules":[
                {"id":"begin","enabled":true,"review_evidence":"simulation","source":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[begin]},
                {"id":"feedback","enabled":true,"review_evidence":"simulation","source":"synthetic","trigger":{"kind":"device_changed","device":"simulation:light"},"actions":[output(60)]}
            ],
            "timed_scenes":[{"id":"later","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"synthetic:step","offset_ms":20,"device":"simulation:light","command":command(30)}]}]
        });
        if startup_fault {
            document["variables"]=hap::serde_json::json!([{"id":"mode","kind":"integer","lifetime":"persistent"}]);
            let set=|value|hap::serde_json::json!({"source":"synthetic:set","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":value}}});
            document["rules"][0]["actions"].as_array_mut().unwrap().push(set(1));
            document["rules"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"cycle","enabled":true,"review_evidence":"simulation","source":"synthetic","trigger":{"kind":"variable_changed","variable":"mode"},"actions":[set(2)]}));
        }
        std::fs::write(&rules,hap::serde_json::to_vec(&document).unwrap()).unwrap();
        homesensed::service::serve_automation_simulation(&std::fs::read(&devices).unwrap(),&std::fs::read(&rules).unwrap(),&root,Duration::from_millis(400),None).await.unwrap();
        let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
        assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),if startup_fault{1}else{2},"delayed={delayed} fault={startup_fault}");
        assert_eq!(sql.query_row("SELECT count(*),count(DISTINCT correlation) FROM rule_admission",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap(),(2,1));
        assert_eq!(sql.query_row("SELECT count(*) FROM rule_chain_outcome WHERE error LIKE '%Cycle%'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(sql.query_row("SELECT count(*) FROM feedback_inference WHERE attribution='inferred'",[],|r|r.get::<_,i64>(0)).unwrap(),if startup_fault{0}else{2});
        let values=sql.prepare("SELECT observation_value FROM feedback_inference ORDER BY sequence").unwrap().query_map([],|r|r.get::<_,i64>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        assert_eq!(values,if startup_fault{vec![]}else{vec![30,60]},"observed values remain physical feedback, not overwritten targets");
        if !startup_fault {
        let parent=sql.query_row("SELECT parent_id FROM rule_admission WHERE rule_id='feedback'",[],|r|r.get::<_,i64>(0)).unwrap();
        assert_eq!(parent,1);
        } else {
            assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission WHERE rule_id='feedback'",[],|r|r.get::<_,i64>(0)).unwrap(),0,"terminal parent must suppress its later matching feedback");
        }
        drop(sql);std::fs::remove_dir_all(root).unwrap();
    }
}
