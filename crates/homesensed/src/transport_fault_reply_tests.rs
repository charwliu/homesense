//! Real arbiter failure and real journal transactions behind the actor's reply.
//! These tests exercise fault-result authority, never grant recovery or replay.
use super::transport_fault_audit_result;
use hap::server::WriteStatus;
use homesense_core::{
    Command, DeviceId, ExecutionContext,
    admission::RuntimePolicy,
    arbiter::{Arbiter, ArbiterError},
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_storage::{Store, journal::Status};
use std::path::PathBuf;

const BOOT: &str = "transport-fault-reply-test";

fn arbiter(control: u64) -> Arbiter {
    Arbiter::new(
        RuntimePolicy {
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control,
            },
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        8,
        1,
    )
    .unwrap()
}

fn journal() -> (PathBuf, Store) {
    let directory =
        std::env::temp_dir().join(format!("homesense-fault-reply-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let mut store = Store::open(directory.join("runtime.sqlite")).unwrap();
    store
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation.json"),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    // An already accepted durable row provides an observable fault audit.
    store
        .accept_commands(
            BOOT,
            &[QueuedCommand {
                id: 1,
                device_id: DeviceId::new("simulation:light").unwrap(),
                command: Command::Switch(true),
                origin: Origin::Hap,
                epochs: Epochs {
                    transport: 1,
                    configuration: 1,
                    control: 1,
                },
                deadline_ms: 1000,
                run_id: None,
            }],
            0,
        )
        .unwrap();
    (directory, store)
}

fn close(directory: PathBuf, store: Store) {
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn arbiter_counter_exhaustion_cannot_issue_recovery_reply_after_successful_journal_audit() {
    let (directory, mut store) = journal();
    let mut owner = arbiter(u64::MAX);
    let memory = owner.storage_failed();
    assert_eq!(memory, Err(ArbiterError::CounterExhausted));
    assert!(!owner.policy().storage_healthy);
    assert!(!owner.policy().valid_configuration_loaded);

    // Do not short-circuit the actual transaction after the memory failure.
    let result = transport_fault_audit_result(memory, store.fail_transport(BOOT, 1));
    assert_eq!(
        store.command_status(BOOT, 1).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        result,
        Err(WriteStatus::ServiceCommunicationFailure),
        "durable success cannot hide the real arbiter invariant failure"
    );
    close(directory, store);
}

#[test]
fn real_journal_audit_failure_cannot_issue_recovery_reply_after_successful_arbiter_fault() {
    let (directory, mut store) = journal();
    let sql = rusqlite::Connection::open(directory.join("runtime.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_fault_audit BEFORE UPDATE ON command_journal BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END;").unwrap();
    let mut owner = arbiter(1);
    let memory = owner.storage_failed();
    assert!(memory.is_ok());
    assert_eq!(owner.policy().epochs.control, 2);
    let result = transport_fault_audit_result(memory, store.fail_transport(BOOT, 1));
    assert_eq!(result, Err(WriteStatus::ServiceCommunicationFailure));
    assert_eq!(
        store.command_status(BOOT, 1).unwrap(),
        Some(Status::Accepted),
        "failed SQLite transaction must not claim cancellation committed"
    );
    drop(sql);
    close(directory, store);
}

#[test]
fn completed_arbiter_fault_and_real_journal_audit_preserve_recoverable_reply() {
    let (directory, mut store) = journal();
    let mut owner = arbiter(1);
    let memory = owner.storage_failed();
    assert!(memory.is_ok());
    assert!(!owner.policy().storage_healthy);
    assert_eq!(owner.policy().epochs.control, 2);
    let result = transport_fault_audit_result(memory, store.fail_transport(BOOT, 1));
    assert_eq!(result, Ok(()));
    assert_eq!(
        store.command_status(BOOT, 1).unwrap(),
        Some(Status::Cancelled)
    );
    close(directory, store);
}
