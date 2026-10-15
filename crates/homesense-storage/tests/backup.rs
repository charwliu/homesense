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

#[test]
fn inspection_validates_standalone_backup_without_migration_or_activation() {
    use homesense_core::ExecutionContext;
    let root=std::env::temp_dir().join(format!("homesense-inspect-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();let source=root.join("active.sqlite");let target=root.join("backup.sqlite");
    let mut store=homesense_storage::Store::open(&source).unwrap();
    store.publish_automation(None,include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,ExecutionContext::Simulation,&[]).unwrap();
    homesense_storage::backup::create(&source,&target,Duration::from_secs(2)).unwrap();
    let before=std::fs::read(&target).unwrap();
    let report=homesense_storage::backup::inspect(&target,ExecutionContext::Simulation).unwrap();
    assert_eq!(report.active_revision,Some(1));assert_eq!(report.schema,homesense_storage::SCHEMA_VERSION);assert_eq!(report.configuration_versions,1);assert_eq!(report.automation_versions,1);assert_eq!(report.bytes,before.len() as u64);
    assert!(homesense_storage::backup::inspect(&target,ExecutionContext::Live).is_err());
    assert_eq!(std::fs::read(&target).unwrap(),before);
    assert!(homesense_storage::backup::inspect(&source,ExecutionContext::Simulation).is_err());
    std::fs::write(root.join("backup.sqlite-wal"),b"").unwrap();assert!(homesense_storage::backup::inspect(&target,ExecutionContext::Simulation).is_err());std::fs::remove_file(root.join("backup.sqlite-wal")).unwrap();
    #[cfg(unix)] {let link=root.join("link.sqlite");std::os::unix::fs::symlink(&target,&link).unwrap();assert!(homesense_storage::backup::inspect(&link,ExecutionContext::Simulation).is_err());}
    let db=rusqlite::Connection::open(&target).unwrap();db.execute_batch("PRAGMA user_version=999").unwrap();drop(db);
    assert!(homesense_storage::backup::inspect(&target,ExecutionContext::Simulation).is_err());
    let missing=root.join("missing.sqlite");assert!(homesense_storage::backup::inspect(&missing,ExecutionContext::Simulation).is_err());assert!(!missing.exists());
    drop(store);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn catalog_is_bounded_sorted_and_never_follows_symlinks() {
    let root=std::env::temp_dir().join(format!("homesense-catalog-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("z.sqlite"),b"not a database").unwrap();std::fs::write(root.join("a.sqlite"),b"123").unwrap();std::fs::write(root.join("ignored.txt"),b"x").unwrap();
    let list=homesense_storage::backup::catalog(&root).unwrap();assert_eq!(list.iter().map(|v|v.name.as_str()).collect::<Vec<_>>(),vec!["a.sqlite","z.sqlite"]);assert_eq!(list[0].bytes,3);assert!(list.iter().all(|v|v.regular_file));
    #[cfg(unix)] {
        std::os::unix::fs::symlink(root.join("missing"),root.join("link.sqlite")).unwrap();
        assert!(!homesense_storage::backup::catalog(&root).unwrap().iter().find(|v|v.name=="link.sqlite").unwrap().regular_file);
        let alias=root.join("alias");std::os::unix::fs::symlink(&root,&alias).unwrap();assert!(homesense_storage::backup::catalog(&alias).is_err());
    }
    for n in 0..1025 {std::fs::write(root.join(format!("ignored-{n}.txt")),b"").unwrap();}
    assert!(homesense_storage::backup::catalog(&root).is_err());std::fs::remove_dir_all(root).unwrap();
}
