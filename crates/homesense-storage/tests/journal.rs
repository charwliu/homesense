#[path = "support/pre_gateway_scope_fixture.rs"]
mod pre_gateway_scope_fixture;

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
    pre_gateway_scope_fixture::strip(&connection);
    connection
        .execute_batch(
            "DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; DROP TABLE scene_cancellation; DROP TABLE scene_sample; DROP TABLE scene_cause; DROP TABLE automation_revision; DROP TABLE command_journal; DROP TABLE light_restore; DROP TABLE study_mode; DROP TABLE logical_variable; DROP TABLE calendar_watermark; DROP TABLE rule_command; DROP TABLE rule_admission; DROP TABLE rule_chain_outcome; DROP TABLE scene_command; DROP TABLE scene_step; DROP TABLE scene_run; DROP TABLE module_off_effect; PRAGMA user_version=1;",
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

#[test]
fn logical_variables_and_command_acceptance_are_one_cas_transaction() {
    use homesense_core::rule::{Value, ValueType};
    use homesense_storage::variables::Update;
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    store
        .define_logical_variable(1, "mode", ValueType::Integer)
        .unwrap();
    let update = Update {
        revision: 1,
        name: "mode".into(),
        expected_version: 0,
        value: Some(Value::Integer(2)),
    };
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_variable BEFORE UPDATE ON logical_variable BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .accept_queue_batch_with_variables(
                "boot",
                &[command(1)],
                &[],
                10,
                None,
                std::slice::from_ref(&update)
            )
            .is_err()
    );
    assert_eq!(store.command_status("boot", 1).unwrap(), None);
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().value,
        None
    );
    connection
        .execute_batch("DROP TRIGGER fail_variable;")
        .unwrap();
    store
        .accept_queue_batch_with_variables(
            "boot",
            &[command(1)],
            &[],
            10,
            None,
            std::slice::from_ref(&update),
        )
        .unwrap();
    assert!(
        store
            .accept_queue_batch_with_variables("boot", &[command(2)], &[], 11, None, &[update])
            .is_err()
    );
    assert_eq!(store.command_status("boot", 2).unwrap(), None);
    drop(store);
    let mut store = Store::open(&db.0).unwrap();
    let value = store.logical_variable(1, "mode").unwrap().unwrap();
    assert_eq!(value.value, Some(Value::Integer(2)));
    assert_eq!(value.version, 1);
    let bad = Update {
        revision: 1,
        name: "mode".into(),
        expected_version: 1,
        value: Some(Value::Bool(true)),
    };
    assert!(
        store
            .accept_queue_batch_with_variables("boot", &[], &[], 12, None, &[bad])
            .is_err()
    );
    let clear = Update {
        revision: 1,
        name: "mode".into(),
        expected_version: 1,
        value: None,
    };
    store
        .accept_queue_batch_with_variables("boot", &[], &[], 12, None, &[clear])
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "mode").unwrap().unwrap().version,
        2
    );
    store.publish(Some(1), 2, b"new config", &[]).unwrap();
    assert!(store.logical_variable(2, "mode").unwrap().is_none());
}

#[test]
fn feedback_rules_wait_for_output_completion_and_a_newer_sample() {
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    let device = command(1).device_id;
    assert!(store.feedback_is_settled("boot1", &device, None).unwrap());
    store.accept_commands("boot1", &[command(1)], 10).unwrap();
    assert!(
        !store
            .feedback_is_settled("boot1", &device, Some(50))
            .unwrap()
    );
    store
        .transition_command("boot1", 1, Status::Accepted, Status::Writing, 11)
        .unwrap();
    assert!(
        !store
            .feedback_is_settled("boot1", &device, Some(50))
            .unwrap()
    );
    store
        .transition_command("boot1", 1, Status::Writing, Status::Written, 12)
        .unwrap();
    assert!(store.feedback_is_settled_in_epoch("boot1", &device, Some(12), 2).unwrap());
    assert!(!store.feedback_is_settled_in_epoch("boot1", &device, Some(12), 1).unwrap());
    assert!(!store.feedback_is_settled_in_epoch("boot1", &device, None, 2).unwrap());
    for sample in [None, Some(10), Some(12)] {
        assert!(!store.feedback_is_settled("boot1", &device, sample).unwrap());
    }
    assert!(
        store
            .feedback_is_settled("boot1", &device, Some(13))
            .unwrap()
    );
    assert!(
        store
            .feedback_is_settled("otherboot", &device, None)
            .unwrap()
    );
    store.accept_commands("boot1", &[command(2)], 20).unwrap();
    assert!(!store.feedback_is_settled_in_epoch("boot1", &device, Some(50), 2).unwrap());
    store
        .transition_command("boot1", 2, Status::Accepted, Status::Cancelled, 21)
        .unwrap();
    assert!(
        store
            .feedback_is_settled("boot1", &device, Some(13))
            .unwrap()
    );
}

