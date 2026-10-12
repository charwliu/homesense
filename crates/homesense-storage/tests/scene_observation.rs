use homesense_core::{
    DeviceId, ExecutionContext,
    queue::Epochs,
    rule::{Value, ValueType},
    scene_observer::{Basis, Inference, Scope},
};
use homesense_storage::{Error, Store, variables::Update};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "homesense-observation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("runtime.sqlite")
    }
    fn store(&self) -> Store {
        let mut store = Store::open(self.path()).unwrap();
        store.publish(None, 1, b"configuration", &[]).unwrap();
        store
            .define_logical_variable(1, "selection", ValueType::Integer)
            .unwrap();
        store
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn scope() -> Scope {
    Scope {
        boot_id: 7,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        stream_generation: 1,
    }
}
fn inference(version: u64, sequence: u64, value: Option<i64>, basis: Basis) -> Inference {
    Inference {
        group: "reviewed-mode".into(),
        expected_version: version,
        value,
        basis,
        scope: scope(),
        sequence,
        device: DeviceId::new("reviewed:key").unwrap(),
        observed_ms: 100,
    }
}
fn recorded(store: &mut Store, version: u64, value: Option<i64>) {
    store
        .accept_queue_batch_with_variables(
            "local",
            &[],
            &[],
            100,
            None,
            &[Update {
                revision: 1,
                name: "selection".into(),
                expected_version: version,
                value: value.map(Value::Integer),
            }],
        )
        .unwrap();
}
#[test]
fn inferred_value_and_unknown_keep_evidence_but_are_not_rule_inputs() {
    let db = Database::new();
    let mut store = db.store();
    let unknown = inference(0, 1, None, Basis::UnknownInitial);
    store
        .record_scene_inference(1, "selection", &unknown)
        .unwrap();
    let snapshot = store
        .scene_selection(1, "selection", Some(scope()))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.stored.value, None);
    assert_eq!(snapshot.evidence, Some(unknown));
    recorded(&mut store, 1, Some(0));
    let selected = inference(2, 3, Some(2), Basis::SelectMember);
    store
        .record_scene_inference(1, "selection", &selected)
        .unwrap();
    let snapshot = store
        .scene_selection(1, "selection", Some(scope()))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.stored.value, Some(Value::Integer(2)));
    assert_eq!(snapshot.evidence, Some(selected));
    let ordinary = store.logical_variable(1, "selection").unwrap().unwrap();
    assert_eq!(ordinary.value, None);
    assert_eq!(ordinary.version, 3);
    let off = inference(3, 4, Some(0), Basis::ToggleOff);
    store.record_scene_inference(1, "selection", &off).unwrap();
    assert_eq!(
        store
            .scene_selection(1, "selection", Some(scope()))
            .unwrap()
            .unwrap()
            .stored
            .value,
        Some(Value::Integer(0))
    );
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
fn scope_changes_and_reopen_never_promote_an_inference_to_recorded() {
    let db = Database::new();
    let mut store = db.store();
    recorded(&mut store, 0, Some(0));
    let selected = inference(1, 2, Some(2), Basis::SelectMember);
    store
        .record_scene_inference(1, "selection", &selected)
        .unwrap();
    drop(store);
    let store = Store::open(db.path()).unwrap();
    assert_eq!(
        store
            .scene_selection(1, "selection", Some(scope()))
            .unwrap()
            .unwrap()
            .stored
            .value,
        Some(Value::Integer(2))
    );
    let mut different = vec![None];
    let mut boot = scope();
    boot.boot_id += 1;
    different.push(Some(boot));
    let mut transport = scope();
    transport.epochs.transport += 1;
    different.push(Some(transport));
    let mut control = scope();
    control.epochs.control += 1;
    different.push(Some(control));
    let mut stream = scope();
    stream.stream_generation += 1;
    different.push(Some(stream));
    for scope in different {
        let state = store
            .scene_selection(1, "selection", scope)
            .unwrap()
            .unwrap();
        assert_eq!(state.stored.value, None);
        assert_eq!(state.stored.version, 2);
        assert_eq!(state.evidence.as_ref(), Some(&selected));
    }
}
#[test]
fn local_cas_override_detaches_old_evidence_and_stale_key_cannot_overwrite_it() {
    let db = Database::new();
    let mut store = db.store();
    recorded(&mut store, 0, Some(0));
    store
        .record_scene_inference(
            1,
            "selection",
            &inference(1, 1, Some(2), Basis::SelectMember),
        )
        .unwrap();
    let stale = inference(2, 2, Some(0), Basis::ToggleOff);
    recorded(&mut store, 2, Some(3));
    assert!(matches!(
        store.record_scene_inference(1, "selection", &stale),
        Err(Error::Conflict)
    ));
    let state = store
        .scene_selection(1, "selection", Some(scope()))
        .unwrap()
        .unwrap();
    assert_eq!(state.stored.value, Some(Value::Integer(3)));
    assert!(state.evidence.is_none());
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    assert!(matches!(
        store.record_scene_inference(
            1,
            "selection",
            &inference(3, 1, Some(2), Basis::SelectMember)
        ),
        Err(Error::Conflict)
    ));
}
#[test]
fn ledger_failure_rolls_back_value_and_version_and_never_creates_commands() {
    let db = Database::new();
    let mut store = db.store();
    recorded(&mut store, 0, Some(0));
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_observation BEFORE INSERT ON scene_observation BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let change = inference(1, 1, Some(2), Basis::SelectMember);
    assert!(
        store
            .record_scene_inference(1, "selection", &change)
            .is_err()
    );
    let state = store
        .scene_selection(1, "selection", Some(scope()))
        .unwrap()
        .unwrap();
    assert_eq!(state.stored.version, 1);
    assert_eq!(state.stored.value, Some(Value::Integer(0)));
    assert!(state.evidence.is_none());
    sql.execute_batch("DROP TRIGGER reject_observation;")
        .unwrap();
    store
        .record_scene_inference(1, "selection", &change)
        .unwrap();
    assert!(
        sql.execute("UPDATE scene_observation SET value=0", [])
            .is_err()
    );
    assert!(sql.execute("DELETE FROM scene_observation", []).is_err());
    for table in ["command_journal", "scene_run", "rule_admission"] {
        let count: i64 = sql
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
#[test]
fn invalid_basis_and_old_active_revision_are_rejected_without_mutation() {
    let db = Database::new();
    let mut store = db.store();
    for change in [
        inference(0, 1, Some(2), Basis::SelectMember),
        inference(0, 1, Some(0), Basis::ToggleOff),
        inference(0, 1, Some(0), Basis::UnknownInitial),
    ] {
        assert!(matches!(
            store.record_scene_inference(1, "selection", &change),
            Err(Error::InvalidAudit)
        ));
    }
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .version,
        0
    );
    store.publish(Some(1), 2, b"next", &[]).unwrap();
    assert!(matches!(
        store.record_scene_inference(
            1,
            "selection",
            &inference(0, 1, None, Basis::UnknownInitial)
        ),
        Err(Error::Conflict)
    ));
}
fn devices(revision: u64) -> Vec<u8> {
    String::from_utf8(include_bytes!("../../../config/simulation.json").to_vec())
        .unwrap()
        .replace("\"revision\": 1", &format!("\"revision\": {revision}"))
        .into_bytes()
}
fn automation(revision: u64) -> Vec<u8> {
    format!(r#"{{"schema_version":1,"configuration_revision":{revision},"variables":[{{"id":"selection","lifetime":"persistent","kind":"integer"}}],"rules":[]}}"#).into_bytes()
}
#[test]
fn publication_cannot_copy_an_inferred_number_without_its_source() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store
        .publish_automation(
            None,
            &devices(1),
            &automation(1),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    recorded(&mut store, 0, Some(0));
    store
        .record_scene_inference(
            1,
            "selection",
            &inference(1, 1, Some(2), Basis::SelectMember),
        )
        .unwrap();
    store
        .publish_automation(
            Some(1),
            &devices(2),
            &automation(2),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let state = store
        .scene_selection(2, "selection", None)
        .unwrap()
        .unwrap();
    assert_eq!(state.stored.value, None);
    assert_eq!(state.stored.version, 0);
    assert!(state.evidence.is_none());
    // Historical evidence remains visible for the old variable version.
    assert!(
        store
            .scene_selection(1, "selection", None)
            .unwrap()
            .unwrap()
            .evidence
            .is_some()
    );
}
#[test]
fn schema21_migration_and_online_backup_preserve_recorded_values_and_inferred_origin() {
    let db = Database::new();
    let mut store = db.store();
    recorded(&mut store, 0, Some(0));
    drop(store);
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute_batch("DROP TABLE scene_observation; PRAGMA user_version=21;")
        .unwrap();
    drop(sql);
    let mut store = Store::open(db.path()).unwrap();
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(0))
    );
    store
        .record_scene_inference(
            1,
            "selection",
            &inference(1, 1, Some(2), Basis::SelectMember),
        )
        .unwrap();
    let backup = db.0.join("snapshot.sqlite");
    homesense_storage::backup::create(&db.path(), &backup, Duration::from_secs(2)).unwrap();
    let restored = Store::open(&backup).unwrap();
    assert_eq!(
        restored
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        None
    );
    assert_eq!(
        restored
            .scene_selection(1, "selection", Some(scope()))
            .unwrap()
            .unwrap()
            .stored
            .value,
        Some(Value::Integer(2))
    );
    assert_eq!(
        restored
            .scene_selection(1, "selection", None)
            .unwrap()
            .unwrap()
            .stored
            .value,
        None
    );
    assert!(
        restored
            .scene_selection(1, "selection", None)
            .unwrap()
            .unwrap()
            .evidence
            .is_some()
    );
    assert_eq!(
        rusqlite::Connection::open(backup)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        22
    );
}
