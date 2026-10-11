use homesense_core::{
    Command, DeviceId,
    admission::{Access, RuntimePolicy},
    arbiter::{Arbiter, CommitResult},
    queue::{CancelReason, Epochs, Origin, QueuedCommand},
};
use homesense_storage::{CancellationAudit, Error, Store};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
struct Database {
    directory: PathBuf,
}
impl Database {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "homesense-storage-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("state.sqlite")
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn committed_configuration_and_audit_survive_close_and_reopen() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    assert!(store.active().unwrap().is_none());
    store.publish(None, 1, b"config-one", &[]).unwrap();
    store
        .publish(
            Some(1),
            2,
            b"config-two",
            &[CancellationAudit {
                boot_id: "boot1",
                command_id: 1,
                reason: CancelReason::ConfigurationChanged,
            }],
        )
        .unwrap();
    drop(store);
    let store = Store::open(db.path()).unwrap();
    assert_eq!(store.active().unwrap().unwrap().payload, b"config-two");
    assert_eq!(store.revision(1).unwrap().unwrap().payload, b"config-one");
    assert_eq!(store.cancellation_count(2).unwrap(), 1);
}
#[test]
fn audit_failure_rolls_back_revision_and_pointer_together() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store.publish(None, 1, b"old", &[]).unwrap();
    let a = || CancellationAudit {
        boot_id: "boot",
        command_id: 1,
        reason: CancelReason::ConfigurationChanged,
    };
    assert!(store.publish(Some(1), 2, b"new", &[a(), a()]).is_err());
    assert!(store.revision(2).unwrap().is_none());
    assert_eq!(store.cancellation_count(2).unwrap(), 0);
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    drop(store);
    assert_eq!(
        Store::open(db.path())
            .unwrap()
            .active()
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}
#[test]
fn compare_and_swap_rejects_stale_writer_and_nonmonotonic_revision() {
    let db = Database::new();
    let mut first = Store::open(db.path()).unwrap();
    first.publish(None, 1, b"one", &[]).unwrap();
    let mut second = Store::open(db.path()).unwrap();
    first.publish(Some(1), 2, b"two", &[]).unwrap();
    assert!(matches!(
        second.publish(Some(1), 3, b"three", &[]),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        second.publish(Some(2), 2, b"replace", &[]),
        Err(Error::InvalidRevision)
    ));
    assert!(matches!(
        second.publish(Some(2), 3, b"", &[]),
        Err(Error::InvalidPayload)
    ));
    assert_eq!(second.active().unwrap().unwrap().payload, b"two");
}
#[test]
fn foreign_databases_are_not_migrated_and_existing_revisions_are_immutable() {
    let db = Database::new();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated(value TEXT);")
        .unwrap();
    assert!(matches!(
        Store::open(db.path()),
        Err(Error::ForeignDatabase)
    ));
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let db2 = Database::new();
    let mut store = Store::open(db2.path()).unwrap();
    store.publish(None, 1, b"original", &[]).unwrap();
    let connection = rusqlite::Connection::open(db2.path()).unwrap();
    assert!(
        connection
            .execute("UPDATE configuration_revision SET payload='changed'", [])
            .is_err()
    );
    assert!(
        connection
            .execute("DELETE FROM configuration_revision", [])
            .is_err()
    );
    assert_eq!(store.active().unwrap().unwrap().payload, b"original");
}
#[test]
fn real_sqlite_commit_drives_arbiter_revision_and_restart_truth() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store.publish(None, 1, b"first", &[]).unwrap();
    let epochs = Epochs {
        transport: 1,
        configuration: store.active().unwrap().unwrap().revision,
        control: 1,
    };
    let mut arbiter = Arbiter::new(
        RuntimePolicy {
            epochs,
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        8,
        2,
    )
    .unwrap();
    let request = QueuedCommand {
        id: 1,
        device_id: DeviceId::new("study:light").unwrap(),
        command: Command::Switch(true),
        origin: Origin::Hap,
        epochs,
        deadline_ms: 100,
        run_id: None,
    };
    arbiter
        .submit(
            request,
            Access {
                authorized: true,
                device_exists: true,
                device_verified: true,
                existing_direct_manual: true,
            },
            0,
        )
        .unwrap();
    let cancelled = arbiter.begin_publication(1, 50).unwrap();
    arbiter.begin_commit(2).unwrap();
    let audit: Vec<_> = cancelled
        .iter()
        .map(|c| CancellationAudit {
            boot_id: "boot1",
            command_id: c.command.id,
            reason: c.reason,
        })
        .collect();
    store.publish(Some(1), 2, b"second", &audit).unwrap();
    // Simulate losing the commit acknowledgement; resolve from the actual pointer.
    arbiter.record_commit(CommitResult::Unknown).unwrap();
    drop(store);
    let store = Store::open(db.path()).unwrap();
    let active = store.active().unwrap().unwrap();
    arbiter
        .record_commit(CommitResult::Committed(active.revision))
        .unwrap();
    assert!(arbiter.policy().publishing);
    arbiter.synchronized(active.revision).unwrap();
    assert_eq!(arbiter.policy().epochs.configuration, 2);
    assert_eq!(store.cancellation_count(2).unwrap(), 1);
    assert_eq!(arbiter.queued_len(), 0);
}
#[test]
fn database_lock_failure_leaves_active_pointer_and_revision_unchanged() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store.publish(None, 1, b"old", &[]).unwrap();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(store.publish(Some(1), 2, b"new", &[]).is_err());
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    assert!(store.revision(2).unwrap().is_none());
}

