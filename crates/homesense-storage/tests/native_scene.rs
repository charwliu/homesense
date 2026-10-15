use homesense_core::{
    Command, DeviceId, Percent,
    queue::{Epochs, Origin, QueuedCommand},
    rule::{Value, ValueType},
};
use homesense_storage::{
    Error, Store,
    journal::Status as Journal,
    native_scene::{Request, Status},
    variables::Update,
};
struct Database(std::path::PathBuf);
static DATABASE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Database {
    fn new() -> (Self, Store) {
        let path = std::env::temp_dir().join(format!(
            "native-store-{}-{}-{}",
            std::process::id(),
            DATABASE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let mut store = Store::open(path.join("runtime.sqlite")).unwrap();
        store.publish(None, 1, b"synthetic", &[]).unwrap();
        store
            .define_logical_variable(1, "selection", ValueType::Integer)
            .unwrap();
        (Self(path), store)
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn request() -> Request {
    Request {
        controller: "synthetic-controller".into(),
        key: "once".into(),
        scene: "first".into(),
        group: "group".into(),
        variable: "selection".into(),
        variable_version: 0,
        boot: "boot1".into(),
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        deadline_ms: 1000,
    }
}
#[test]
fn reservation_phase_cas_and_restart_never_reopen_a_consumed_key() {
    let (db, mut store) = Database::new();
    let request = request();
    store.reserve_native_scene(&request, 10).unwrap();
    assert!(store.reserve_native_scene(&request, 11).is_err());
    let mut other = request.clone();
    other.key = "other".into();
    assert!(matches!(
        store.reserve_native_scene(&other, 11),
        Err(Error::Conflict)
    ));
    store.consume_native_phase(&request, 0, 12).unwrap();
    assert!(matches!(
        store.consume_native_phase(&request, 0, 13),
        Err(Error::Conflict)
    ));
    store
        .native_phase_accepted(&request, 1, &[1], 2, 13)
        .unwrap();
    drop(store);
    let mut store = Store::open(db.0.join("runtime.sqlite")).unwrap();
    assert_eq!(
        store
            .native_scene_request(&request.controller, &request.key)
            .unwrap()
            .unwrap()
            .phase,
        1
    );
    store
        .expire_native_scenes("boot2", request.epochs, 0)
        .unwrap();
    let row = store
        .native_scene_request(&request.controller, &request.key)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, Status::Unknown);
    assert_eq!(row.updated_ms, 13);
    assert_eq!(row.initial_commands, vec![1]);
    assert!(store.reserve_native_scene(&request, 14).is_err());
    assert!(store.consume_native_phase(&request, 1, 14).is_err());
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        None
    );
}
#[test]
fn manual_variable_version_blocks_every_later_phase_and_completion() {
    let (_db, mut store) = Database::new();
    let request = request();
    store.reserve_native_scene(&request, 10).unwrap();
    store.consume_native_phase(&request, 0, 11).unwrap();
    store
        .native_phase_accepted(&request, 1, &[1], 2, 12)
        .unwrap();
    store
        .accept_queue_batch_with_variables(
            "boot1",
            &[],
            &[],
            13,
            None,
            &[Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 0,
                value: Some(Value::Integer(2)),
            }],
        )
        .unwrap();
    assert!(matches!(
        store.consume_native_phase(&request, 1, 14),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.finish_native_scene(&request, 1, Some(1), &[], 15),
        Err(Error::Conflict)
    ));
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(2))
    );
    assert_eq!(
        store
            .native_scene_request(&request.controller, &request.key)
            .unwrap()
            .unwrap()
            .phase,
        1
    );
    store
        .fail_native_scene(&request.controller, &request.key, 16)
        .unwrap();
    assert_eq!(
        store
            .native_scene_request(&request.controller, &request.key)
            .unwrap()
            .unwrap()
            .status,
        Status::Unknown
    );
}
#[test]
fn actual_native_dimmer_setting_and_observed_receipt_commit_atomically() {
    let (db, mut store) = Database::new();
    let request = request();
    let device = DeviceId::new("light").unwrap();
    store.reserve_native_scene(&request, 10).unwrap();
    store.consume_native_phase(&request, 0, 11).unwrap();
    let command = QueuedCommand {
        id: 1,
        device_id: device.clone(),
        command: Command::Toggle,
        origin: Origin::Hap,
        epochs: request.epochs,
        deadline_ms: 1000,
        run_id: None,
    };
    store.accept_commands("boot1", &[command], 12).unwrap();
    store
        .transition_command("boot1", 1, Journal::Accepted, Journal::Writing, 13)
        .unwrap();
    store
        .transition_command("boot1", 1, Journal::Writing, Journal::Written, 14)
        .unwrap();
    store
        .native_phase_accepted(&request, 1, &[1], 2, 14)
        .unwrap();
    let sql = rusqlite::Connection::open(db.0.join("runtime.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_native BEFORE UPDATE ON native_scene_request WHEN NEW.status='observed' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let levels = [(device.clone(), 1, Percent::new(30).unwrap())];
    assert!(
        store
            .finish_native_scene(&request, 1, Some(1), &levels, 20)
            .is_err()
    );
    assert!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&device))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .native_scene_request(&request.controller, &request.key)
            .unwrap()
            .unwrap()
            .status,
        Status::Pending
    );
    sql.execute_batch("DROP TRIGGER fail_native;").unwrap();
    store
        .finish_native_scene(&request, 1, Some(1), &levels, 21)
        .unwrap();
    assert_eq!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&device))
            .unwrap()[&device],
        Percent::new(30).unwrap()
    );
    assert_eq!(
        store
            .native_scene_request(&request.controller, &request.key)
            .unwrap()
            .unwrap()
            .selected,
        Some(1)
    );
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        None
    );
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Journal::Written)
    );
}

#[test]
fn schema23_upgrade_preserves_historical_selection_and_starts_with_no_relative_requests() {
    let (db, mut store) = Database::new();
    store
        .accept_queue_batch_with_variables(
            "old-boot",
            &[],
            &[],
            5,
            None,
            &[Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 0,
                value: Some(Value::Integer(2)),
            }],
        )
        .unwrap();
    drop(store);
    let sql = rusqlite::Connection::open(db.0.join("runtime.sqlite")).unwrap();
    sql.execute_batch("DROP TABLE native_scene_request; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=23;")
        .unwrap();
    drop(sql);
    let store = Store::open(db.0.join("runtime.sqlite")).unwrap();
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(2))
    );
    assert!(
        store
            .native_scene_request("synthetic-controller", "once")
            .unwrap()
            .is_none()
    );
    let sql = rusqlite::Connection::open(db.0.join("runtime.sqlite")).unwrap();
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        homesense_storage::SCHEMA_VERSION
    );
}