#[test]
fn calendar_watermark_survives_reopen_and_rejects_stale_or_backward_checkpoints() {
    use homesense_storage::{Error, calendar::Checkpoint};
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"fixture", &[]).unwrap();
    assert_eq!(store.calendar_watermark(1, None).unwrap(), None);
    let now = 1_800_000_000_000;
    store
        .calendar_watermark(
            1,
            Some(Checkpoint {
                expected: None,
                next: now,
            }),
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&db.0).unwrap();
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(now));
    for update in [
        Checkpoint {
            expected: None,
            next: now + 1,
        },
        Checkpoint {
            expected: Some(now),
            next: now - 1,
        },
    ] {
        assert!(matches!(
            store.calendar_watermark(1, Some(update)),
            Err(Error::Conflict)
        ));
    }
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_calendar BEFORE UPDATE ON calendar_watermark BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        store
            .calendar_watermark(
                1,
                Some(Checkpoint {
                    expected: Some(now),
                    next: now + 1
                })
            )
            .is_err()
    );
    assert_eq!(store.calendar_watermark(1, None).unwrap(), Some(now));
    store.publish(Some(1), 2, b"next", &[]).unwrap();
    assert!(matches!(
        store.calendar_watermark(1, None),
        Err(Error::Conflict)
    ));
    assert_eq!(store.calendar_watermark(2, None).unwrap(), None);
}

#[test]
fn scene_receipts_preserve_terminal_states_and_interrupt_old_runs_without_replay() {
    use homesense_storage::scene_run::Status as Scene;
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    store
        .begin_scene_run("old", "complete", 1, Some("reading"), 10)
        .unwrap();
    assert!(
        store
            .begin_scene_run("old", "bad-revision", 2, None, 10)
            .is_err()
    );
    assert_eq!(
        store
            .checkpoint_scene_run("old", "complete", 0, Scene::Running, 11)
            .unwrap(),
        1
    );
    assert!(
        store
            .checkpoint_scene_run("old", "complete", 0, Scene::Cancelled, 12)
            .is_err()
    );
    assert!(
        store
            .checkpoint_scene_run("old", "complete", 1, Scene::Partial, 9)
            .is_err()
    );
    store
        .checkpoint_scene_run("old", "complete", 1, Scene::CommandsWritten, 12)
        .unwrap();
    assert!(
        store
            .checkpoint_scene_run("old", "complete", 2, Scene::Running, 13)
            .is_err()
    );
    store
        .begin_scene_run("old", "unfinished", 1, None, 20)
        .unwrap();
    store.begin_scene_run("new", "current", 1, None, 0).unwrap();
    drop(store);
    let mut store = Store::open(&db.0).unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_scene_recovery BEFORE UPDATE ON scene_run WHEN NEW.status='interrupted' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(store.recover_scene_runs("new").is_err());
    assert_eq!(
        store
            .scene_run("old", "unfinished")
            .unwrap()
            .unwrap()
            .status,
        Scene::Pending
    );
    sql.execute_batch("DROP TRIGGER fail_scene_recovery;")
        .unwrap();
    assert_eq!(store.recover_scene_runs("new").unwrap(), 1);
    assert_eq!(store.recover_scene_runs("new").unwrap(), 0);
    let record = store.scene_run("old", "unfinished").unwrap().unwrap();
    assert_eq!(record.status, Scene::Interrupted);
    assert_eq!(record.updated_ms, 20);
    assert_eq!(
        store.scene_run("old", "complete").unwrap().unwrap().status,
        Scene::CommandsWritten
    );
    assert_eq!(
        store.scene_run("new", "current").unwrap().unwrap().status,
        Scene::Pending
    );
    let commands: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(commands, 0);
}

#[test]
fn scene_step_links_commit_atomically_with_all_expanded_commands() {
    use homesense_storage::scene_run::StepLink;
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    store.begin_scene_run("boot", "scene", 1, None, 0).unwrap();
    let link = StepLink {
        sample: None,
        run: "scene".into(),
        index: 0,
        source: "combined:light".into(),
    };
    let mut commands = vec![command(1), command(2)];
    for c in &mut commands {
        c.origin = Origin::Rule;
    }
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_scene_link BEFORE INSERT ON scene_command WHEN NEW.command_id=2 BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .accept_queue_batch_with_audits("boot", &commands, &[], 1, None, &[], None, Some(&link))
            .is_err()
    );
    assert!(store.command_status("boot", 1).unwrap().is_none());
    assert!(store.command_status("boot", 2).unwrap().is_none());
    assert!(
        store
            .scene_commands("boot", "scene", 0, 128)
            .unwrap()
            .is_empty()
    );
    let count: i64 = sql
        .query_row("SELECT count(*) FROM scene_step", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    sql.execute_batch("DROP TRIGGER fail_scene_link;").unwrap();
    store
        .accept_queue_batch_with_audits("boot", &commands, &[], 1, None, &[], None, Some(&link))
        .unwrap();
    let rows = store.scene_commands("boot", "scene", 0, 128).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.step == 0 && r.source == "combined:light" && r.sample.is_none())
    );
    let mut next = command(3);
    next.origin = Origin::Rule;
    assert!(
        store
            .accept_queue_batch_with_audits("boot", &[next], &[], 2, None, &[], None, Some(&link))
            .is_err()
    );
    assert!(store.command_status("boot", 3).unwrap().is_none());
}

