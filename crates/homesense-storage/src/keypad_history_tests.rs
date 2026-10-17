//! Per-endpoint completeness, provenance, index maintenance and query cost.
use super::*;
use rusqlite::Connection;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn setup() -> (Database, Store) {
    let path = Database(std::env::temp_dir().join(format!(
        "keypad-history-{}-{}.sqlite",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )));
    let store = Store::open(&path.0).unwrap();
    store
        .connection
        .execute_batch(
            "INSERT INTO configuration_revision(revision,payload) VALUES(1,x'7b7d'),(2,x'7b7d')",
        )
        .unwrap();
    (path, store)
}
fn row(db: &Connection, command: i64, device: &str, intent: &[u8], status: &str) {
    db.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES('boot',?1,1,?2,'native',?3,1,1,1000000,?1,?4)",params![command,device,intent,status]).unwrap();
}
fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
#[test]
fn all168_endpoint_colors_survive_other_key_history_and_keep_boot_revision_scope() {
    let (_db, store) = setup();
    let ids = (0..168)
        .map(|n| id(&format!("key:{n}")))
        .collect::<Vec<_>>();
    for (n, id) in ids.iter().enumerate() {
        row(
            &store.connection,
            n as i64 + 1,
            id.as_str(),
            &[1, 6, (n % 2) as u8, 0],
            "written",
        );
    }
    for n in 169..269 {
        row(&store.connection, n, "key:0", &[1, 6, 1, 0], "written");
    }
    assert_eq!(
        store
            .recent_device_commands("boot", 1, &ids[..128], 64)
            .unwrap()
            .len(),
        64
    );
    let mut state = vec![];
    for chunk in ids.chunks(128) {
        state.extend(store.keypad_state_records("boot", 1, chunk).unwrap());
    }
    assert_eq!(state.len(), 168);
    for (n, id) in ids.iter().enumerate() {
        let value = state.iter().find(|v| v.device == id.as_str()).unwrap();
        assert_eq!(value.status, "written");
        assert_eq!(
            value.intent,
            [1, 6, if n == 0 { 1 } else { (n % 2) as u8 }, 0]
        );
    }
    assert!(
        store
            .keypad_state_records("other-boot", 1, &ids[..128])
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .keypad_state_records("boot", 2, &ids[..128])
            .unwrap()
            .is_empty()
    );
    let partial = store
        .keypad_state_records("boot", 1, &[id("key:2")])
        .unwrap();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].device, "key:2");
}
#[test]
fn request_failure_read_receipts_and_setting_families_cannot_replace_successful_writes() {
    let (_db, store) = setup();
    for (command, intent, status) in [
        (1, [1, 9, 0xc6, 2], "written"),
        (2, [1, 9, 0xc5, 0], "written"),
        (3, [1, 6, 1, 0], "written"),
        (4, [1, 6, 0, 0], "accepted"),
        (5, [1, 9, 0xc8, 2], "written"),
        (6, [1, 9, 0xc7, 0], "written"),
        (7, [1, 9, 0xc2, 1], "written"),
        (8, [1, 9, 0xc1, 0], "written"),
        (9, [1, 9, 0xc6, 3], "uncertain"),
    ] {
        row(&store.connection, command, "key", &intent, status);
    }
    let values = store.keypad_state_records("boot", 1, &[id("key")]).unwrap();
    let commands = values.iter().map(|v| v.command).collect::<BTreeSet<_>>();
    assert_eq!(commands, [1, 3, 4, 5, 6, 7, 8, 9].into());
    assert_eq!(values.len(), 8);
    for command in [1, 3, 5, 7] {
        assert_eq!(
            values.iter().find(|v| v.command == command).unwrap().status,
            "written"
        );
    }
    assert!(
        !values.iter().any(|v| v.command == 2),
        "superseded read receipts are not last_written settings"
    );
    row(&store.connection, 10, "key", &[1, 10, 0, 0], "written");
    let values = store.keypad_state_records("boot", 1, &[id("key")]).unwrap();
    assert!(values.iter().any(|v| v.command == 10));
    assert!(!values.iter().any(|v| v.command == 3 || v.command == 4));
}
#[test]
fn indexed_request_and_success_lookups_do_not_scan_failed_hot_key_history() {
    let (_db, mut store) = setup();
    row(&store.connection, 1, "key", &[1, 6, 1, 0], "written");
    let mut inserted = 0;
    let mut samples = vec![];
    for size in [1000, 10000, 100000] {
        let tx = store.connection.transaction().unwrap();
        {
            let mut q=tx.prepare("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES('boot',?1,1,'key','native',x'01060000',1,1,1000000,?1,'failed')").unwrap();
            for n in inserted + 2..=size + 1 {
                q.execute([n]).unwrap();
            }
        }
        tx.commit().unwrap();
        inserted = size;
        let state = store.keypad_state_records("boot", 1, &[id("key")]).unwrap();
        assert!(
            state
                .iter()
                .any(|v| v.command == 1 && v.status == "written")
        );
        assert!(
            state
                .iter()
                .any(|v| v.command == size as u64 + 1 && v.status == "failed")
        );
        for written in [false, true] {
            let sql = query_sql(written);
            let mut query = store.connection.prepare(&sql).unwrap();
            let actual = query
                .query_row(params!["boot", 1, "key", 0], record)
                .unwrap();
            assert_eq!(actual.command, if written { 1 } else { size as u64 + 1 });
            let steps = query.get_status(rusqlite::StatementStatus::VmStep);
            samples.push((size, written, steps));
            assert!(steps <= 128, "bounded point lookup: {samples:?}");
            let plan = store
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params!["boot", 1, "key", 0], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|v| v.contains(if written {
                    "command_keypad_written_state"
                } else {
                    "command_keypad_request_state"
                })),
                "{plan:?}"
            );
        }
    }
    eprintln!("keypad point-lookup steps 1k/10k/100k={samples:?}");
}
#[test]
fn index_only_upgrade_is_idempotent_and_preserves_rows_and_schema27() {
    let (db, store) = setup();
    row(&store.connection, 1, "key", &[1, 6, 1, 0], "written");
    let before = store
        .connection
        .query_row(
            "SELECT hex(intent)||status||updated_ms FROM command_journal",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    store
        .connection
        .execute_batch(
            "DROP INDEX command_keypad_request_state;DROP INDEX command_keypad_written_state;",
        )
        .unwrap();
    drop(store);
    for _ in 0..2 {
        let store = Store::open(&db.0).unwrap();
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            27
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT hex(intent)||status||updated_ms FROM command_journal",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            before
        );
        assert_eq!(store.connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('command_keypad_request_state','command_keypad_written_state')",[],|r|r.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(
            store.keypad_state_records("boot", 1, &[id("key")]).unwrap()[0].command,
            1
        );
    }
}
#[test]
fn scope_capacity_duplicates_and_invalid_revisions_are_rejected() {
    let (_db, store) = setup();
    assert!(
        store
            .keypad_state_records("boot", 1, &[])
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.keypad_state_records("boot", 1, &[id("key"), id("key")]),
        Err(Error::InvalidAudit)
    ));
    assert!(
        store
            .keypad_state_records(
                "boot",
                1,
                &(0..129).map(|n| id(&format!("key{n}"))).collect::<Vec<_>>()
            )
            .is_err()
    );
    assert!(store.keypad_state_records("boot", 0, &[id("key")]).is_err());
}
