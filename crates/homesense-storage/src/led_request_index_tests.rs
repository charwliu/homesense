//! Scope-filter, migration, and VM-cost regressions with synthetic journal rows.
use super::*;
use rusqlite::{Connection, params, types::Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const INDEX: &str = "command_led_request_scope";
const DDL: &str = "CREATE INDEX IF NOT EXISTS command_led_request_scope ON command_journal(boot_id,revision,device_id,transport_epoch,control_epoch,command_id DESC) WHERE intent IN (x'01060000',x'01060100',x'010A0000')";
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "homesense-led-index-{}-{}.sqlite",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn setup() -> (Database, Store) {
    let db = Database::new();
    let store = Store::open(&db.0).unwrap();
    store
        .connection
        .execute(
            "INSERT INTO configuration_revision(revision,payload) VALUES(1,x'7b7d'),(2,x'7b7d')",
            [],
        )
        .unwrap();
    (db, store)
}
fn row(c: &Connection, id: i64, scope: (&str, i64, &str, (i64, i64)), intent: &[u8], status: &str) {
    let (boot, revision, device, epochs) = scope;
    c.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES(?1,?2,?3,?4,'rule',?5,?6,?7,1000,100,?8)",params![boot,id,revision,device,intent,epochs.0,epochs.1,status]).unwrap();
}
fn epochs() -> homesense_core::queue::Epochs {
    homesense_core::queue::Epochs {
        configuration: 1,
        transport: 1,
        control: 1,
    }
}
fn dump(c: &Connection) -> Vec<Vec<Value>> {
    let mut q = c
        .prepare("SELECT * FROM command_journal ORDER BY boot_id,command_id")
        .unwrap();
    let columns = q.column_count();
    q.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn plan(c: &Connection) -> Vec<String> {
    c.prepare(&format!("EXPLAIN QUERY PLAN {LATEST_LED_REQUEST_SQL}"))
        .unwrap()
        .query_map(params!["boot", 1, "led", 1, 1], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn steps(c: &Connection) -> (i64, i32) {
    let mut q = c.prepare(LATEST_LED_REQUEST_SQL).unwrap();
    let id = q
        .query_row(params!["boot", 1, "led", 1, 1], |r| r.get(0))
        .unwrap();
    (id, q.get_status(rusqlite::StatementStatus::VmStep))
}
#[test]
fn led_lookup_vm_cost_does_not_grow_with_unrelated_boot_history() {
    let (_db, mut store) = setup();
    row(
        &store.connection,
        1,
        ("boot", 1, "led", (1, 1)),
        &[1, 6, 1, 0],
        "written",
    );
    let mut inserted = 0;
    let mut samples = vec![];
    for count in [1000, 10000, 100000] {
        let tx = store.connection.transaction().unwrap();
        {
            let mut q=tx.prepare("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES('boot',?1,1,'other-light','rule',x'01021E00',1,1,1000,100,'written')").unwrap();
            for id in inserted + 2..=count + 1 {
                q.execute([id]).unwrap();
            }
        }
        tx.commit().unwrap();
        inserted = count;
        let actual = store
            .latest_led_request("boot", 1, &DeviceId::new("led").unwrap(), epochs())
            .unwrap()
            .unwrap();
        assert_eq!(actual.command, 1);
        let (id, vm) = steps(&store.connection);
        assert_eq!(id, 1);
        samples.push((count, vm));
    }
    eprintln!(
        "LED lookup VM steps at1k/10k/100k unrelated rows: {samples:?}; plan={:?}",
        plan(&store.connection)
    );
    assert!(
        samples.iter().all(|(_, vm)| *vm <= 128),
        "exact scope lookup must not scan whole-boot history: {samples:?}"
    );
    assert!(plan(&store.connection).iter().any(|s| s.contains(INDEX)));
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        100001
    );
}
#[test]
fn old_current_schema_open_adds_index_once_without_changing_any_journal_row() {
    let (db, store) = setup();
    row(
        &store.connection,
        1,
        ("boot", 1, "led", (1, 1)),
        &[1, 6, 1, 0],
        "written",
    );
    row(
        &store.connection,
        2,
        ("boot", 1, "led", (1, 1)),
        &[1, 6, 0, 0],
        "uncertain",
    );
    store
        .connection
        .execute_batch("DROP INDEX IF EXISTS command_led_request_scope")
        .unwrap();
    let before = dump(&store.connection);
    let version = store
        .connection
        .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
        .unwrap();
    drop(store);
    let reopened = Store::open(&db.0).unwrap();
    assert_eq!(dump(&reopened.connection), before);
    assert_eq!(
        reopened
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        version
    );
    assert_eq!(
        reopened
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [INDEX],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        reopened
            .latest_led_request("boot", 1, &DeviceId::new("led").unwrap(), epochs())
            .unwrap()
            .unwrap()
            .status,
        "uncertain"
    );
    drop(reopened);
    let again = Store::open(&db.0).unwrap();
    assert_eq!(dump(&again.connection), before);
    assert!(plan(&again.connection).iter().any(|s| s.contains(INDEX)));
}
#[test]
fn extra_partial_index_is_compatible_with_legacy_schema_and_data() {
    let (db, store) = setup();
    row(
        &store.connection,
        1,
        ("boot", 1, "led", (1, 1)),
        &[1, 6, 1, 0],
        "written",
    );
    store.connection.execute_batch(DDL).unwrap();
    let before = dump(&store.connection);
    let version = store
        .connection
        .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
        .unwrap();
    drop(store);
    // This same test also executes against the frozen unmodified v11 opener.
    let legacy = Store::open(&db.0).unwrap();
    assert_eq!(dump(&legacy.connection), before);
    assert_eq!(
        legacy
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        version
    );
    assert!(plan(&legacy.connection).iter().any(|s| s.contains(INDEX)));
}
#[test]
fn scope_and_all_request_statuses_remain_exact() {
    let (_db, store) = setup();
    let c = &store.connection;
    row(c, 1, ("boot", 1, "led", (1, 1)), &[1, 6, 1, 0], "written");
    for (id, boot, rev, device, epoch, intent) in [
        (2, "other", 1, "led", (1, 1), [1, 6, 0, 0]),
        (3, "boot", 2, "led", (1, 1), [1, 6, 0, 0]),
        (4, "boot", 1, "other", (1, 1), [1, 6, 0, 0]),
        (5, "boot", 1, "led", (2, 1), [1, 6, 0, 0]),
        (6, "boot", 1, "led", (1, 2), [1, 6, 0, 0]),
        (7, "boot", 1, "led", (1, 1), [1, 9, 0xc8, 2]),
    ] {
        row(c, id, (boot, rev, device, epoch), &intent, "written");
    }
    assert_eq!(
        store
            .latest_led_request("boot", 1, &DeviceId::new("led").unwrap(), epochs())
            .unwrap()
            .unwrap()
            .command,
        1
    );
    for (offset, status) in [
        "accepted",
        "writing",
        "written",
        "observed",
        "cancelled",
        "failed",
        "uncertain",
    ]
    .into_iter()
    .enumerate()
    {
        let id = 10 + offset as i64;
        row(c, id, ("boot", 1, "led", (1, 1)), &[1, 10, 0, 0], status);
        let found = store
            .latest_led_request("boot", 1, &DeviceId::new("led").unwrap(), epochs())
            .unwrap()
            .unwrap();
        assert_eq!(found.command, id as u64);
        assert_eq!(found.status, status);
    }
}