#[test]
fn latest_scene_uses_durable_insertion_order_across_boot_clocks_and_revisions() {
    use homesense_storage::scene_run::Query;
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"one", &[]).unwrap();
    store
        .begin_scene_run("old", "r1", 1, Some("reading"), 99999)
        .unwrap();
    store
        .begin_scene_run("new", "r2", 1, Some("reading"), 1)
        .unwrap();
    store
        .begin_scene_run("new", "unrelated", 1, Some("daily"), 2)
        .unwrap();
    let query = Query::Latest {
        definition: "reading".into(),
    };
    let latest = store.query_scene(1, &query).unwrap().unwrap();
    assert_eq!(latest.run, "r2");
    assert_eq!(latest.boot, "new");
    store.publish(Some(1), 2, b"two", &[]).unwrap();
    assert!(store.query_scene(2, &query).unwrap().is_none());
    store
        .begin_scene_run("new", "r3", 2, Some("reading"), 3)
        .unwrap();
    assert_eq!(store.query_scene(2, &query).unwrap().unwrap().run, "r3");
    assert_eq!(store.query_scene(1, &query).unwrap().unwrap().run, "r2");
    let explicit=Query::Run{boot:"new".into(),run:"r2".into(),after:0,limit:1};
    assert!(store.query_scene(2,&explicit).unwrap().is_none());
    assert_eq!(store.query_scene(1,&explicit).unwrap().unwrap().run,"r2");
    assert!(
        store
            .query_scene(
                2,
                &Query::Run {
                    boot: "new".into(),
                    run: "missing".into(),
                    after: 0,
                    limit: 1
                }
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn deferred_receipts_variables_and_immediate_commands_commit_atomically() {
    use homesense_core::rule::{Value,ValueType};
    let db=Database::new();let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.define_logical_variable(1,"mode",ValueType::Integer).unwrap();
    let update=homesense_storage::variables::Update{revision:1,name:"mode".into(),expected_version:0,value:Some(Value::Integer(2))};
    let audit=homesense_storage::rule_audit::Admission{revision:1,correlation:1,event:1,parent:0,rule:"rule".into()};
    let starts=["one","two"].into_iter().enumerate().map(|(action_index,run)|homesense_storage::scene_run::Start{action_index,source:"rule:call".into(),revision:1,run:run.into(),definition:"delayed".into()}).collect::<Vec<_>>();
    let mut command=command(1);command.origin=Origin::Rule;
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_deferred BEFORE INSERT ON scene_run WHEN NEW.run_id='two' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let accept=|store:&mut Store|store.accept_queue_batch_with_scene_starts("boot",std::slice::from_ref(&command),&[],10,None,std::slice::from_ref(&update),Some(&audit),None,&starts);
    assert!(accept(&mut store).is_err());
    assert!(store.scene_run("boot","one").unwrap().is_none());
    assert!(store.command_status("boot",1).unwrap().is_none());
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().version,0);
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    sql.execute_batch("DROP TRIGGER fail_deferred; CREATE TRIGGER fail_variable BEFORE UPDATE ON logical_variable BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(accept(&mut store).is_err());
    assert!(store.scene_run("boot","one").unwrap().is_none());
    assert!(store.scene_run("boot","two").unwrap().is_none());
    sql.execute_batch("DROP TRIGGER fail_variable;").unwrap();
    accept(&mut store).unwrap();
    for run in ["one","two"] {assert_eq!(store.scene_run("boot",run).unwrap().unwrap().status,homesense_storage::scene_run::Status::Pending);}
    assert_eq!(store.command_status("boot",1).unwrap(),Some(Status::Accepted));
    assert_eq!(store.logical_variable(1,"mode").unwrap().unwrap().value,Some(Value::Integer(2)));
    assert!(accept(&mut store).is_err(),"duplicate acceptance cannot be retried as new work");
    drop(store);
    let store=Store::open(&db.0).unwrap();
    for run in ["one","two"] {
        let cause=store.scene_cause("boot",run).unwrap().unwrap();
        assert_eq!(cause.correlation,1);assert_eq!(cause.event,1);
        assert_eq!(cause.source,"rule:call");
        let page=store.query_scene(1,&homesense_storage::scene_run::Query::Run{boot:"boot".into(),run:run.into(),after:0,limit:1}).unwrap().unwrap();
        assert_eq!(page.cause,Some(cause));
    }
    assert!(store.scene_cause("boot","missing").unwrap().is_none());
}

#[test]
fn schema_twelve_migration_preserves_existing_manual_scene_without_inventing_cause() {
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","manual",1,Some("scene"),10).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; DROP TABLE scene_cancellation; DROP TABLE scene_sample; DROP TABLE scene_cause; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=12;").unwrap();
    drop(sql);
    let store=Store::open(&db.0).unwrap();
    assert_eq!(store.scene_run("boot","manual").unwrap().unwrap().started_ms,10);
    assert!(store.scene_cause("boot","manual").unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}

#[test]
fn fade_samples_append_contiguously_and_rollback_with_commands() {
    use homesense_storage::scene_run::StepLink;
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","fade",1,None,0).unwrap();
    let mut link=StepLink{run:"fade".into(),index:0,source:"fade-source".into(),sample:Some(0)};
    let mut c=command(1);c.origin=Origin::Rule;
    store.accept_queue_batch_with_audits("boot",&[c],&[],1,None,&[],None,Some(&link)).unwrap();
    for (sample,source) in [(0,"fade-source"),(2,"fade-source"),(1,"changed"),(1024,"fade-source")] {
        link.sample=Some(sample);link.source=source.into();
        let mut c=command(2);c.origin=Origin::Rule;
        assert!(store.accept_queue_batch_with_audits("boot",&[c],&[],2,None,&[],None,Some(&link)).is_err());
        assert!(store.command_status("boot",2).unwrap().is_none());
    }
    link.sample=Some(1);link.source="fade-source".into();
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_sample BEFORE INSERT ON scene_sample WHEN NEW.sample_index=1 BEGIN SELECT RAISE(ABORT,'sample failure'); END;").unwrap();
    let mut c=command(2);c.origin=Origin::Rule;
    assert!(store.accept_queue_batch_with_audits("boot",&[c.clone()],&[],2,None,&[],None,Some(&link)).is_err());
    assert!(store.command_status("boot",2).unwrap().is_none());
    assert_eq!(store.scene_commands("boot","fade",0,128).unwrap().len(),1);
    sql.execute_batch("DROP TRIGGER reject_sample;").unwrap();
    store.accept_queue_batch_with_audits("boot",&[c],&[],2,None,&[],None,Some(&link)).unwrap();
    assert_eq!(store.scene_commands("boot","fade",0,128).unwrap().len(),2);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_sample",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    let rows=store.scene_commands("boot","fade",0,128).unwrap();
    assert_eq!(rows.iter().map(|r|r.sample).collect::<Vec<_>>(),vec![Some(0),Some(1)]);
    assert_eq!(store.scene_commands("boot","fade",1,1).unwrap()[0].sample,Some(1));

    store.checkpoint_scene_run("boot","fade",0,homesense_storage::scene_run::Status::Cancelled,3).unwrap();
    link.sample=Some(2);
    let mut c=command(3);c.origin=Origin::Rule;
    assert!(store.accept_queue_batch_with_audits("boot",&[c],&[],3,None,&[],None,Some(&link)).is_err());
    assert!(store.command_status("boot",3).unwrap().is_none());
}

#[test]
fn schema_thirteen_keeps_scene_links_without_inventing_fade_samples() {
    use homesense_storage::scene_run::StepLink;
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","scene",1,None,0).unwrap();
    let link=StepLink{run:"scene".into(),index:0,source:"ordinary".into(),sample:None};
    let mut c=command(1);c.origin=Origin::Rule;
    store.accept_queue_batch_with_audits("boot",&[c],&[],1,None,&[],None,Some(&link)).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; DROP TABLE scene_cancellation; DROP TABLE scene_sample; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=13;").unwrap();
    let store=Store::open(&db.0).unwrap();
    let rows=store.scene_commands("boot","scene",0,128).unwrap();
    assert_eq!(rows.len(),1);
    assert_eq!(rows[0].source,"ordinary");
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_sample",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}

#[test]
fn expanded_sample_commands_keep_sample_identity_across_pages() {
    use homesense_storage::scene_run::StepLink;
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","fade",1,None,0).unwrap();
    for (sample,ids) in [(0,vec![10,11]),(1,vec![12,13])] {
        let commands=ids.into_iter().map(|id|{let mut c=command(id);c.origin=Origin::Rule;c}).collect::<Vec<_>>();
        let link=StepLink{run:"fade".into(),index:0,source:"combined".into(),sample:Some(sample)};
        store.accept_queue_batch_with_audits("boot",&commands,&[],1,None,&[],None,Some(&link)).unwrap();
    }
    let page=store.scene_commands("boot","fade",10,2).unwrap();
    assert_eq!(page.iter().map(|r|(r.command_id,r.sample)).collect::<Vec<_>>(),vec![(11,Some(0)),(12,Some(1))]);
    let link=StepLink{run:"fade".into(),index:0,source:"combined".into(),sample:Some(2)};
    for ids in [vec![9],vec![15,14]] {
        let commands=ids.into_iter().map(|id|{let mut c=command(id);c.origin=Origin::Rule;c}).collect::<Vec<_>>();
        assert!(store.accept_queue_batch_with_audits("boot",&commands,&[],2,None,&[],None,Some(&link)).is_err());
    }
    assert_eq!(store.scene_commands("boot","fade",0,128).unwrap().len(),4);
}

#[test]
fn terminal_scene_preserves_accepted_motor_stop_only() {
    use homesense_core::MotorMotion;
    use homesense_storage::scene_run::{StepLink, Status as SceneStatus};
    let db = Database::new();
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"config", &[]).unwrap();
    store.begin_scene_run("boot", "scene", 1, None, 0).unwrap();
    for (index, action) in [Command::Motor(MotorMotion::Stop), Command::Motor(MotorMotion::Up), Command::Motor(MotorMotion::Down), Command::Switch(true)].into_iter().enumerate() {
        let mut c = command(index as u64 + 1);
        c.origin = Origin::Rule;
        c.command = action;
        let link = StepLink { run: "scene".into(), index, source: "synthetic:stop".into(), sample: None };
        store.accept_queue_batch_with_audits("boot", &[c], &[], 1, None, &[], None, Some(&link)).unwrap();
    }
    store.checkpoint_scene_run("boot", "scene", 0, SceneStatus::Cancelled, 2).unwrap();
    assert!(store.begin_command_write("boot", 1, 3).unwrap());
    assert_eq!(store.command_status("boot", 1).unwrap(), Some(Status::Writing));
    for id in 2..=4 {
        assert!(!store.begin_command_write("boot", id, 3).unwrap());
        assert_eq!(store.command_status("boot", id).unwrap(), Some(Status::Cancelled));
    }
    assert!(store.begin_command_write("boot", 1, 4).is_err());
}

#[test]
fn scene_cancellation_is_atomic_with_checkpoint_and_survives_schema_upgrade() {
    use homesense_storage::scene_run::{Cancellation, CancellationReason, Status as Scene};
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","scene",1,None,10).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; DROP TABLE scene_cancellation; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=14;").unwrap();
    let mut store=Store::open(&db.0).unwrap();
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,0);
    let reason=Cancellation{device:DeviceId::new("synthetic:light").unwrap(),reason:CancellationReason::SceneReplacement};
    sql.execute_batch("CREATE TRIGGER fail_cancel_reason BEFORE INSERT ON scene_cancellation BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Running,11,Some(&reason)).is_err());
    let unchanged=store.scene_run("boot","scene").unwrap().unwrap();
    assert_eq!(unchanged.version,0);assert_eq!(unchanged.status,Scene::Pending);
    sql.execute_batch("DROP TRIGGER fail_cancel_reason;").unwrap();
    assert_eq!(store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Running,11,Some(&reason)).unwrap(),1);
    assert!(store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Cancelled,12,Some(&reason)).is_err());
    let manual=Cancellation{reason:CancellationReason::ManualOverride,..reason};
    assert_eq!(store.checkpoint_scene_run_with_cancellation("boot","scene",1,Scene::Cancelled,12,Some(&manual)).unwrap(),2);
    drop(store);
    let reopened=Store::open(&db.0).unwrap();
    assert_eq!(reopened.scene_run("boot","scene").unwrap().unwrap().status,Scene::Cancelled);
    let mut stmt=sql.prepare("SELECT version,device_id,reason FROM scene_cancellation ORDER BY version").unwrap();
    let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    assert_eq!(rows,vec![(1,"synthetic:light".into(),"scene_replacement".into()),(2,"synthetic:light".into(),"manual_override".into())]);
    let query=|after,limit|homesense_storage::scene_run::Query::Cancellations{boot:"boot".into(),run:"scene".into(),after,limit};
    let first=reopened.query_scene(1,&query(0,1)).unwrap().unwrap();
    assert!(first.commands.is_empty());
    assert_eq!(first.cancellations.len(),1);
    assert_eq!(first.cancellations[0].cancellation.reason,CancellationReason::SceneReplacement);
    assert_eq!(first.next_after,Some(1));
    let second=reopened.query_scene(1,&query(first.next_after.unwrap(),1)).unwrap().unwrap();
    assert_eq!(second.cancellations[0].cancellation.reason,CancellationReason::ManualOverride);
    assert_eq!(second.next_after,Some(2));
    assert!(reopened.query_scene(1,&query(2,1)).unwrap().unwrap().cancellations.is_empty());
    assert!(reopened.query_scene(2,&query(0,1)).unwrap().is_none());
    for (after,limit) in [(0,0),(0,129),(u64::MAX,1)] {assert!(reopened.query_scene(1,&query(after,limit)).is_err());}

}

