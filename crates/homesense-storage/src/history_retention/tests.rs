use super::*;
use std::path::PathBuf;
static FIXTURE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const DAY: i64 = 86_400_000;
const START: i64 = 1_790_000_000_000;
struct Fixture {
    root: PathBuf,
    source: PathBuf,
    archives: PathBuf,
    db: Connection,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hs-retention-{}-{}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("state.sqlite");
        let mut store = crate::Store::open(&source).unwrap();
        store.publish(None, 1, b"fixture", &[]).unwrap();
        store.ensure_keypad_completion_audit().unwrap();
        drop(store);
        let db = Connection::open(&source).unwrap();
        db.pragma_update(None, "foreign_keys", "ON").unwrap();
        Self {
            archives: root.join("archives"),
            root,
            source,
            db,
        }
    }
    fn command(&self, boot: &str, id: i64, status: &str, intent: &[u8]) {
        self.db.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES(?1,?2,1,'light','background',?3,1,1,100,10,?4)",params![boot,id,intent,status]).unwrap();
    }
    fn seed(&self) {
        for id in 1..=3 {
            self.command("old", id, "written", &[1, 0, 0, 0]);
        }
    }
    fn tick(&self, now: i64) -> Result<Report, String> {
        maintain(&self.source, &self.archives, now)
    }
    fn warm(&self) {
        for day in 0..30 {
            self.tick(START + day * DAY).unwrap();
        }
    }
    fn count(&self) -> i64 {
        self.db
            .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
            .unwrap()
    }
    fn archive(&self) -> PathBuf {
        std::fs::read_dir(&self.archives)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn thirty_day_boundary_archives_exact_rows_before_pruning_and_preserves_rowids() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY - 1).unwrap().deleted, 0);
    assert_eq!(f.count(), 3);
    let retained: i64 =
        f.db.query_row(
            "SELECT rowid FROM command_journal WHERE command_id=3",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let report = f.tick(START + 30 * DAY).unwrap();
    assert_eq!((report.archived, report.deleted), (2, 2));
    assert_eq!(f.count(), 1);
    assert_eq!(
        f.db.query_row(
            "SELECT rowid FROM command_journal WHERE command_id=3",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        retained
    );
    let a = Connection::open_with_flags(f.archive(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        a.query_row(
            "SELECT count(*) FROM history_rows WHERE table_name='command_journal'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        a.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        ARCHIVE_ID
    );
    assert_eq!(
        a.query_row(
            "SELECT count(*) FROM history_rows WHERE table_name='configuration_revision'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(crate::Store::open(f.archive()).is_err()); // no replay/activation
    assert_eq!(f.tick(START + 31 * DAY).unwrap().deleted, 0);
    assert_eq!(
        f.db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.archive()).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}
#[test]
fn monotonic_timestamps_are_not_calendar_dates_and_changed_rows_restart_age() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    // Source milliseconds look ancient but cannot be treated as Unix timestamps.
    f.db.execute(
        "UPDATE command_journal SET updated_ms=11 WHERE command_id=1",
        [],
    )
    .unwrap();
    let r = f.tick(START + 30 * DAY).unwrap();
    assert_eq!(r.deleted, 1);
    assert!(
        f.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE command_id=1)",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}
#[test]
fn wall_clock_rollback_and_forward_gap_never_age_data_early() {
    let f = Fixture::new();
    f.seed();
    f.tick(START).unwrap();
    assert!(f.tick(START - 1).is_err());
    assert_eq!(f.count(), 3);
    let r = f.tick(START + 40 * DAY).unwrap();
    assert!(r.clock_reset);
    assert_eq!(r.deleted, 0);
    assert_eq!(f.count(), 3);
}
#[test]
fn pending_uncertain_and_current_actuator_fences_survive() {
    let f = Fixture::new();
    for (id, status) in [
        (1, "accepted"),
        (2, "writing"),
        (3, "uncertain"),
        (4, "failed"),
        (5, "written"),
    ] {
        f.command("current", id, status, &[1, 1, 1, 0]);
    }
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY).unwrap().deleted, 0);
    assert_eq!(f.count(), 5);
}
#[test]
fn old_actuator_retains_last_receipt_and_current_device_state() {
    let f = Fixture::new();
    for id in 1..=3 {
        f.command("old", id, "written", &[1, 2, 100, 0]);
    }
    f.command("new", 1, "written", &[1, 0, 0, 0]);
    let rowid: i64 =
        f.db.query_row(
            "SELECT rowid FROM command_journal WHERE boot_id='old' AND command_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    f.db.execute(
        "INSERT INTO light_setting VALUES(1,'light',100,?1)",
        [rowid],
    )
    .unwrap();
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY).unwrap().deleted, 1); // old #2 only
    assert_eq!(
        f.db.query_row(
            "SELECT count(*) FROM light_setting s JOIN command_journal j ON j.rowid=s.journal_row",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn declared_references_and_immutable_scope_triggers_are_preserved() {
    let f = Fixture::new();
    f.seed();
    f.db.execute(
        "INSERT INTO gateway_command_requirement VALUES('old',1,'gateway',1)",
        [],
    )
    .unwrap();
    f.db.execute_batch("CREATE TABLE future_reference(boot TEXT,id INTEGER,FOREIGN KEY(boot,id) REFERENCES command_journal(boot_id,command_id));INSERT INTO future_reference VALUES('old',2);").unwrap();
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY).unwrap().deleted, 0);
    assert_eq!(f.count(), 3);
    assert!(
        f.db.execute("DELETE FROM gateway_command_requirement", [])
            .is_err()
    );
}
#[test]
fn embedded_receipts_publication_and_group_references_are_roots() {
    let f = Fixture::new();
    for id in 1..=8 {
        f.command("old", id, "written", &[1, 0, 0, 0]);
    }
    f.db.execute("INSERT INTO component_mode_audit VALUES(1,'v',1,'p','old','c',1,'inference_invalidated','1,3')",[]).unwrap();
    f.db.execute(
        "INSERT INTO publication_cancellation VALUES(1,'old',2,'test')",
        [],
    )
    .unwrap();
    f.db.execute("UPDATE command_journal SET run_id=4 WHERE command_id=5", [])
        .unwrap();
    f.db.execute("INSERT INTO keypad_completion_audit VALUES('old',1,1,'key','request','p','c','v',1,1,1,1,10,10,6,'','{}','7')",[]).unwrap();
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY).unwrap().deleted, 0);
    assert_eq!(f.count(), 8);
}
#[test]
fn native_request_ids_are_exact_tokens_not_substrings() {
    let f = Fixture::new();
    for id in [1, 11, 12] {
        f.command("old", id, "written", &[1, 0, 0, 0]);
    }
    f.db.execute(
        "INSERT INTO logical_variable VALUES(1,'v','integer',0,1)",
        [],
    )
    .unwrap();
    f.db.execute("INSERT INTO native_scene_request VALUES('c','k','s','g','v',1,'old',1,1,1,100,0,0,'observed','11','',0,10)",[]).unwrap();
    f.warm();
    assert_eq!(f.tick(START + 30 * DAY).unwrap().deleted, 1);
    assert!(
        f.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE command_id=11)",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}
#[test]
fn archive_write_failure_never_deletes_or_overwrites() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    std::fs::write(&f.archives, b"not a directory").unwrap();
    assert!(f.tick(START + 30 * DAY).is_err());
    assert_eq!(f.count(), 3);
    assert_eq!(std::fs::read(&f.archives).unwrap(), b"not a directory");
}
#[test]
fn disk_io_race_revalidates_changed_rows_and_new_references() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    let r = maintain_inner(&f.source, &f.archives, START + 30 * DAY, || {
        f.db.execute(
            "UPDATE command_journal SET updated_ms=20 WHERE command_id=1",
            [],
        )
        .unwrap();
        f.db.execute(
            "INSERT INTO publication_cancellation VALUES(1,'old',2,'raced')",
            [],
        )
        .unwrap();
    })
    .unwrap();
    assert_eq!((r.archived, r.deleted), (2, 0));
    assert_eq!(f.count(), 3);
}
#[test]
fn blocked_writer_is_fail_fast_and_foreign_missing_databases_are_not_modified() {
    let f = Fixture::new();
    f.seed();
    f.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(f.tick(START).is_err());
    f.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.count(), 3);
    let missing = f.root.join("missing.sqlite");
    assert!(maintain(&missing, &f.archives, START).is_err());
    assert!(!missing.exists());
    let foreign = f.root.join("foreign.sqlite");
    let db = Connection::open(&foreign).unwrap();
    db.execute_batch("CREATE TABLE secret(v);INSERT INTO secret VALUES(42);")
        .unwrap();
    assert!(maintain(&foreign, &f.archives, START).is_err());
    assert_eq!(
        db.query_row("SELECT v FROM secret", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        42
    );
}
#[test]
fn scan_is_bounded_and_wraps_without_starving_old_records() {
    let f = Fixture::new();
    for id in 1..=260 {
        f.command("old", id, "written", &[1, 0, 0, 0]);
    }
    for n in 0..3 {
        assert!(f.tick(START + n).unwrap().scanned <= 128);
    }
    let n: i64 =
        f.db.query_row("SELECT count(*) FROM history_retention_command", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 260);
}
#[test]
fn archive_verifier_rejects_corruption_or_missing_content() {
    let f = Fixture::new();
    f.seed();
    let r = read_row(&f.db, 1).unwrap().unwrap();
    private_directory(&f.archives).unwrap();
    let path = f.archives.join("check.sqlite");
    archive(&f.db, &path, START, std::slice::from_ref(&r)).unwrap();
    let a = Connection::open(&path).unwrap();
    a.execute(
        "UPDATE history_rows SET payload='corrupt' WHERE table_name='command_journal'",
        [],
    )
    .unwrap();
    drop(a);
    assert!(verify_archive(&path, &[r]).is_err());
    assert_eq!(f.count(), 3);
}
#[test]
fn real_volume_scan_remains_bounded_and_observation_metadata_is_digest_only() {
    let f = Fixture::new();
    f.db.execute_batch("BEGIN").unwrap();
    for id in 1..=10_676 {
        f.command("old", id, "written", &[1, 0, 0, 0]);
    }
    f.db.execute_batch("COMMIT").unwrap();
    let r = f.tick(START).unwrap();
    assert_eq!(r.scanned, 128);
    let max: i64 =
        f.db.query_row(
            "SELECT max(length(snapshot)) FROM history_retention_command",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(max, 64);
}
#[test]
fn aged_real_volume_batch_archives_and_deletes_with_original_deadline() {
    let f = Fixture::new();
    f.db.execute_batch("BEGIN").unwrap();
    for id in 1..=10_676 {
        f.command("old", id, "written", &[1, 0, 0, 0]);
    }
    f.db.execute_batch("COMMIT").unwrap();
    f.tick(START).unwrap();
    // The fixture represents 30 daily successful observation intervals; product
    // code still enforces its unchanged 50ms progress deadline on the aged pass.
    f.db.execute(
        "UPDATE history_retention_clock SET last_unix_ms=?1,scan_after=0",
        [START + 30 * DAY],
    )
    .unwrap();
    let r = f.tick(START + 30 * DAY).unwrap();
    assert_eq!(r.deleted, 128);
    assert_eq!(f.count(), 10_676 - 128);
}
#[test]
fn retry_reuses_verified_archive_without_overwriting_or_duplicate_files() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    // Hold the source writer only after the archive is safely durable.
    let first = maintain_inner(&f.source, &f.archives, START + 30 * DAY, || {
        f.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    });
    assert!(first.is_err());
    assert_eq!(f.count(), 3);
    let path = f.archive();
    let bytes = std::fs::read(&path).unwrap();
    f.db.execute_batch("ROLLBACK").unwrap();
    let r = f.tick(START + 30 * DAY + 60_000).unwrap();
    assert_eq!(r.deleted, 2);
    assert_eq!(std::fs::read_dir(&f.archives).unwrap().count(), 1);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[cfg(unix)]
#[test]
fn symlink_archive_directory_is_rejected_before_source_delete() {
    let f = Fixture::new();
    f.seed();
    f.warm();
    let other = f.root.join("other");
    std::fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(&other, &f.archives).unwrap();
    assert!(f.tick(START + 30 * DAY).is_err());
    assert_eq!(f.count(), 3);
    assert_eq!(std::fs::read_dir(other).unwrap().count(), 0);
}
#[test]
fn long_gap_resets_age_with_constant_size_clock_write_not_bulk_marker_updates() {
    let f = Fixture::new();
    f.seed();
    f.tick(START).unwrap();
    let before: i64 =
        f.db.query_row(
            "SELECT first_seen_unix_ms FROM history_retention_command WHERE command_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let r = f.tick(START + 40 * DAY).unwrap();
    assert!(r.clock_reset);
    assert_eq!(r.deleted, 0);
    assert_eq!(
        f.db.query_row(
            "SELECT first_seen_unix_ms FROM history_retention_command WHERE command_id=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        before
    );
    for day in 41..70 {
        assert_eq!(f.tick(START + day * DAY).unwrap().deleted, 0);
    }
    assert_eq!(f.tick(START + 70 * DAY).unwrap().deleted, 2);
}
