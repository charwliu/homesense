use homesense_core::{
    Command, DeviceId, MotorMotion,
    admission::{Access, Rejection, RuntimePolicy},
    arbiter::{Arbiter, ArbiterError, CommitResult},
    queue::{Epochs, Origin, QueuedCommand},
};
fn access(_: &QueuedCommand) -> Access {
    Access {
        authorized: true,
        device_exists: true,
        device_verified: true,
        existing_direct_manual: true,
    }
}
fn fixture() -> Arbiter {
    Arbiter::new(
        RuntimePolicy {
            epochs: Epochs {
                transport: 1,
                configuration: 4,
                control: 7,
            },
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        8,
        2,
    )
    .unwrap()
}
fn request(a: &Arbiter, id: u64, stop: bool) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: DeviceId::new(if stop { "motor" } else { "light" }).unwrap(),
        command: if stop {
            Command::Motor(MotorMotion::Stop)
        } else {
            Command::Switch(true)
        },
        origin: Origin::Hap,
        epochs: a.policy().epochs,
        deadline_ms: 100,
        run_id: None,
    }
}
fn enqueue(a: &mut Arbiter, id: u64, stop: bool) {
    let r = request(a, id, stop);
    a.submit(r.clone(), access(&r), 0).unwrap();
}
#[test]
fn publication_waits_for_frame_and_drains_stop_with_original_mapping_revision() {
    let mut a = fixture();
    enqueue(&mut a, 1, false);
    assert_eq!(a.dispatch(0, access).unwrap().command.unwrap().id, 1);
    enqueue(&mut a, 2, false);
    enqueue(&mut a, 3, true);
    let cancelled = a.begin_publication(1, 50).unwrap();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].command.id, 2);
    let new_stop = request(&a, 4, true);
    assert_eq!(
        a.submit(new_stop.clone(), access(&new_stop), 2)
            .unwrap_err(),
        ArbiterError::Admission(Rejection::ConfigurationChanged)
    );
    assert_eq!(a.drain_stop(2, access).unwrap_err(), ArbiterError::Busy);
    assert_eq!(a.begin_commit(2), Err(ArbiterError::Busy));
    a.finish_write(1, true).unwrap();
    let stop = a
        .drain_stop(3, |original| {
            assert_eq!(original.epochs.control, 7);
            access(original)
        })
        .unwrap()
        .unwrap();
    assert_eq!(stop.epochs.configuration, 4);
    assert_eq!(stop.epochs.control, 8);
    assert_eq!(a.begin_commit(3), Err(ArbiterError::Busy));
    a.finish_write(stop.id, true).unwrap();
    a.begin_commit(4).unwrap();
    a.record_commit(CommitResult::Committed(5)).unwrap();
    assert!(!a.policy().gateway_ready);
    assert!(a.policy().publishing);
    assert_eq!(a.synchronized(4), Err(ArbiterError::NotSynchronized));
    a.synchronized(5).unwrap();
    assert!(a.policy().gateway_ready);
    assert!(a.dispatch(5, access).unwrap().command.is_none());
}
#[test]
fn rollback_does_not_restore_cancelled_work_or_old_producers() {
    let mut a = fixture();
    let late = request(&a, 2, false);
    enqueue(&mut a, 1, false);
    a.begin_publication(1, 50).unwrap();
    a.begin_commit(2).unwrap();
    a.record_commit(CommitResult::RolledBack).unwrap();
    assert_eq!(a.policy().epochs.configuration, 4);
    assert_eq!(a.policy().epochs.control, 8);
    assert_eq!(
        a.submit(late.clone(), access(&late), 3).unwrap_err(),
        ArbiterError::Admission(Rejection::ConfigurationChanged)
    );
    assert!(a.dispatch(3, access).unwrap().command.is_none());
    enqueue(&mut a, 3, false);
    assert!(a.dispatch(4, access).unwrap().command.is_some());
}
#[test]
fn failed_stop_or_expired_barrier_cannot_commit() {
    let mut a = fixture();
    enqueue(&mut a, 1, true);
    a.begin_publication(1, 50).unwrap();
    let stop = a.drain_stop(2, access).unwrap().unwrap();
    a.finish_write(stop.id, false).unwrap();
    assert_eq!(a.begin_commit(3), Err(ArbiterError::DrainFailed));
    assert_eq!(
        a.record_commit(CommitResult::Committed(5)),
        Err(ArbiterError::InvalidState)
    );
    a.abort_before_commit().unwrap();
    assert!(!a.policy().gateway_ready);
    let mut a = fixture();
    enqueue(&mut a, 1, true);
    a.begin_publication(1, 2).unwrap();
    assert_eq!(a.drain_stop(2, access).unwrap_err(), ArbiterError::Expired);
    let cancelled = a.abort_before_commit().unwrap();
    assert_eq!(cancelled.len(), 1);
}
#[test]
fn unknown_commit_holds_barrier_until_durable_pointer_resolution() {
    let mut a = fixture();
    a.begin_publication(0, 50).unwrap();
    a.begin_commit(1).unwrap();
    a.record_commit(CommitResult::Unknown).unwrap();
    assert!(a.policy().publishing);
    assert!(!a.policy().gateway_ready);
    assert_eq!(
        a.abort_before_commit().unwrap_err(),
        ArbiterError::InvalidState
    );
    a.record_commit(CommitResult::Committed(5)).unwrap();
    a.synchronized(5).unwrap();
}
#[test]
fn revocation_is_checked_again_at_dispatch_and_stop_drain() {
    let mut a = fixture();
    enqueue(&mut a, 1, false);
    let d = a
        .dispatch(1, |r| Access {
            authorized: false,
            ..access(r)
        })
        .unwrap();
    assert!(d.command.is_none());
    assert_eq!(d.rejected.len(), 1);
    enqueue(&mut a, 2, true);
    a.begin_publication(2, 50).unwrap();
    assert_eq!(
        a.drain_stop(3, |r| Access {
            authorized: false,
            ..access(r)
        })
        .unwrap_err(),
        ArbiterError::Admission(Rejection::Unauthorized)
    );
    assert_eq!(a.begin_commit(4), Err(ArbiterError::DrainFailed));
    assert_eq!(a.abort_before_commit().unwrap().len(), 1);
}