#[test]
fn validated_configuration_is_checked_before_publish_and_again_at_startup() {
    use homesense_core::ExecutionContext::{Live, Simulation};
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    let payload = include_bytes!("../../../config/simulation.json");
    assert!(
        store
            .publish_configuration(None, payload, Live, &[])
            .is_err()
    );
    assert!(store.active().unwrap().is_none());
    store
        .publish_configuration(None, payload, Simulation, &[])
        .unwrap();
    drop(store);
    let store = Store::open(db.path()).unwrap();
    assert_eq!(
        store
            .active_configuration(Simulation)
            .unwrap()
            .unwrap()
            .revision(),
        1
    );
    assert!(store.active_configuration(Live).is_err());
    let db2 = Database::new();
    let mut store = Store::open(db2.path()).unwrap();
    // Low-level storage accepts opaque bytes; startup must detect mismatch.
    store.publish(None, 2, payload, &[]).unwrap();
    assert!(matches!(
        store.active_configuration(Simulation),
        Err(Error::InvalidRevision)
    ));
}

fn bundle_devices(revision: u64) -> Vec<u8> {
    String::from_utf8(include_bytes!("../../../config/simulation.json").to_vec())
        .unwrap()
        .replace("\"revision\": 1", &format!("\"revision\": {revision}"))
        .into_bytes()
}
fn bundle_rules(revision: u64, kind: &str) -> Vec<u8> {
    format!(r#"{{"schema_version":1,"configuration_revision":{revision},"variables":[{{"id":"mode","kind":"{kind}","lifetime":"persistent"}},{{"id":"scratch","kind":"integer","lifetime":"temporary"}}],"rules":[]}}"#).into_bytes()
}
#[test]
fn automation_publication_is_atomic_preserves_values_and_blocks_legacy_drop() {
    use homesense_core::ExecutionContext::Simulation;
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store
        .publish_automation(
            None,
            &bundle_devices(1),
            &bundle_rules(1, "integer"),
            Simulation,
            &[],
        )
        .unwrap();
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute(
        "UPDATE logical_variable SET value=7,version=3 WHERE revision=1 AND name='mode'",
        [],
    )
    .unwrap();
    assert!(
        store
            .publish_configuration(Some(1), &bundle_devices(2), Simulation, &[])
            .is_err()
    );
    assert!(
        store
            .publish_automation(
                Some(1),
                &bundle_devices(2),
                &bundle_rules(1, "integer"),
                Simulation,
                &[]
            )
            .is_err()
    );
    assert!(
        store
            .publish_automation(
                Some(1),
                &bundle_devices(2),
                &bundle_rules(2, "bool"),
                Simulation,
                &[]
            )
            .is_err()
    );
    assert!(store.revision(2).unwrap().is_none());
    let count = || {
        sql.query_row("SELECT count(*) FROM automation_revision", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    assert_eq!(count(), 1);
    // Failure after both payload inserts must roll back configuration, rules,
    // declarations and the active pointer together.
    sql.execute_batch("CREATE TRIGGER fail_variable BEFORE INSERT ON logical_variable WHEN NEW.revision=2 BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .publish_automation(
                Some(1),
                &bundle_devices(2),
                &bundle_rules(2, "integer"),
                Simulation,
                &[]
            )
            .is_err()
    );
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    assert!(store.revision(2).unwrap().is_none());
    assert_eq!(count(), 1);
    sql.execute_batch("DROP TRIGGER fail_variable;").unwrap();
    store
        .publish_automation(
            Some(1),
            &bundle_devices(2),
            &bundle_rules(2, "integer"),
            Simulation,
            &[],
        )
        .unwrap();
    assert!(
        store
            .publish_automation(
                Some(1),
                &bundle_devices(3),
                &bundle_rules(3, "integer"),
                Simulation,
                &[]
            )
            .is_err()
    );
    assert!(store.revision(3).unwrap().is_none());
    assert_eq!(
        sql.query_row(
            "SELECT value FROM logical_variable WHERE revision=2 AND name='mode'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        7
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM logical_variable WHERE revision=2",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        sql.execute(
            "UPDATE automation_revision SET payload=x'00' WHERE revision=2",
            []
        )
        .is_err()
    );
    drop(store);
    let store = Store::open(db.path()).unwrap();
    let bundle = store.active_bundle(Simulation).unwrap().unwrap();
    assert_eq!(bundle.configuration.revision(), 2);
    assert_eq!(bundle.automation.unwrap().revision, 2);
}
#[test]
fn legacy_configuration_has_no_automation_and_migrates_explicitly() {
    use homesense_core::ExecutionContext::Simulation;
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store
        .publish_configuration(None, &bundle_devices(1), Simulation, &[])
        .unwrap();
    assert!(
        store
            .active_bundle(Simulation)
            .unwrap()
            .unwrap()
            .automation
            .is_none()
    );
    store
        .publish_automation(
            Some(1),
            &bundle_devices(2),
            &bundle_rules(2, "integer"),
            Simulation,
            &[],
        )
        .unwrap();
    assert!(
        store
            .active_bundle(Simulation)
            .unwrap()
            .unwrap()
            .automation
            .is_some()
    );
}

#[test]
fn corrupted_automation_blocks_legacy_startup_validation() {
    use homesense_core::ExecutionContext::Simulation;
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    store
        .publish_automation(
            None,
            &bundle_devices(1),
            &bundle_rules(1, "integer"),
            Simulation,
            &[],
        )
        .unwrap();
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute_batch(
        "DROP TRIGGER immutable_automation_update; UPDATE automation_revision SET payload=x'00';",
    )
    .unwrap();
    assert!(store.active_bundle(Simulation).is_err());
    assert!(store.active_configuration(Simulation).is_err());
}

#[test]
fn explicit_variable_conversion_commits_atomically_and_rejects_stale_or_lossy_source() {
    use homesense_core::{ExecutionContext::Simulation,rule::Value};
    let db=Database::new();
    let mut store=Store::open(db.path()).unwrap();
    store.publish_automation(None,&bundle_devices(1),&bundle_rules(1,"integer"),Simulation,&[]).unwrap();
    let sql=rusqlite::Connection::open(db.path()).unwrap();
    sql.execute("UPDATE logical_variable SET value=2 WHERE name='mode'",[]).unwrap();
    let rules=|source| format!(r#"{{"schema_version":1,"configuration_revision":2,"variables":[{{"id":"mode","kind":"bool","lifetime":"persistent"}}],"variable_migrations":[{{"variable":"mode","source_revision":{source},"source_type":"integer","conversion":"integer_zero_one_to_bool","review_evidence":"fixture reviewed"}}],"rules":[]}}"#).into_bytes();
    assert!(store.publish_automation(Some(1),&bundle_devices(2),&rules(1),Simulation,&[]).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision,1);
    assert!(store.revision(2).unwrap().is_none());
    sql.execute("UPDATE logical_variable SET value=1 WHERE name='mode'",[]).unwrap();
    let config=homesense_config::Configuration::parse(&bundle_devices(2),Simulation).unwrap();
    let candidate=homesense_config::rule_document::Document::parse(&rules(1),&config).unwrap();
    let source=store.logical_variable(1,"mode").unwrap().unwrap();
    assert_eq!(candidate.published_variable_value(Some(1),"mode",Some((source.kind,source.value.as_ref()))).unwrap(),Some(Value::Bool(true)));
    // Even a valid preflight cannot authorize a now-lossy source in the transaction.
    sql.execute("UPDATE logical_variable SET value=2 WHERE name='mode'",[]).unwrap();
    assert!(store.publish_automation(Some(1),&bundle_devices(2),&rules(1),Simulation,&[]).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision,1);
    assert!(store.automation_payload(2).unwrap().is_none());
    sql.execute("UPDATE logical_variable SET value=1 WHERE name='mode'",[]).unwrap();

    sql.execute_batch("CREATE TRIGGER fail_migration BEFORE INSERT ON logical_variable WHEN NEW.revision=2 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(store.publish_automation(Some(1),&bundle_devices(2),&rules(1),Simulation,&[]).is_err());
    assert!(store.automation_payload(2).unwrap().is_none());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(Value::Integer(1)));
    sql.execute_batch("DROP TRIGGER fail_migration;").unwrap();
    store.publish_automation(Some(1),&bundle_devices(2),&rules(1),Simulation,&[]).unwrap();
    assert_eq!(store.logical_variable(2,"mode").unwrap().unwrap().value,Some(Value::Bool(true)));
    let third=String::from_utf8(rules(1)).unwrap().replace("\"configuration_revision\":2","\"configuration_revision\":3");
    assert!(store.publish_automation(Some(2),&bundle_devices(3),third.as_bytes(),Simulation,&[]).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision,2);
    drop(store);
    let store=Store::open(db.path()).unwrap();
    assert!(store.active_bundle(Simulation).unwrap().is_some());
    assert_eq!(store.logical_variable(2,"mode").unwrap().unwrap().value,Some(Value::Bool(true)));
}

#[test]
fn migration_requires_existing_source_even_for_reset_unknown() {
    use homesense_core::ExecutionContext::Simulation;
    let db=Database::new();
    let mut store=Store::open(db.path()).unwrap();
    let rules=br#"{"schema_version":1,"configuration_revision":2,"variables":[{"id":"missing","kind":"bool","lifetime":"persistent"}],"variable_migrations":[{"variable":"missing","source_revision":1,"source_type":"integer","conversion":"reset_unknown","review_evidence":"fixture"}],"rules":[]}"#;
    assert!(store.publish_automation(None,&bundle_devices(2),rules,Simulation,&[]).is_err());
    assert!(store.active().unwrap().is_none());
    assert!(store.revision(2).unwrap().is_none());
    store.publish_automation(None,&bundle_devices(1),&bundle_rules(1,"integer"),Simulation,&[]).unwrap();
    assert!(store.publish_automation(Some(1),&bundle_devices(2),rules,Simulation,&[]).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision,1);
    assert!(store.automation_payload(2).unwrap().is_none());
}
