use std::time::Duration;
#[test]
fn consistent_snapshot_includes_wal_but_excludes_uncommitted_changes() {
    let root = std::env::temp_dir().join(format!(
        "homesense-backup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let source = root.join("active.sqlite");
    let target = root.join("snapshot.sqlite");
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE example(value INTEGER); INSERT INTO example VALUES (42); BEGIN; UPDATE example SET value=99;").unwrap();
    assert!(
        std::fs::metadata(source.with_extension("sqlite-wal"))
            .unwrap()
            .len()
            > 0
    );
    homesense_storage::backup::create(&source, &target, Duration::from_secs(2)).unwrap();
    let snapshot = rusqlite::Connection::open(&target).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT value FROM example", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert_eq!(
        snapshot
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    drop(snapshot);
    assert!(homesense_storage::backup::create(&source, &target, Duration::from_secs(2)).is_err());
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let missing = root.join("missing.sqlite");
    let failed = root.join("failed.sqlite");
    assert!(homesense_storage::backup::create(&missing, &failed, Duration::from_secs(2)).is_err());
    assert!(!failed.exists());
    assert!(!missing.exists());
    std::fs::remove_dir_all(root).unwrap();
}