#[test]
fn storage_fault_invalidates_old_commands_and_default_rejects_manual_degradation() {
    let mut a = fixture();
    enqueue(&mut a, 1, false);
    let writing = a.dispatch(0, access).unwrap().command.unwrap();
    enqueue(&mut a, 2, false);
    enqueue(&mut a, 3, true);
    let late = request(&a, 4, false);
    let cancelled = a.storage_failed().unwrap();
    assert_eq!(cancelled.len(), 2);
    assert_eq!(a.policy().epochs.control, 8);
    assert!(!a.policy().storage_healthy);
    assert!(a.storage_failed().unwrap().is_empty());
    assert_eq!(a.policy().epochs.control, 8);
    assert_eq!(
        a.submit(late.clone(), access(&late), 1).unwrap_err(),
        ArbiterError::Admission(Rejection::ConfigurationChanged)
    );
    let fresh = request(&a, 5, false);
    assert_eq!(
        a.submit(fresh.clone(), access(&fresh), 1).unwrap_err(),
        ArbiterError::Admission(Rejection::PersistenceUnavailable)
    );
    assert_eq!(a.storage_recovered(4).unwrap_err(), ArbiterError::Busy);
    a.finish_write(writing.id, true).unwrap();
    assert!(a.storage_recovered(4).unwrap().is_empty());
    assert_eq!(a.policy().epochs.control, 9);
    assert!(a.policy().storage_healthy);
    assert!(a.dispatch(2, access).unwrap().command.is_none());
}

#[test]
fn opted_in_degradation_revalidates_stop_and_recovery_cancels_other_pending_actions() {
    let mut policy = fixture().policy();
    policy.manual_degraded_enabled = true;
    let mut a = Arbiter::new(policy, 8, 2).unwrap();
    enqueue(&mut a, 1, true);
    let cancelled = a.storage_failed().unwrap();
    assert_eq!(cancelled.len(), 1);
    // Explicitly reissue with a new ID and current epoch, preserving original mapping.
    enqueue(&mut a, 2, true);
    enqueue(&mut a, 3, false);
    let mut http = request(&a, 4, false);
    http.origin = Origin::Http;
    assert_eq!(
        a.submit(http.clone(), access(&http), 0).unwrap_err(),
        ArbiterError::Admission(Rejection::PersistenceUnavailable)
    );
    assert_eq!(a.storage_recovered(4).unwrap_err(), ArbiterError::Busy);
    let stop = a.dispatch(1, access).unwrap().command.unwrap();
    assert!(stop.is_stop());
    a.finish_write(stop.id, true).unwrap();
    let cancelled = a.storage_recovered(4).unwrap();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].command.id, 3);
    assert!(a.dispatch(2, access).unwrap().command.is_none());
}

#[test]
fn revision_mismatch_stops_degradation_and_cannot_be_cleared_by_retry() {
    let mut policy = fixture().policy();
    policy.manual_degraded_enabled = true;
    let mut a = Arbiter::new(policy, 8, 2).unwrap();
    a.storage_failed().unwrap();
    assert_eq!(
        a.storage_recovered(5).unwrap_err(),
        ArbiterError::RevisionMismatch
    );
    assert!(!a.policy().valid_configuration_loaded);
    assert_eq!(
        a.storage_recovered(4).unwrap_err(),
        ArbiterError::NotSynchronized
    );
    let r = request(&a, 1, true);
    assert_eq!(
        a.submit(r.clone(), access(&r), 0).unwrap_err(),
        ArbiterError::Admission(Rejection::ConfigurationChanged)
    );
}

