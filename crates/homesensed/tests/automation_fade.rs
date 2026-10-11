use hap::serde_json::json;
#[tokio::test]
async fn startup_fade_writes_samples_then_resumes_its_causal_rule() {
    let root = std::env::temp_dir().join(format!("automation-fade-{}", uuid::Uuid::new_v4()));
    let document = json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"done","kind":"integer","lifetime":"persistent"}],
        "rules":[
            {"id":"begin","enabled":true,"review_evidence":"synthetic test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"call","action":{"kind":"scene","id":"fade","activate":true}}]},
            {"id":"after","enabled":true,"review_evidence":"synthetic test","source":"test","trigger":{"kind":"scene_result","scene":"fade"},"actions":[{"source":"done","action":{"kind":"set","variable":"done","value":{"kind":"integer","value":1}}}]}
        ],
        "timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"synthetic test","max_lateness_ms":1000,"steps":[{"source":"ramp","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false},"fade":{"duration_ms":300,"interval_ms":100,"lateness_ms":200}}]}]
    });
    homesensed::service::serve_automation_simulation(
        include_bytes!("../../../config/simulation.json"),
        &hap::serde_json::to_vec(&document).unwrap(),
        &root,
        std::time::Duration::from_secs(1),
        None,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let commands: Vec<(Vec<u8>, String)> = sql
        .prepare("SELECT intent,status FROM command_journal ORDER BY command_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(commands.len() >= 2 && commands.len() <= 3, "{commands:?}");
    assert!(
        commands
            .iter()
            .all(|(intent, status)| intent[..2] == [1, 2] && status == "written")
    );
    assert!(commands.windows(2).all(|pair| pair[0].0[2] < pair[1].0[2]));
    assert_eq!(commands.last().unwrap().0[2], 60);
    let samples: i64 = sql
        .query_row("SELECT count(*) FROM scene_sample", [], |r| r.get(0))
        .unwrap();
    assert_eq!(samples, commands.len() as i64);
    let complete: i64 = sql
        .query_row(
            "SELECT count(*) FROM scene_run WHERE status='commands_written'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(complete, 1);
    let done: i64 = sql
        .query_row(
            "SELECT value FROM logical_variable WHERE name='done'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(done, 1);
    let linked: i64 = sql.query_row("SELECT count(*) FROM rule_admission child JOIN rule_admission parent ON child.boot_id=parent.boot_id AND child.correlation=parent.correlation AND child.parent_id=parent.event_id WHERE child.rule_id='after' AND parent.rule_id='begin'",[],|r|r.get(0)).unwrap();
    assert_eq!(linked, 1);
    assert!(!root.join("runtime.sqlite").exists());
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn service_exit_cancels_partial_fade_and_restart_never_replays_samples() {
    let root = std::env::temp_dir().join(format!("automation-fade-stop-{}", uuid::Uuid::new_v4()));
    let devices = include_bytes!("../../../config/simulation.json");
    let document = hap::serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,
        "variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"synthetic test","max_lateness_ms":1000,"steps":[{"source":"ramp","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":100,"ramp":false},"fade":{"duration_ms":1000,"interval_ms":100,"lateness_ms":200}}]}]
    })).unwrap();
    homesensed::service::serve_automation_simulation(
        devices,
        &document,
        &root,
        std::time::Duration::from_millis(350),
        Some("fade"),
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let before: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert!(before > 0 && before < 10);
    let state: String = sql
        .query_row("SELECT status FROM scene_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "partial");
    let unfinished: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unfinished, 0);
    drop(sql);
    homesensed::service::serve_automation_simulation(
        devices,
        &document,
        &root,
        std::time::Duration::from_millis(1200),
        None,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_sample", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let state: String = sql
        .query_row("SELECT status FROM scene_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "partial");
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn publication_stops_old_fade_before_new_mapping_runs() {
    use homesensed::service::{
        SimulationPublication, serve_automation_simulation_with_publication,
    };
    use std::time::Duration;
    let root =
        std::env::temp_dir().join(format!("automation-fade-publish-{}", uuid::Uuid::new_v4()));
    let devices = include_bytes!("../../../config/simulation.json");
    let mut next: hap::serde_json::Value = hap::serde_json::from_slice(devices).unwrap();
    next["revision"] = 2.into();
    next["devices"][0]["channel"] = 2.into();
    let next = hap::serde_json::to_vec(&next).unwrap();
    let document = |revision, duration| {
        hap::serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":revision,
        "variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"synthetic test","max_lateness_ms":1000,"steps":[{"source":"ramp","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":100,"ramp":false},"fade":{"duration_ms":duration,"interval_ms":100,"lateness_ms":200}}]}]
    })).unwrap()
    };
    let old_rules = document(1, 1000);
    let new_rules = document(2, 300);
    serve_automation_simulation_with_publication(
        devices,
        &old_rules,
        &root,
        Duration::from_millis(1000),
        Some("fade"),
        Some(SimulationPublication {
            devices: &next,
            rules: &new_rules,
            after: Duration::from_millis(350),
            scene: Some("fade"),
        }),
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let states: Vec<(i64, String)> = sql
        .prepare("SELECT revision,status FROM scene_run ORDER BY revision")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        states,
        vec![(1, "partial".into()), (2, "commands_written".into())]
    );
    let commands: Vec<(i64, i64, Vec<u8>)> = sql
        .prepare("SELECT revision,transport_epoch,intent FROM command_journal ORDER BY command_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(commands.iter().any(|(revision, _, _)| *revision == 1));
    assert!(
        commands
            .iter()
            .filter(|(revision, _, _)| *revision == 1)
            .all(|(_, epoch, intent)| *epoch == 1 && intent[2] < 100)
    );
    let first_new = commands
        .iter()
        .position(|(revision, _, _)| *revision == 2)
        .unwrap();
    assert!(
        commands[first_new..]
            .iter()
            .all(|(revision, epoch, _)| *revision == 2 && *epoch == 2)
    );
    assert_eq!(commands.last().unwrap().2[2], 100);
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn rejected_fade_capacity_does_not_abort_other_scene_in_service() {
    let root = std::env::temp_dir().join(format!("fade-capacity-{}", uuid::Uuid::new_v4()));
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    let mut second = devices["devices"][0].clone();
    second["id"] = "simulation:second".into();
    second["channel"] = 2.into();
    devices["devices"].as_array_mut().unwrap().push(second);
    let scenes:Vec<_>=[("first","simulation:light"),("second","simulation:second")].into_iter().map(|(id,device)|json!({"id":id,"enabled":true,"review_evidence":"synthetic test","max_lateness_ms":1000,"steps":[{"source":"fade","offset_ms":0,"device":device,"command":{"kind":"level","value":60,"ramp":false},"fade":{"duration_ms":300,"interval_ms":50,"lateness_ms":200}}]})).collect();
    let actions: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|id| json!({"source":id,"action":{"kind":"scene","id":id,"activate":true}}))
        .collect();
    let document = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"start","enabled":true,"review_evidence":"synthetic test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":actions}],"timed_scenes":scenes});
    homesensed::service::serve_automation_simulation(
        &hap::serde_json::to_vec(&devices).unwrap(),
        &hap::serde_json::to_vec(&document).unwrap(),
        &root,
        std::time::Duration::from_secs(1),
        None,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let states: Vec<(String, String)> = sql
        .prepare("SELECT definition,status FROM scene_run ORDER BY definition")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        states,
        vec![
            ("first".into(), "commands_written".into()),
            ("second".into(), "cancelled".into())
        ]
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE device_id='simulation:second'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(sql.query_row("SELECT count(*) FROM command_journal WHERE device_id='simulation:light' AND status='written'",[],|r|r.get::<_,i64>(0)).unwrap()>0);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
