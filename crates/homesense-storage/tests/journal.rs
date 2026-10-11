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
            "DROP TABLE scene_cause; DROP TABLE automation_revision; DROP TABLE command_journal; DROP TABLE light_restore; DROP TABLE study_mode; DROP TABLE logical_variable; DROP TABLE calendar_watermark; DROP TABLE rule_command; DROP TABLE rule_admission; DROP TABLE rule_chain_outcome; DROP TABLE scene_command; DROP TABLE scene_step; DROP TABLE scene_run; PRAGMA user_version=1;",
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
            .all(|r| r.step == 0 && r.source == "combined:light")
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
    sql.execute_batch("DROP TABLE scene_cause; PRAGMA user_version=12;").unwrap();
    drop(sql);
    let store=Store::open(&db.0).unwrap();
    assert_eq!(store.scene_run("boot","manual").unwrap().unwrap().started_ms,10);
    assert!(store.scene_cause("boot","manual").unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).unwrap(),13);
}
