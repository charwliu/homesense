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