#[test]
fn storage_fault_during_publication_blocks_commit_and_unknown_outcomes_stay_closed() {
    let mut a = fixture();
    enqueue(&mut a, 1, true);
    a.begin_publication(0, 50).unwrap();
    assert_eq!(a.storage_failed().unwrap().len(), 1);
    assert_eq!(
        a.begin_commit(1),
        Err(ArbiterError::Admission(Rejection::PersistenceUnavailable))
    );
    a.abort_before_commit().unwrap();
    a.storage_recovered(4).unwrap();
    a.begin_publication(2, 50).unwrap();
    a.begin_commit(3).unwrap();
    a.storage_failed().unwrap();
    a.record_commit(CommitResult::Unknown).unwrap();
    assert_eq!(
        a.storage_recovered(4).unwrap_err(),
        ArbiterError::InvalidState
    );
    a.record_commit(CommitResult::Committed(5)).unwrap();
    assert_eq!(
        a.synchronized(5),
        Err(ArbiterError::Admission(Rejection::PersistenceUnavailable))
    );
    a.storage_recovered(5).unwrap();
    assert!(a.policy().publishing);
    a.synchronized(5).unwrap();
    assert!(a.policy().gateway_ready);
}

#[test]
fn failed_batch_does_not_partially_enqueue_or_consume_ids() {
    let mut a = Arbiter::new(fixture().policy(), 1, 1).unwrap();
    let first = request(&a, 1, false);
    let second = request(&a, 2, false);
    assert!(
        a.submit_batch(
            vec![
                (first.clone(), access(&first)),
                (second.clone(), access(&second))
            ],
            0
        )
        .is_err()
    );
    assert_eq!(a.queued_len(), 0);
    a.submit(first.clone(), access(&first), 0).unwrap();
    assert_eq!(a.dispatch(0, access).unwrap().command.unwrap().id, 1);
}
#[test]
fn failed_batch_does_not_cancel_preexisting_motor_command_with_stop() {
    let mut a = fixture();
    let mut up = request(&a, 1, true);
    up.command = Command::Motor(MotorMotion::Up);
    a.submit(up.clone(), access(&up), 0).unwrap();
    let stop = request(&a, 2, true);
    let later = request(&a, 3, false);
    let denied = Access {
        authorized: false,
        ..access(&later)
    };
    assert!(
        a.submit_batch(vec![(stop.clone(), access(&stop)), (later, denied)], 0)
            .is_err()
    );
    assert_eq!(
        a.dispatch(1, access).unwrap().command.unwrap().command,
        Command::Motor(MotorMotion::Up)
    );
}

#[test]
fn new_transport_generation_requires_closed_control_and_fresh_synchronization() {
    let mut a = fixture();
    let old = request(&a, 10, false);
    assert_eq!(a.transport_reconnected(2), Err(ArbiterError::InvalidState));
    enqueue(&mut a, 1, false);
    a.dispatch(0, access).unwrap();
    a.storage_failed().unwrap();
    assert_eq!(a.transport_reconnected(2), Err(ArbiterError::Busy));
    assert_eq!(a.policy().epochs.transport, 1);
    a.finish_write(1, false).unwrap();
    assert_eq!(
        a.transport_reconnected(1),
        Err(ArbiterError::NotSynchronized)
    );
    assert_eq!(
        a.transport_reconnected(0),
        Err(ArbiterError::NotSynchronized)
    );
    assert!(a.transport_reconnected(2).unwrap().is_empty());
    assert!(!a.policy().gateway_ready);
    assert!(!a.policy().storage_healthy);
    assert_eq!(a.storage_recovered(4), Err(ArbiterError::NotSynchronized));
    assert_eq!(
        a.gateway_resynchronized(1, 4),
        Err(ArbiterError::NotSynchronized)
    );
    assert_eq!(
        a.gateway_resynchronized(2, 5),
        Err(ArbiterError::NotSynchronized)
    );
    a.gateway_resynchronized(2, 4).unwrap();
    a.storage_recovered(4).unwrap();
    assert_eq!(
        a.submit(old.clone(), access(&old), 1),
        Err(ArbiterError::Admission(Rejection::ConfigurationChanged))
    );
    assert!(a.dispatch(1, access).unwrap().command.is_none());
    enqueue(&mut a, 2, false);
    assert_eq!(
        a.dispatch(2, access)
            .unwrap()
            .command
            .unwrap()
            .epochs
            .transport,
        2
    );
}