#[test]
fn schema_fifteen_preserves_cancellation_history_and_accepts_cancel_request() {
    use homesense_storage::scene_run::{Cancellation, CancellationReason, Query, Status as Scene};
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","scene",1,None,0).unwrap();
    let device=DeviceId::new("synthetic:light").unwrap();
    store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Running,1,Some(&Cancellation{device:device.clone(),reason:CancellationReason::ManualOverride})).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("ALTER TABLE scene_cancellation RENAME TO newer;
        CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement')), PRIMARY KEY(boot_id,run_id,version), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));
        INSERT INTO scene_cancellation SELECT * FROM newer; DROP TABLE newer; DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=15;").unwrap();
    let mut store=Store::open(&db.0).unwrap();
    let request=Cancellation{device,reason:CancellationReason::CancelRequested};
    sql.execute_batch("CREATE TRIGGER reject_request BEFORE INSERT ON scene_cancellation BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(store.checkpoint_scene_run_with_cancellation("boot","scene",1,Scene::Cancelled,2,Some(&request)).is_err());
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,1);
    sql.execute_batch("DROP TRIGGER reject_request;").unwrap();
    store.checkpoint_scene_run_with_cancellation("boot","scene",1,Scene::Cancelled,2,Some(&request)).unwrap();
    drop(store);
    let store=Store::open(&db.0).unwrap();
    let page=store.query_scene(1,&Query::Cancellations{boot:"boot".into(),run:"scene".into(),after:0,limit:128}).unwrap().unwrap();
    assert_eq!(page.cancellations.iter().map(|v|v.cancellation.reason).collect::<Vec<_>>(),vec![CancellationReason::ManualOverride,CancellationReason::CancelRequested]);
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().status,Scene::Cancelled);
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}

