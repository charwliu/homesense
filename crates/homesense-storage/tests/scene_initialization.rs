use homesense_core::{
    queue::Epochs,
    rule::{Value, ValueType},
};
use homesense_storage::{Error, Store, scene_initialization::Origin, variables::Update};
fn source(value: i64) -> Origin {
    Origin {
        group: "synthetic-group".into(),
        boot_id: 7,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        observed_ms: 10,
        initialized_ms: 12,
        value,
    }
}
fn database() -> (std::path::PathBuf, Store) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "initialization-store-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let mut store = Store::open(path.join("runtime.sqlite")).unwrap();
    store
        .publish(None, 1, b"synthetic configuration", &[])
        .unwrap();
    store
        .define_logical_variable(1, "selection", ValueType::Integer)
        .unwrap();
    (path, store)
}
#[test]
fn initialization_origin_is_atomic_once_and_not_carried_by_later_writes() {
    let (path, mut store) = database();
    let origin = source(1);
    store
        .initialize_scene_selection("selection", 0, &origin)
        .unwrap();
    let current = store
        .scene_selection(1, "selection", None)
        .unwrap()
        .unwrap();
    assert_eq!(current.stored.value, Some(Value::Integer(1)));
    assert_eq!(current.initialization, Some(origin));
    assert!(matches!(
        store.initialize_scene_selection("selection", 0, &source(0)),
        Err(Error::Conflict)
    ));
    store
        .accept_queue_batch_with_variables(
            "synthetic-boot",
            &[],
            &[],
            20,
            None,
            &[Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 1,
                value: Some(Value::Integer(0)),
            }],
        )
        .unwrap();
    assert!(
        store
            .scene_selection(1, "selection", None)
            .unwrap()
            .unwrap()
            .initialization
            .is_none()
    );
    drop(store);
    let store = Store::open(path.join("runtime.sqlite")).unwrap();
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(0))
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn origin_insert_failure_rolls_back_selection_and_version() {
    let (path, mut store) = database();
    let sql = rusqlite::Connection::open(path.join("runtime.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_init BEFORE INSERT ON scene_initialization BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(
        store
            .initialize_scene_selection("selection", 0, &source(0))
            .is_err()
    );
    let current = store.logical_variable(1, "selection").unwrap().unwrap();
    assert!(current.value.is_none());
    assert_eq!(current.version, 0);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_initialization", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn version22_migration_preserves_raw_values_and_adds_empty_origin_table() {
    let (path, mut store) = database();
    store
        .accept_queue_batch_with_variables(
            "synthetic",
            &[],
            &[],
            0,
            None,
            &[Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 0,
                value: Some(Value::Integer(3)),
            }],
        )
        .unwrap();
    drop(store);
    let sql = rusqlite::Connection::open(path.join("runtime.sqlite")).unwrap();
    sql.execute_batch("DROP TRIGGER immutable_scene_initialization_update;DROP TRIGGER immutable_scene_initialization_delete;DROP TABLE scene_initialization;ALTER TABLE rule_command DROP COLUMN source; PRAGMA user_version=22;").unwrap();
    drop(sql);
    let store = Store::open(path.join("runtime.sqlite")).unwrap();
    let state = store
        .scene_selection(1, "selection", None)
        .unwrap()
        .unwrap();
    assert_eq!(state.stored.value, Some(Value::Integer(3)));
    assert!(state.initialization.is_none());
    let sql = rusqlite::Connection::open(path.join("runtime.sqlite")).unwrap();
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        25
    );
    drop(sql);
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
