use homesense_core::ExecutionContext;
use homesense_storage::{Store, scene_run::{ManualStart, Status}};

#[test]
fn manual_batch_is_atomic_revision_bound_and_never_replayed() {
    let root = std::env::temp_dir().join(format!("manual-scene-batch-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("state.sqlite");
    let mut store = Store::open(&path).unwrap();
    store.publish_configuration(None, include_bytes!("../../../config/simulation.json"), ExecutionContext::Simulation, &[]).unwrap();
    let starts = vec![ManualStart { run: "one".into(), definition: "reading".into() },
        ManualStart { run: "two".into(), definition: "daily".into() }];
    assert!(store.begin_manual_scene_batch("boot", 1, &[], 10).is_err());
    assert!(store.begin_manual_scene_batch("boot", 1, &vec![starts[0].clone();65], 10).is_err());
    assert!(store.begin_manual_scene_batch("boot", 2, &starts, 10).is_err());
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_second_manual BEFORE INSERT ON scene_run WHEN NEW.run_id='two' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(store.begin_manual_scene_batch("boot", 1, &starts, 10).is_err());
    assert!(store.scene_run("boot", "one").unwrap().is_none());
    assert!(store.scene_run("boot", "two").unwrap().is_none());
    sql.execute_batch("DROP TRIGGER fail_second_manual;").unwrap();
    let mut bad = starts.clone(); bad[1].definition.clear();
    assert!(store.begin_manual_scene_batch("boot", 1, &bad, 10).is_err());
    assert!(store.scene_run("boot", "one").unwrap().is_none());
    assert!(store.begin_manual_scene_batch("boot", 1, &[starts[0].clone(),starts[0].clone()], 10).is_err());
    assert!(store.scene_run("boot", "one").unwrap().is_none());
    store.begin_manual_scene_batch("boot", 1, &starts, 10).unwrap();
    assert!(store.begin_manual_scene_batch("boot", 1, &starts, 11).is_err());
    for start in &starts {
        let record = store.scene_run("boot", &start.run).unwrap().unwrap();
        assert_eq!(record.status, Status::Pending);
        assert_eq!(record.started_ms, 10);
        assert_eq!(record.definition.as_ref(), Some(&start.definition));
        assert!(store.scene_cause("boot", &start.run).unwrap().is_none());
    }
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.recover_scene_runs("next-boot").unwrap(), 2);
    assert_eq!(store.recover_scene_runs("next-boot").unwrap(), 0);
    for start in &starts { assert_eq!(store.scene_run("boot", &start.run).unwrap().unwrap().status, Status::Interrupted); }
    assert_eq!(sql.query_row("SELECT COUNT(*) FROM command_journal", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    drop(sql); drop(store); std::fs::remove_dir_all(root).unwrap();
}