#[test]
fn expired_scene_step_migrates_atomically_and_cannot_be_admitted() {
    use homesense_storage::scene_run::{Expiry,Status as Scene,StepLink};
    let db=Database::new();let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    store.begin_scene_run("boot","scene",1,None,0).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("DROP TABLE light_setting; DROP TABLE feedback_inference; DROP TABLE scene_expiry; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=16;").unwrap();
    let mut store=Store::open(&db.0).unwrap();
    let expiry=Expiry{index:0,device:DeviceId::new("synthetic:light").unwrap(),source:"synthetic:late".into()};
    sql.execute_batch("CREATE TRIGGER fail_expiry BEFORE INSERT ON scene_expiry BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(store.checkpoint_scene_run_with_expiry("boot","scene",0,Scene::Running,10,&expiry).is_err());
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,0);
    sql.execute_batch("DROP TRIGGER fail_expiry;").unwrap();
    assert_eq!(store.checkpoint_scene_run_with_expiry("boot","scene",0,Scene::Running,10,&expiry).unwrap(),1);
    assert!(store.checkpoint_scene_run_with_expiry("boot","scene",1,Scene::Running,11,&expiry).is_err());
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,1);
    let mut c=command(1);c.origin=Origin::Rule;
    let mut link=StepLink{run:"scene".into(),index:0,source:"synthetic:late".into(),sample:None};
    assert!(store.accept_queue_batch_with_audits("boot",&[c.clone()],&[],11,None,&[],None,Some(&link)).is_err());
    assert!(store.command_status("boot",1).unwrap().is_none());
    link.index=1;
    store.accept_queue_batch_with_audits("boot",&[c],&[],11,None,&[],None,Some(&link)).unwrap();
    assert!(store.checkpoint_scene_run_with_expiry("boot","scene",1,Scene::Running,12,&Expiry{index:1,..expiry.clone()}).is_err());
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,1);
    drop(store);let store=Store::open(&db.0).unwrap();
    assert_eq!(store.scene_run("boot","scene").unwrap().unwrap().version,1);
    assert_eq!(sql.query_row("SELECT source FROM scene_expiry",[],|r|r.get::<_,String>(0)).unwrap(),expiry.source);
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}

