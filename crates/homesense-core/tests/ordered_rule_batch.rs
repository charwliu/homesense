use homesense_core::{
    Command, DeviceId, LevelTransition, Percent,
    admission::{Access, RuntimePolicy},
    arbiter::Arbiter,
    queue::{CancelReason, Epochs, Origin, QueuedCommand},
};
fn actor(capacity: usize) -> Arbiter {
    Arbiter::new(
        RuntimePolicy {
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
            publishing: false,
            controller: true,
            gateway_ready: true,
            storage_healthy: true,
            manual_degraded_enabled: false,
            valid_configuration_loaded: true,
        },
        capacity,
        1,
    )
    .unwrap()
}
fn access() -> Access {
    Access {
        authorized: true,
        device_exists: true,
        device_verified: true,
        existing_direct_manual: false,
    }
}
fn command(id: u64, origin: Origin, run: Option<u64>) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: DeviceId::new("synthetic-light").unwrap(),
        command: Command::SetLevel {
            level: Percent::new(0).unwrap(),
            transition: LevelTransition::Immediate,
        },
        origin,
        epochs: Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        },
        deadline_ms: 1000,
        run_id: run,
    }
}
#[test]
fn ordered_rule_preserves_equal_nodes_while_default_slider_still_coalesces() {
    let mut ordered = actor(2);
    let commands = vec![
        (command(1, Origin::Rule, Some(1)), access()),
        (command(2, Origin::Rule, Some(1)), access()),
    ];
    assert!(
        ordered
            .submit_ordered_rule_batch_committed(commands, 0, |accepted, cancelled| {
                assert_eq!(accepted.len(), 2);
                assert!(cancelled.is_empty());
                Ok::<(), ()>(())
            })
            .unwrap()
            .is_empty()
    );
    for id in [1, 2] {
        assert_eq!(
            ordered
                .dispatch(1, |_| access())
                .unwrap()
                .command
                .unwrap()
                .id,
            id
        );
        ordered.finish_write(id, true).unwrap();
    }
    assert!(ordered.dispatch(2, |_| access()).unwrap().command.is_none());
    for origin in [Origin::Hap, Origin::Http, Origin::Rule] {
        let mut default = actor(1);
        let cmds = vec![
            (command(1, origin, Some(1)), access()),
            (command(2, origin, Some(1)), access()),
        ];
        let cancelled = default
            .submit_batch_committed(cmds, 0, |_, _| Ok::<(), ()>(()))
            .unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].command.id, 1);
        assert_eq!(
            default
                .dispatch(1, |_| access())
                .unwrap()
                .command
                .unwrap()
                .id,
            2
        );
    }
}
#[test]
fn ordered_entry_rejects_non_rule_mixed_run_authority_and_stale_epochs_without_commit() {
    let mut a = actor(4);
    let first = command(1, Origin::Rule, Some(1));
    let variants = [
        command(2, Origin::Hap, Some(1)),
        command(2, Origin::Http, Some(1)),
        command(2, Origin::Rule, None),
        command(2, Origin::Rule, Some(2)),
        QueuedCommand {
            epochs: Epochs {
                transport: 2,
                ..first.epochs
            },
            ..command(2, Origin::Rule, Some(1))
        },
    ];
    for bad in variants {
        assert!(
            a.submit_ordered_rule_batch_committed(
                vec![(first.clone(), access()), (bad, access())],
                0,
                |_, _| -> Result<(), ()> { panic!("rejection must precede persistence") }
            )
            .is_err()
        );
        assert!(a.dispatch(1, |_| access()).unwrap().command.is_none());
    }
    assert!(
        a.submit_ordered_rule_batch_committed(
            vec![
                (first.clone(), access()),
                (
                    command(2, Origin::Rule, Some(1)),
                    Access {
                        authorized: false,
                        ..access()
                    }
                )
            ],
            0,
            |_, _| -> Result<(), ()> { panic!("unauthorized") }
        )
        .is_err()
    );
    assert!(
        a.submit_ordered_rule_batch_committed(
            vec![(command(1, Origin::Rule, Some(0)), access())],
            0,
            |_, _| Ok::<(), ()>(())
        )
        .is_err()
    );
    assert!(a.dispatch(1, |_| access()).unwrap().command.is_none());
    a.submit_ordered_rule_batch_committed(vec![(first, access())], 1, |_, _| Ok::<(), ()>(()))
        .unwrap();
    assert_eq!(a.dispatch(2, |_| access()).unwrap().command.unwrap().id, 1);
}
#[test]
fn capacity_or_persistence_failure_keeps_all_queue_effects_and_ids_unpublished() {
    for failure in ["capacity", "persist"] {
        let mut a = actor(if failure == "capacity" { 1 } else { 2 });
        let batch = vec![
            (command(1, Origin::Rule, Some(1)), access()),
            (command(2, Origin::Rule, Some(1)), access()),
        ];
        assert!(
            a.submit_ordered_rule_batch_committed(batch.clone(), 0, |_, _| Err::<(), _>(
                "injected"
            ))
            .is_err()
        );
        assert!(a.dispatch(1, |_| access()).unwrap().command.is_none());
        a.submit_ordered_rule_batch_committed(vec![batch[0].clone()], 1, |_, _| Ok::<(), ()>(()))
            .unwrap();
        assert_eq!(a.dispatch(2, |_| access()).unwrap().command.unwrap().id, 1);
    }
}
#[test]
fn manual_override_and_explicit_run_cancel_remove_every_pending_repeated_node() {
    for manual in [false, true] {
        let mut a = actor(3);
        a.submit_ordered_rule_batch_committed(
            vec![
                (command(1, Origin::Rule, Some(1)), access()),
                (command(2, Origin::Rule, Some(1)), access()),
            ],
            0,
            |_, _| Ok::<(), ()>(()),
        )
        .unwrap();
        let cancelled = if manual {
            a.submit_batch_committed(
                vec![(command(3, Origin::Hap, None), access())],
                1,
                |_, cancelled| {
                    assert_eq!(cancelled.len(), 2);
                    Ok::<(), ()>(())
                },
            )
            .unwrap()
        } else {
            a.cancel_run_committed(1, |cancelled| {
                assert_eq!(cancelled.len(), 2);
                Ok::<(), ()>(())
            })
            .unwrap()
        };
        assert_eq!(
            cancelled.iter().map(|c| c.command.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(cancelled.iter().all(|c| c.reason
            == if manual {
                CancelReason::Superseded
            } else {
                CancelReason::RunCancelled
            }));
        assert_eq!(
            a.dispatch(2, |_| access()).unwrap().command.map(|c| c.id),
            manual.then_some(3)
        );
    }
}
