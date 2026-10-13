use homesense_core::{
    Command, DeviceId, ExecutionContext,
    queue::{CancelReason, Cancellation, Epochs, Origin, QueuedCommand},
    rule::{Value, ValueType},
};
use homesense_storage::{Store, journal::Status, rule_audit::Admission, variables::Update};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "homesense-rule-sources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> PathBuf {
        self.0.join("runtime.sqlite")
    }
    fn store(&self) -> Store {
        let mut store = Store::open(self.path()).unwrap();
        store.publish_configuration(None, br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"load","name":"Synthetic load","module_id":"relay","channel":1,"enabled":true}]}"#, ExecutionContext::Simulation, &[]).unwrap();
        store
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command(id: u64) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: DeviceId::new("load").unwrap(),
        command: Command::Switch(true),
        origin: Origin::Rule,
        epochs: Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        },
        deadline_ms: 1000,
        run_id: None,
    }
}
fn audit(event: u64) -> Admission {
    Admission {
        revision: 1,
        correlation: 7,
        event,
        parent: 0,
        rule: "synthetic-rule".into(),
    }
}
fn update(version: u64) -> Update {
    Update {
        revision: 1,
        name: "mode".into(),
        expected_version: version,
        value: Some(Value::Integer(2)),
    }
}
#[test]
fn source_labels_variables_and_cancellations_rollback_together_on_sql_or_cas_failure() {
    let db = Database::new();
    let mut store = db.store();
    store
        .define_logical_variable(1, "mode", ValueType::Integer)
        .unwrap();
    store
        .accept_queue_batch_with_rule_audit(
            "boot",
            &[command(1)],
            &[],
            1,
            None,
            &[],
            Some(&audit(1)),
        )
        .unwrap();
    let cancel = Cancellation {
        command: command(1),
        reason: CancelReason::Superseded,
    };
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    // Failure is after journal and admission insertion, on the second source row.
    sql.execute_batch("CREATE TRIGGER reject_source BEFORE INSERT ON rule_command WHEN NEW.command_id=3 BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let commands = [command(2), command(3)];
    let sources = vec!["node:one".into(), "node:two".into()];
    let accept = |s: &mut Store, version| {
        s.accept_queue_batch_with_read_checks_and_sources(
            "boot",
            &commands,
            std::slice::from_ref(&cancel),
            10,
            None,
            &[update(version)],
            Some(&audit(2)),
            None,
            &[],
            &[],
            Some(&sources),
        )
    };
    assert!(accept(&mut store, 0).is_err());
    sql.execute_batch("DROP TRIGGER reject_source;").unwrap();
    assert!(accept(&mut store, 1).is_err()); // variable CAS fails after both source rows
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().version,
        0
    );
    assert_eq!(
        store.command_status("boot", 1).unwrap(),
        Some(Status::Accepted)
    );
    for id in [2, 3] {
        assert_eq!(store.command_status("boot", id).unwrap(), None);
    }
    assert_eq!(store.rule_chain("boot", 7, 0, 64).unwrap().records.len(), 1);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM rule_command", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    accept(&mut store, 0).unwrap();
    drop(store);
    let mut reopened = Store::open(db.path()).unwrap();
    assert_eq!(
        reopened.command_status("boot", 1).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        reopened.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(2))
    );
    let page = reopened.rule_chain("boot", 7, 0, 64).unwrap();
    assert!(page.records[0].commands[0].source.is_none());
    assert_eq!(
        page.records[1]
            .commands
            .iter()
            .map(|c| c.source.as_deref())
            .collect::<Vec<_>>(),
        [Some("node:one"), Some("node:two")]
    );
    assert_eq!(
        page.records[1]
            .commands
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
}
#[test]
fn invalid_or_misaligned_labels_never_accept_any_receipt() {
    let db = Database::new();
    let mut store = db.store();
    for labels in [
        vec![],
        vec![String::new()],
        vec!["x".repeat(8193)],
        vec!["界".repeat(2731)],
        vec!["one".into(), "two".into()],
    ] {
        assert!(
            store
                .accept_queue_batch_with_read_checks_and_sources(
                    "boot",
                    &[command(1)],
                    &[],
                    1,
                    None,
                    &[],
                    Some(&audit(1)),
                    None,
                    &[],
                    &[],
                    Some(&labels)
                )
                .is_err()
        );
        assert!(store.command_status("boot", 1).unwrap().is_none());
        assert!(
            store
                .rule_chain("boot", 7, 0, 64)
                .unwrap()
                .records
                .is_empty()
        );
    }
    let sources = vec!["x".repeat(8192)];
    assert!(
        store
            .accept_queue_batch_with_read_checks_and_sources(
                "boot",
                &[command(1)],
                &[],
                1,
                None,
                &[],
                None,
                None,
                &[],
                &[],
                Some(&sources)
            )
            .is_err()
    );
    store
        .accept_queue_batch_with_read_checks_and_sources(
            "boot",
            &[command(1)],
            &[],
            1,
            None,
            &[],
            Some(&audit(1)),
            None,
            &[],
            &[],
            Some(&sources),
        )
        .unwrap();
    assert_eq!(
        store.rule_chain("boot", 7, 0, 64).unwrap().records[0].commands[0]
            .source
            .as_deref(),
        Some(sources[0].as_str())
    );
}
#[test]
fn schema_24_backup_and_migration_keep_historical_sources_unknown() {
    let db = Database::new();
    let mut store = db.store();
    store
        .accept_queue_batch_with_rule_audit(
            "boot",
            &[command(1)],
            &[],
            1,
            None,
            &[],
            Some(&audit(1)),
        )
        .unwrap();
    drop(store);
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute_batch("ALTER TABLE rule_command DROP COLUMN source; PRAGMA user_version=24;")
        .unwrap();
    drop(sql);
    let old = db.0.join("before.sqlite");
    homesense_storage::backup::create(&db.path(), &old, Duration::from_secs(5)).unwrap();
    let before = std::fs::read(&old).unwrap();
    assert_eq!(
        homesense_storage::backup::inspect(&old, ExecutionContext::Simulation)
            .unwrap()
            .schema,
        24
    );
    assert_eq!(std::fs::read(&old).unwrap(), before); // inspection never migrates the backup
    let mut upgraded = Store::open(db.path()).unwrap();
    let row = &upgraded.rule_chain("boot", 7, 0, 64).unwrap().records[0].commands[0];
    assert_eq!(row.id, 1);
    assert_eq!(row.device, "load");
    assert!(row.source.is_none());
    let labels = vec!["new-node".into()];
    upgraded
        .accept_queue_batch_with_read_checks_and_sources(
            "boot",
            &[command(2)],
            &[],
            2,
            None,
            &[],
            Some(&audit(2)),
            None,
            &[],
            &[],
            Some(&labels),
        )
        .unwrap();
    drop(upgraded);
    let mut reopened = Store::open(db.path()).unwrap();
    let page = reopened.rule_chain("boot", 7, 0, 64).unwrap();
    assert!(page.records[0].commands[0].source.is_none());
    assert_eq!(
        page.records[1].commands[0].source.as_deref(),
        Some("new-node")
    );
    let new = db.0.join("after.sqlite");
    homesense_storage::backup::create(&db.path(), &new, Duration::from_secs(5)).unwrap();
    assert_eq!(
        homesense_storage::backup::inspect(&new, ExecutionContext::Simulation)
            .unwrap()
            .schema,
        25
    );
    assert_eq!(std::fs::read(old).unwrap(), before);
}