#[test]
fn expiry_history_pages_by_checkpoint_version_and_configuration() {
    use homesense_storage::scene_run::{Expiry,Status as Scene,Query};
    let db=Database::new();let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();store.begin_scene_run("boot","scene",1,None,0).unwrap();
    for (index,version) in [(3,0),(1,1),(7,2)] {
        store.checkpoint_scene_run_with_expiry("boot","scene",version,Scene::Running,version+1,&Expiry{index,device:DeviceId::new("synthetic:light").unwrap(),source:format!("synthetic:{index}")}).unwrap();
    }
    let query=|after,limit|Query::Expiries{boot:"boot".into(),run:"scene".into(),after,limit};
    let first=store.query_scene(1,&query(0,2)).unwrap().unwrap();
    assert!(first.commands.is_empty() && first.cancellations.is_empty());
    assert_eq!(first.expiries.iter().map(|r|(r.version,r.expiry.index)).collect::<Vec<_>>(),vec![(1,3),(2,1)]);
    assert_eq!(first.next_after,Some(2));
    let second=store.query_scene(1,&query(2,2)).unwrap().unwrap();
    assert_eq!(second.expiries[0].expiry.index,7);assert_eq!(second.next_after,None);
    assert!(store.query_scene(1,&query(3,2)).unwrap().unwrap().expiries.is_empty());
    assert!(store.query_scene(2,&query(0,2)).unwrap().is_none());
    for (after,limit) in [(0,0),(0,129),(u64::MAX,1)] {assert!(store.query_scene(1,&query(after,limit)).is_err());}
}

