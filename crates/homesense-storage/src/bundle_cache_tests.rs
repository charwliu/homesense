use super::*;
use homesense_core::ExecutionContext::Simulation;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn store() -> Store {
    let mut store = Store::open(":memory:").unwrap();
    let devices = include_bytes!("../../../config/simulation.json");
    let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    store
        .publish_automation(None, devices, rules, Simulation, &[])
        .unwrap();
    store
}

#[test]
fn warm_bundle_does_not_reread_payload_columns() {
    let store = store();
    let held = store.active_bundle_shared(Simulation).unwrap().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    store
        .connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Read {
                    table_name: "configuration_revision" | "automation_revision",
                    column_name: "payload"
                }
            ) {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            Authorization::Allow
        }))
        .unwrap();
    for _ in 0..20 {
        assert!(Arc::ptr_eq(
            &held,
            &store.active_bundle_shared(Simulation).unwrap().unwrap()
        ));
    }
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "a warm immutable revision must not fetch megabytes of payload per read"
    );
}

#[test]
fn local_sql_edits_at_same_revision_invalidate_bundle_cache() {
    for table in ["configuration_revision", "automation_revision"] {
        let store = store();
        store.active_bundle_shared(Simulation).unwrap().unwrap();
        let trigger = if table == "configuration_revision" {
            "immutable_configuration_update"
        } else {
            "immutable_automation_update"
        };
        store
            .connection
            .execute_batch(&format!(
                "DROP TRIGGER {trigger}; UPDATE {table} SET payload=x'7b7d' WHERE revision=1"
            ))
            .unwrap();
        assert!(store.active_bundle_shared(Simulation).is_err());
    }
}

#[test]
fn local_schema_edits_cannot_hide_a_missing_automation_table() {
    let store = store();
    store.active_bundle_shared(Simulation).unwrap().unwrap();
    let changes = store.connection.total_changes();
    store
        .connection
        .execute_batch("ALTER TABLE automation_revision RENAME TO removed_automation")
        .unwrap();
    assert_eq!(
        store.connection.total_changes(),
        changes,
        "DDL needs its own fence"
    );
    assert!(store.active_bundle_shared(Simulation).is_err());
}
