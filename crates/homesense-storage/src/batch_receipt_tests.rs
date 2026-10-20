use super::*;
use homesense_core::{DeviceId, Percent, queue::Epochs};

fn pending() -> Store {
    let mut store = Store::open(":memory:").unwrap();
    store
        .publish(None, 1, b"synthetic batch receipt", &[])
        .unwrap();
    let commands = [10, 80, 30]
        .into_iter()
        .enumerate()
        .map(|(n, level)| QueuedCommand {
            id: n as u64 + 1,
            device_id: DeviceId::new("dimmer").unwrap(),
            command: Command::SetLevel {
                level: Percent::new(level).unwrap(),
                transition: LevelTransition::Immediate,
            },
            origin: Origin::Http,
            epochs: Epochs {
                configuration: 1,
                transport: 1,
                control: 1,
            },
            deadline_ms: 1000,
            run_id: None,
        })
        .collect::<Vec<_>>();
    store.accept_commands("boot", &commands, 0).unwrap();
    store
        .transition_commands("boot", &[1, 2], Status::Accepted, Status::Writing, 1)
        .unwrap();
    store
}

#[test]
fn batch_receipt_later_conflict_rolls_back_status_and_light_setting() {
    let mut store = pending();
    // The third row has not entered Writing. The earlier completed row must
    // not persist if a later receipt member is inconsistent.
    assert!(matches!(
        store.transition_commands("boot", &[1, 3], Status::Writing, Status::Written, 2),
        Err(Error::Conflict)
    ));
    assert_eq!(
        store.command_status("boot", 1).unwrap(),
        Some(Status::Writing)
    );
    assert_eq!(
        store.command_status("boot", 3).unwrap(),
        Some(Status::Accepted)
    );
    assert!(
        store
            .recorded_dimmer_levels(1, &[DeviceId::new("dimmer").unwrap()])
            .unwrap()
            .is_empty()
    );
    store
        .transition_commands("boot", &[1, 2], Status::Writing, Status::Written, 2)
        .unwrap();
    for id in [1, 2] {
        assert_eq!(
            store.command_status("boot", id).unwrap(),
            Some(Status::Written)
        );
    }
    assert_eq!(
        store
            .recorded_dimmer_levels(1, &[DeviceId::new("dimmer").unwrap()])
            .unwrap()[&DeviceId::new("dimmer").unwrap()]
            .get(),
        80
    );
    assert_eq!(
        store.command_status("boot", 3).unwrap(),
        Some(Status::Accepted)
    );
}

#[test]
fn batch_receipt_bounds_boot_and_monotonic_time_never_change_a_prefix() {
    let mut store = pending();
    for ids in [vec![], vec![1, 1], (1..=129).collect()] {
        assert!(matches!(
            store.transition_commands("boot", &ids, Status::Writing, Status::Written, 2),
            Err(Error::InvalidAudit)
        ));
    }
    for (boot, now) in [("different-boot", 2), ("boot", 0)] {
        assert!(matches!(
            store.transition_commands(boot, &[1, 2], Status::Writing, Status::Written, now),
            Err(Error::Conflict)
        ));
    }
    for id in [1, 2] {
        assert_eq!(
            store.command_status("boot", id).unwrap(),
            Some(Status::Writing)
        );
    }
    assert!(
        store
            .recorded_dimmer_levels(1, &[DeviceId::new("dimmer").unwrap()])
            .unwrap()
            .is_empty()
    );
}