#[test]
fn feedback_inference_requires_written_owned_matching_fresh_target_and_claims_once() {
    use homesense_storage::feedback_inference::Inference;
    use homesense_core::Observation;
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    let audit=homesense_storage::rule_audit::Admission{revision:1,correlation:7,event:1,parent:0,rule:"synthetic".into()};
    let mut cmd=command(1); cmd.origin=Origin::Rule;
    store.accept_queue_batch_with_audits("boot",&[cmd.clone()],&[],10,None,&[],Some(&audit),None).unwrap();
    let i=Inference{sequence:1,epochs:cmd.epochs,correlation:7,parent_event:1,command_id:1,device:cmd.device_id.clone(),observed:Observation::Switch(true),observed_ms:20,window_ms:10};
    assert!(store.written_feedback_target("boot",1).unwrap().is_none());
    assert!(store.record_feedback_inference("boot",&i).is_err(),"accepted is not written");
    assert!(store.begin_command_write("boot",1,11).unwrap());
    assert!(store.record_feedback_inference("boot",&i).is_err(),"writing is not written");
    store.transition_command("boot",1,Status::Writing,Status::Written,12).unwrap();
    let written=store.written_feedback_target("boot",1).unwrap().unwrap();
    assert_eq!(written.candidate.correlation,7);
    assert_eq!(written.candidate.parent_event,1);
    assert_eq!(written.candidate.written_ms,12);
    assert_eq!(written.target,Command::Switch(true));
    assert_eq!(written.epochs,cmd.epochs);
    assert_eq!(written.device,cmd.device_id);
    assert!(store.written_feedback_target("other",1).unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    for variant in 0..12 {
        let mut bad=i.clone();
        match variant {
            0=>bad.epochs.transport=2,
            1=>bad.epochs.control=2,
            2=>bad.correlation=8,
            3=>bad.parent_event=2,
            4=>bad.observed=Observation::Switch(false),
            5=>bad.device=DeviceId::new("other").unwrap(),
            6=>bad.observed_ms=11,
            7=>bad.observed_ms=22,
            8=>bad.observed=Observation::Button{pressed:true},
            9=>bad.sequence=0,
            10=>bad.window_ms=60_001,
            _=>bad.command_id=u64::MAX,
        }
        assert!(store.record_feedback_inference("boot",&bad).is_err(),"variant {variant}");
    }
    assert!(store.record_feedback_inference("foreign",&i).is_err());
    assert_eq!(sql.query_row("SELECT count(*) FROM feedback_inference",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    sql.execute_batch("CREATE TRIGGER reject_inference BEFORE INSERT ON feedback_inference BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(store.record_feedback_inference("boot",&i).is_err());
    assert_eq!(sql.query_row("SELECT count(*) FROM feedback_inference",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    sql.execute_batch("DROP TRIGGER reject_inference;").unwrap();
    store.record_feedback_inference("boot",&i).unwrap();
    assert!(store.record_feedback_inference("boot",&i).is_err(),"duplicate must not permit another continuation");
    assert_eq!(store.command_status("boot",1).unwrap(),Some(Status::Written));
    assert_eq!(sql.query_row("SELECT attribution,written_ms,observed_ms FROM feedback_inference",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(),("inferred".into(),12,20));
    assert!(sql.execute("UPDATE feedback_inference SET attribution='inferred'",[]).is_err());
    store.finish_rule_chain("boot",1,7,21,None).unwrap();
    assert!(store.written_feedback_target("boot",1).unwrap().is_some(),"terminal parent remains visible for suppression");
    let mut later=i.clone(); later.sequence=2;
    assert!(store.record_feedback_inference("boot",&later).is_err(),"terminal chain cannot resume");
    drop(store);
    let mut reopened=Store::open(&db.0).unwrap();
    assert!(reopened.record_feedback_inference("boot",&i).is_err());
    assert_eq!(sql.query_row("SELECT count(*) FROM feedback_inference",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    reopened.publish(Some(1),2,b"new",&[]).unwrap();
    assert!(matches!(reopened.record_feedback_inference("boot",&later),Err(homesense_storage::Error::Conflict)));
}

#[test]
fn feedback_inference_migrates_and_resolves_delayed_scene_parent() {
    use homesense_storage::{feedback_inference::Inference,scene_run::{Start,StepLink}};
    use homesense_core::{Observation,Percent,LevelTransition};
    let db=Database::new();
    let mut store=Store::open(&db.0).unwrap();
    store.publish(None,1,b"config",&[]).unwrap();
    drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("DROP TABLE light_setting; DROP TABLE feedback_inference; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=17;").unwrap();
    let mut store=Store::open(&db.0).unwrap();
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
    let audit=homesense_storage::rule_audit::Admission{revision:1,correlation:9,event:1,parent:0,rule:"synthetic".into()};
    let start=Start{run:"later".into(),revision:1,definition:"scene".into(),source:"synthetic:scene".into(),action_index:0};
    store.accept_queue_batch_with_scene_starts("boot",&[],&[],1,None,&[],Some(&audit),None,&[start]).unwrap();
    let mut cmd=command(1);cmd.origin=Origin::Rule;
    cmd.command=Command::SetLevel{level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate};
    let link=StepLink{sample:None,run:"later".into(),index:0,source:"synthetic:step".into()};
    store.accept_queue_batch_with_audits("boot",&[cmd.clone()],&[],2,None,&[],None,Some(&link)).unwrap();
    store.begin_command_write("boot",1,3).unwrap();
    store.transition_command("boot",1,Status::Writing,Status::Written,4).unwrap();
    let written=store.written_feedback_target("boot",1).unwrap().unwrap();
    assert_eq!(written.candidate.correlation,9);
    assert_eq!(written.candidate.parent_event,1);
    assert_eq!(written.target,cmd.command);
    let mut i=Inference{sequence:1,epochs:cmd.epochs,correlation:9,parent_event:1,command_id:1,device:cmd.device_id,observed:Observation::Level(Percent::new(36).unwrap()),observed_ms:5,window_ms:10};
    assert!(store.record_feedback_inference("boot",&i).is_err(),"ramp intermediate cannot match final target");
    i.observed=Observation::Level(Percent::new(40).unwrap());
    store.record_feedback_inference("boot",&i).unwrap();
    assert_eq!(sql.query_row("SELECT parent_event FROM feedback_inference",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(store.command_status("boot",1).unwrap(),Some(Status::Written));
}

#[test]
fn schema_eighteen_preserves_history_and_records_system_cancellation_causes() {
    use homesense_storage::scene_run::{Cancellation,CancellationReason as Reason,Query,Status as Scene};
    let db=Database::new();let mut store=Store::open(&db.0).unwrap();store.publish(None,1,b"config",&[]).unwrap();store.begin_scene_run("boot","scene",1,None,0).unwrap();
    let device=DeviceId::new("synthetic:light").unwrap();
    store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Running,1,Some(&Cancellation{device:device.clone(),reason:Reason::CancelRequested})).unwrap();drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("ALTER TABLE scene_cancellation RENAME TO newer; CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL,run_id TEXT NOT NULL,version INTEGER NOT NULL CHECK(version>0),device_id TEXT NOT NULL,reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement','cancel_requested')),PRIMARY KEY(boot_id,run_id,version),FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id)); INSERT INTO scene_cancellation SELECT * FROM newer; DROP TABLE newer; DROP TABLE light_setting; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=18;").unwrap();
    let mut store=Store::open(&db.0).unwrap();let reasons=[Reason::ServiceStopped,Reason::ConfigurationPublished,Reason::TransportFailure,Reason::PreparationFailed];
    for (i,reason) in reasons.iter().enumerate(){store.checkpoint_scene_run_with_cancellation("boot","scene",i as u64+1,Scene::Running,i as u64+2,Some(&Cancellation{device:device.clone(),reason:*reason})).unwrap();}
    drop(store);let store=Store::open(&db.0).unwrap();let page=store.query_scene(1,&Query::Cancellations{boot:"boot".into(),run:"scene".into(),after:0,limit:128}).unwrap().unwrap();
    assert_eq!(page.cancellations.iter().map(|c|c.cancellation.reason).collect::<Vec<_>>(),[vec![Reason::CancelRequested],reasons.to_vec()].concat());assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}

#[test]
fn schema_nineteen_retains_legacy_publication_reason_without_claiming_barrier_commit() {
    use homesense_storage::scene_run::{Cancellation,CancellationReason as Reason,Query,Status as Scene};
    let db=Database::new();let mut store=Store::open(&db.0).unwrap();store.publish(None,1,b"config",&[]).unwrap();store.begin_scene_run("boot","scene",1,None,0).unwrap();
    let device=DeviceId::new("synthetic:light").unwrap();store.checkpoint_scene_run_with_cancellation("boot","scene",0,Scene::Running,1,Some(&Cancellation{device:device.clone(),reason:Reason::ConfigurationPublished})).unwrap();drop(store);
    let sql=rusqlite::Connection::open(&db.0).unwrap();let schema:String=sql.query_row("SELECT sql FROM sqlite_master WHERE name='scene_cancellation'",[],|row|row.get(0)).unwrap();
    sql.execute_batch("ALTER TABLE scene_cancellation RENAME TO newer").unwrap();sql.execute_batch(&schema.replace(",'publication_barrier'","")).unwrap();pre_gateway_scope_fixture::strip(&sql);
    sql.execute_batch("INSERT INTO scene_cancellation SELECT * FROM newer; DROP TABLE newer; DROP TABLE light_setting; ALTER TABLE rule_command DROP COLUMN source; DROP TABLE module_off_effect; PRAGMA user_version=19;").unwrap();
    let mut store=Store::open(&db.0).unwrap();store.checkpoint_scene_run_with_cancellation("boot","scene",1,Scene::Cancelled,2,Some(&Cancellation{device,reason:Reason::PublicationBarrier})).unwrap();
    let page=store.query_scene(1,&Query::Cancellations{boot:"boot".into(),run:"scene".into(),after:0,limit:128}).unwrap().unwrap();assert_eq!(page.cancellations.iter().map(|c|c.cancellation.reason).collect::<Vec<_>>(),vec![Reason::ConfigurationPublished,Reason::PublicationBarrier]);assert_eq!(sql.pragma_query_value(None,"user_version",|row|row.get::<_,i64>(0)).unwrap(),homesense_storage::SCHEMA_VERSION);
}
