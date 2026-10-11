use homesense_core::{
    Command, DeviceId,
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_storage::{Store, journal::Status};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "homesense-journal-{}-{}.sqlite",
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
fn command(id: u64) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: DeviceId::new("light").unwrap(),
        command: Command::Switch(true),
        origin: Origin::Hap,
        epochs: Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        },
        deadline_ms: 1000,
        run_id: None,
    }
}
#[test]
fn acceptance_is_atomic_under_insert_failure_and_revision_conflict() {
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON command_journal WHEN NEW.command_id=2 BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .accept_commands("boot1", &[command(1), command(2)], 10)
            .is_err()
    );
    assert_eq!(store.command_status("boot1", 1).unwrap(), None);
    connection
        .execute_batch("DROP TRIGGER fail_second;")
        .unwrap();
    store
        .accept_commands("boot1", &[command(1), command(2)], 10)
        .unwrap();
    assert!(
        store
            .accept_commands("boot1", &[command(3), command(1)], 10)
            .is_err()
    );
    assert_eq!(store.command_status("boot1", 3).unwrap(), None);
    store.publish(Some(1), 2, b"new", &[]).unwrap();
    assert!(store.accept_commands("boot1", &[command(3)], 10).is_err());
    assert_eq!(store.command_status("boot1", 3).unwrap(), None);
}
#[test]
fn restart_marks_old_nonterminal_records_uncertain_without_replay() {
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    store
        .accept_commands("boot1", &(1..=4).map(command).collect::<Vec<_>>(), 10)
        .unwrap();
    assert!(
        store
            .transition_command("boot1", 1, Status::Accepted, Status::Observed, 11)
            .is_err()
    );
    assert!(
        store
            .transition_command("boot1", 1, Status::Accepted, Status::Writing, 9)
            .is_err()
    );
    for id in 2..=4 {
        store
            .transition_command("boot1", id, Status::Accepted, Status::Writing, 11)
            .unwrap();
    }
    for id in 3..=4 {
        store
            .transition_command("boot1", id, Status::Writing, Status::Written, 12)
            .unwrap();
    }
    store
        .transition_command("boot1", 4, Status::Written, Status::Observed, 13)
        .unwrap();
    drop(store);
    let mut store = Store::open(&db.0).unwrap();
    store.accept_commands("boot2", &[command(1)], 0).unwrap();
    assert_eq!(store.recover_commands("boot2").unwrap(), 3);
    assert_eq!(store.recover_commands("boot2").unwrap(), 0);
    for id in 1..=3 {
        assert_eq!(
            store.command_status("boot1", id).unwrap(),
            Some(Status::Uncertain)
        );
    }
    assert_eq!(
        store.command_status("boot1", 4).unwrap(),
        Some(Status::Observed)
    );
    assert_eq!(
        store.command_status("boot2", 1).unwrap(),
        Some(Status::Accepted)
    );
    assert!(
        store
            .transition_command("boot1", 1, Status::Accepted, Status::Writing, 14)
            .is_err()
    );
}
#[test]
fn version_one_database_migrates_without_losing_configuration() {
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"original", &[]).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection
        .execute_batch(
            "DROP TABLE command_journal; DROP TABLE light_restore; DROP TABLE study_mode; PRAGMA user_version=1;",
        )
        .unwrap();
    drop(connection);
    let mut store = Store::open(&db.0).unwrap();
    assert_eq!(store.active().unwrap().unwrap().payload, b"original");
    store.accept_commands("newboot", &[command(1)], 0).unwrap();
    assert!(
        store
            .accept_commands("newboot", &[command(2)], 1000)
            .is_err()
    );
    assert!(
        store
            .accept_commands("newboot", &[command(u64::MAX)], 0)
            .is_err()
    );
}

#[test]
fn queue_publication_follows_atomic_journal_commit_including_coalescing() {
    use homesense_core::{
        LevelTransition, Percent,
        admission::{Access, RuntimePolicy},
        arbiter::{Arbiter, BatchCommitError},
    };
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    let access = Access {
        authorized: true,
        device_exists: true,
        device_verified: true,
        existing_direct_manual: true,
    };
    let mut arbiter = Arbiter::new(
        RuntimePolicy {
            epochs: command(1).epochs,
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        2,
        1,
    )
    .unwrap();
    let light = |id, n| {
        let mut c = command(id);
        c.command = Command::SetLevel {
            level: Percent::new(n).unwrap(),
            transition: LevelTransition::Immediate,
        };
        c
    };
    arbiter
        .submit_batch_committed(vec![(light(1, 30), access)], 0, |commands, cancelled| {
            store.accept_queue_batch("boot1", commands, cancelled, 0)
        })
        .unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_accept BEFORE INSERT ON command_journal WHEN NEW.command_id=2 BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        arbiter
            .submit_batch_committed(vec![(light(2, 60), access)], 1, |commands, cancelled| store
                .accept_queue_batch("boot1", commands, cancelled, 1)),
        Err(BatchCommitError::Persistence(_))
    ));
    assert_eq!(arbiter.queued_len(), 1);
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(store.command_status("boot1", 2).unwrap(), None);
    connection
        .execute_batch("DROP TRIGGER fail_accept;")
        .unwrap();
    let cancelled = arbiter
        .submit_batch_committed(vec![(light(2, 60), access)], 2, |commands, cancelled| {
            store.accept_queue_batch("boot1", commands, cancelled, 2)
        })
        .unwrap();
    assert_eq!(cancelled[0].command.id, 1);
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        store.command_status("boot1", 2).unwrap(),
        Some(Status::Accepted)
    );
    let reason: String = connection
        .query_row(
            "SELECT cancellation_reason FROM command_journal WHERE command_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "Superseded");
    assert_eq!(
        arbiter.dispatch(3, |_| access).unwrap().command.unwrap().id,
        2
    );
}