#[test]
fn reconnect_cancels_degraded_stop_instead_of_replaying_on_new_socket() {
    let mut policy = fixture().policy();
    policy.manual_degraded_enabled = true;
    let mut a = Arbiter::new(policy, 8, 2).unwrap();
    a.storage_failed().unwrap();
    enqueue(&mut a, 1, true);
    enqueue(&mut a, 2, false);
    let cancelled = a.transport_reconnected(2).unwrap();
    assert_eq!(cancelled.len(), 2);
    assert!(
        cancelled
            .iter()
            .all(|c| c.reason == homesense_core::queue::CancelReason::TransportChanged)
    );
    assert!(
        cancelled
            .iter()
            .any(|c| c.command.command == Command::Motor(MotorMotion::Stop))
    );
    assert_eq!(a.queued_len(), 0);
    a.gateway_resynchronized(2, 4).unwrap();
    a.storage_recovered(4).unwrap();
    assert!(a.dispatch(1, access).unwrap().command.is_none());
}

#[test]
fn publication_recovery_requires_committed_revision_and_fresh_epoch() {
    let mut a = fixture();
    let stale = request(&a, 99, false);
    assert_eq!(a.publication_recovered(2, 4), Err(ArbiterError::NotSynchronized));
    enqueue(&mut a, 1, false);
    a.begin_publication(1, 50).unwrap();
    a.storage_failed().unwrap();
    assert_eq!(a.publication_recovered(2, 5), Err(ArbiterError::NotSynchronized));

    // A failed pre-commit publication cannot be promoted by recovery.
    let mut a = fixture();
    enqueue(&mut a, 1, false);
    a.begin_publication(1, 50).unwrap();
    a.begin_commit(2).unwrap();
    a.record_commit(CommitResult::Committed(5)).unwrap();
    assert_eq!(a.publication_recovered(2, 5), Err(ArbiterError::NotSynchronized));
    a.storage_failed().unwrap();
    let before = a.policy();
    for (epoch, revision) in [(1, 5), (0, 5), (2, 4), (2, 6)] {
        assert_eq!(a.publication_recovered(epoch, revision), Err(ArbiterError::NotSynchronized));
        assert_eq!(a.policy().epochs, before.epochs);
        assert!(!a.policy().storage_healthy && !a.policy().gateway_ready && a.policy().publishing);
    }
    a.publication_recovered(2, 5).unwrap();
    let policy = a.policy();
    assert_eq!(policy.epochs.transport, 2);
    assert_eq!(policy.epochs.configuration, 5);
    assert!(policy.epochs.control > before.epochs.control);
    assert!(policy.storage_healthy && policy.gateway_ready && !policy.publishing);
    assert!(a.submit(stale.clone(), access(&stale), 3).is_err());
    assert!(a.dispatch(3, access).unwrap().command.is_none());
    assert_eq!(a.publication_recovered(3, 5), Err(ArbiterError::NotSynchronized));
    enqueue(&mut a, 2, false);
    assert_eq!(a.dispatch(4, access).unwrap().command.unwrap().id, 2);
}

#[test]
fn module_off_green_tail_commits_with_effects_and_failure_leaves_old_work_unchanged() {
    let mut a=fixture();enqueue(&mut a,1,false);let epochs=a.policy().epochs;
    let modules=vec![(QueuedCommand{id:2,device_id:DeviceId::new("module-ticket").unwrap(),command:Command::ModuleAllOff,origin:Origin::Http,epochs,deadline_ms:100,run_id:None},access(&request(&a,2,false)))];
    let mut reset=QueuedCommand{id:3,device_id:DeviceId::new("key6").unwrap(),command:Command::PanelLed(false),origin:Origin::Http,epochs,deadline_ms:100,run_id:None};let affected=[DeviceId::new("light").unwrap()].into();
    let result=a.submit_module_off_with_reset_committed(modules.clone(),Some((reset.clone(),access(&reset))),&affected,0,|_,_|Err::<(),_>("audit failed"));assert!(result.is_err());assert_eq!(a.queued_len(),1);
    reset.command=Command::PanelLed(true);assert!(a.submit_module_off_with_reset_committed(modules.clone(),Some((reset.clone(),access(&reset))),&affected,0,|_,_|Ok::<_,()>(())).is_err());assert_eq!(a.queued_len(),1);
    reset.command=Command::PanelLed(false);let cancelled=a.submit_module_off_with_reset_committed(modules,Some((reset.clone(),access(&reset))),&affected,0,|commands,cancelled|{assert_eq!(commands.len(),2);assert_eq!(cancelled.len(),1);Ok::<_,()>(())}).unwrap();assert_eq!(cancelled.len(),1);assert_eq!(a.queued_len(),2);
}
