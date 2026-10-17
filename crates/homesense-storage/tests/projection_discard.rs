use homesense_core::{
    Command, DeviceId,
    queue::{CancelReason, Epochs, Origin, QueuedCommand},
};
use homesense_storage::{Store, journal::Status};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "projection-discard-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
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
fn setup(commands: &[Command]) -> (Database, Store) {
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    let commands = commands
        .iter()
        .enumerate()
        .map(|(index, command)| QueuedCommand {
            id: index as u64 + 1,
            device_id: DeviceId::new(format!("device-{index}")).unwrap(),
            command: *command,
            origin: Origin::Rule,
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
            deadline_ms: 15_000,
            run_id: Some(1),
        })
        .collect::<Vec<_>>();
    store.accept_commands("boot1", &commands, 10).unwrap();
    for command in &commands {
        store
            .reserve_command_write("boot1", command.id, 20, None)
            .unwrap()
            .unwrap()
            .release();
    }
    (db, store)
}
fn assert_writing(store: &Store, ids: &[u64]) {
    for id in ids {
        assert_eq!(
            store.command_status("boot1", *id).unwrap(),
            Some(Status::Writing)
        );
    }
}

#[test]
fn shortened_projection_deadline_cancels_whole_batch_without_renewing_persisted_budget() {
    let (db, mut store) = setup(&[
        Command::PanelLed(true),
        Command::PanelLed(false),
        Command::PanelLedOff,
    ]);
    let members = [(1, 4_500), (2, 4_500), (3, 4_500)];
    assert!(
        store
            .cancel_projection_before_delivery("boot1", &members, 4_499, CancelReason::Expired)
            .is_err()
    );
    assert_writing(&store, &[1, 2, 3]);
    store
        .cancel_projection_before_delivery("boot1", &members, 4_500, CancelReason::Expired)
        .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    for id in 1..=3 {
        assert_eq!(
            store.command_status("boot1", id).unwrap(),
            Some(Status::Cancelled)
        );
        let row = sql.query_row("SELECT deadline_ms,cancellation_reason FROM command_journal WHERE boot_id='boot1' AND command_id=?1", [id as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).unwrap();
        assert_eq!(row, (15_000, "Expired".into()));
    }
    assert!(
        store
            .cancel_projection_before_delivery("boot1", &members, 4_501, CancelReason::Expired)
            .is_err()
    );
    assert!(
        store
            .transition_command("boot1", 1, Status::Cancelled, Status::Written, 4_502)
            .is_err()
    );
}

#[test]
fn projection_discard_rejects_invalid_bounds_and_actuators_atomically() {
    let (_db, mut store) = setup(&[Command::PanelLed(true), Command::Switch(true)]);
    for members in [
        vec![],
        vec![(1, 0)],
        vec![(1, 16_000)],
        vec![(1, 1_000), (1, 1_000)],
        vec![(1, 1_000), (2, 1_000)],
    ] {
        assert!(
            store
                .cancel_projection_before_delivery("boot1", &members, 16_000, CancelReason::Expired)
                .is_err()
        );
        assert_writing(&store, &[1, 2]);
    }
    assert!(
        store
            .cancel_projection_before_delivery("boot1", &[(1, 1_000)], 2_000, CancelReason::Stopped)
            .is_err()
    );
    assert_writing(&store, &[1, 2]);
    assert!(
        store
            .transition_command("boot1", 2, Status::Writing, Status::Cancelled, 2_000)
            .is_err()
    );
}

#[test]
fn projection_discard_sql_failure_rolls_back_every_member_and_preserves_actuator_obligation() {
    let (db, mut store) = setup(&[
        Command::PanelLed(true),
        Command::PanelLed(false),
        Command::Switch(true),
    ]);
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_second_projection BEFORE UPDATE OF status ON command_journal WHEN NEW.command_id=2 AND NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'injected projection cancellation failure'); END;").unwrap();
    assert!(
        store
            .cancel_projection_before_delivery(
                "boot1",
                &[(1, 4_500), (2, 4_500)],
                3_000,
                CancelReason::RunCancelled
            )
            .is_err()
    );
    assert_writing(&store, &[1, 2, 3]);
    sql.execute_batch("DROP TRIGGER fail_second_projection")
        .unwrap();
    store
        .cancel_projection_before_delivery(
            "boot1",
            &[(1, 4_500), (2, 4_500)],
            3_000,
            CancelReason::RunCancelled,
        )
        .unwrap();
    for id in 1..=2 {
        assert_eq!(
            store.command_status("boot1", id).unwrap(),
            Some(Status::Cancelled)
        );
        assert_eq!(sql.query_row("SELECT cancellation_reason FROM command_journal WHERE boot_id='boot1' AND command_id=?1", [id as i64], |r| r.get::<_, String>(0)).unwrap(), "RunCancelled");
    }
    assert_writing(&store, &[3]);
    store
        .transition_command("boot1", 3, Status::Writing, Status::Written, 3_001)
        .unwrap();
}
