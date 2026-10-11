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