#[test]
fn same_batch_coalescing_and_failed_stop_commit_keep_consistent_audit() {
    use homesense_core::{
        MotorMotion,
        admission::{Access, RuntimePolicy},
        arbiter::Arbiter,
    };
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    let access = Access {
        authorized: true,
        device_exists: true,
        device_verified: true,
        existing_direct_manual: true,
    };
    let mut arbiter = Arbiter::new(
        RuntimePolicy {
            epochs: command(1).epochs,
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        2,
        1,
    )
    .unwrap();
    let mut movement = command(1);
    movement.command = Command::Motor(MotorMotion::Up);
    arbiter
        .submit_batch_committed(vec![(movement, access)], 0, |commands, cancelled| {
            store.accept_queue_batch("boot1", commands, cancelled, 0)
        })
        .unwrap();
    let mut stop = command(2);
    stop.command = Command::Motor(MotorMotion::Stop);
    assert!(
        arbiter
            .submit_batch_committed(vec![(stop.clone(), access)], 1, |_, _| Err(
                "storage unavailable"
            ))
            .is_err()
    );
    let cancelled = arbiter
        .submit_batch_committed(vec![(stop, access)], 2, |commands, cancelled| {
            store.accept_queue_batch("boot1", commands, cancelled, 2)
        })
        .unwrap();
    assert_eq!(cancelled[0].command.id, 1);
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        arbiter.dispatch(3, |_| access).unwrap().command.unwrap().id,
        2
    );
    arbiter.finish_write(2, true).unwrap();
    let mut third = command(3);
    third.command = Command::SetLevel {
        level: homesense_core::Percent::new(30).unwrap(),
        transition: homesense_core::LevelTransition::Immediate,
    };
    let mut fourth = third.clone();
    fourth.id = 4;
    fourth.command = Command::SetLevel {
        level: homesense_core::Percent::new(70).unwrap(),
        transition: homesense_core::LevelTransition::Immediate,
    };
    arbiter
        .submit_batch_committed(
            vec![(third, access), (fourth, access)],
            4,
            |commands, cancelled| store.accept_queue_batch("boot1", commands, cancelled, 4),
        )
        .unwrap();
    assert_eq!(
        store.command_status("boot1", 3).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        store.command_status("boot1", 4).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        arbiter.dispatch(5, |_| access).unwrap().command.unwrap().id,
        4
    );
}

#[test]
fn publication_cancellation_is_atomic_and_survives_failed_configuration_commit() {
    use homesense_core::queue::{CancelReason, Cancellation};
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"old", &[]).unwrap();
    store
        .accept_commands("boot1", &[command(1), command(2)], 10)
        .unwrap();
    let cancelled = |command| Cancellation {
        command,
        reason: CancelReason::ConfigurationChanged,
    };
    let mut wrong = command(2);
    wrong.epochs.control += 1;
    assert!(
        store
            .cancel_queued("boot1", &[cancelled(command(1)), cancelled(wrong)], 20)
            .is_err()
    );
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Accepted)
    );
    store
        .transition_command("boot1", 2, Status::Accepted, Status::Writing, 20)
        .unwrap();
    assert!(
        store
            .cancel_queued("boot1", &[cancelled(command(1)), cancelled(command(2))], 30)
            .is_err()
    );
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Accepted)
    );
    store
        .cancel_queued("boot1", &[cancelled(command(1))], 30)
        .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_publication BEFORE INSERT ON configuration_revision BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(store.publish(Some(1), 2, b"new", &[]).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    drop(store);
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        store.command_status("boot1", 2).unwrap(),
        Some(Status::Writing)
    );
    let reason: String = sql.query_row("SELECT cancellation_reason FROM command_journal WHERE boot_id='boot1' AND command_id=1", [], |r| r.get(0)).unwrap();
    assert_eq!(reason, "ConfigurationChanged");
}
