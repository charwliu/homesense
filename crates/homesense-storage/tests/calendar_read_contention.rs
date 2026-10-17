//! Real WAL connections; no transport, mocked SQL, sleeps, or timeout changes.
use homesense_storage::{Error, Store, calendar::Checkpoint};
use rusqlite::{Connection, ErrorCode};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
const STAMP: i64 = 1_700_000_000_000;
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
struct Fixture {
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "homesense-calendar-contention-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("runtime.sqlite")
    }
    fn open(&self) -> Store {
        let mut store = Store::open(self.path()).unwrap();
        store
            .publish_configuration(
                None,
                include_bytes!("../../../config/simulation.json"),
                homesense_core::ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        assert_eq!(
            store
                .calendar_watermark(
                    1,
                    Some(Checkpoint {
                        expected: None,
                        next: STAMP
                    })
                )
                .unwrap(),
            Some(STAMP)
        );
        store
    }
    fn writer(&self) -> Connection {
        let writer = Connection::open(self.path()).unwrap();
        assert_eq!(
            writer
                .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn busy(value: &Result<Option<i64>, Error>) -> bool {
    matches!(value,Err(Error::Database(rusqlite::Error::SqliteFailure(e,_))) if e.code==ErrorCode::DatabaseBusy)
}
#[test]
fn calendar_read_none_does_not_require_an_unrelated_wal_writer_reservation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let writer = fixture.writer();
    let start = Instant::now();
    let result = store.calendar_watermark(1, None);
    let elapsed = start.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(STAMP));
    eprintln!(
        "calendar_none_unrelated_writer result={result:?} elapsed_us={} no_transport=true",
        elapsed.as_micros()
    );
    assert_eq!(
        result.unwrap(),
        Some(STAMP),
        "a read-only watermark snapshot must not reserve a writer"
    );
}
#[test]
fn calendar_read_none_returns_committed_snapshot_not_an_uncommitted_checkpoint() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let writer = fixture.writer();
    writer
        .execute(
            "UPDATE calendar_watermark SET unix_ms=?1 WHERE revision=1",
            [STAMP + 1],
        )
        .unwrap();
    let start = Instant::now();
    let result = store.calendar_watermark(1, None);
    let elapsed = start.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(STAMP));
    eprintln!(
        "calendar_none_uncommitted_writer result={result:?} elapsed_us={} no_transport=true",
        elapsed.as_micros()
    );
    assert_eq!(
        result.unwrap(),
        Some(STAMP),
        "only the committed watermark is visible under WAL"
    );
}
#[test]
fn calendar_checkpoint_keeps_writer_exclusion_and_exact_cas() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let writer = fixture.writer();
    let start = Instant::now();
    let blocked = store.calendar_watermark(
        1,
        Some(Checkpoint {
            expected: Some(STAMP),
            next: STAMP + 1,
        }),
    );
    let elapsed = start.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    eprintln!(
        "calendar_checkpoint_writer_exclusion busy={} elapsed_us={} no_transport=true",
        busy(&blocked),
        elapsed.as_micros()
    );
    assert!(
        busy(&blocked),
        "checkpoint updates must still require the IMMEDIATE writer lock"
    );
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(STAMP));
    assert!(matches!(
        store.calendar_watermark(
            1,
            Some(Checkpoint {
                expected: None,
                next: STAMP + 1
            })
        ),
        Err(Error::Conflict)
    ));
    assert_eq!(
        store
            .calendar_watermark(
                1,
                Some(Checkpoint {
                    expected: Some(STAMP),
                    next: STAMP + 1
                })
            )
            .unwrap(),
        Some(STAMP + 1)
    );
}
#[test]
fn calendar_snapshot_and_checkpoint_keep_revision_and_monotonicity_guards() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    assert!(matches!(
        store.calendar_watermark(2, None),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.calendar_watermark(
            2,
            Some(Checkpoint {
                expected: None,
                next: STAMP + 1
            })
        ),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.calendar_watermark(
            1,
            Some(Checkpoint {
                expected: Some(STAMP),
                next: STAMP - 1
            })
        ),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.calendar_watermark(
            1,
            Some(Checkpoint {
                expected: Some(STAMP),
                next: 946_684_799_999
            })
        ),
        Err(Error::InvalidAudit)
    ));
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(STAMP));
}
