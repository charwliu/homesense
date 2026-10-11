use homesense_core::{Command, DeviceId, queue::Epochs, scene::*};
const E: Epochs = Epochs {
    transport: 1,
    configuration: 1,
    control: 1,
};
fn step(id: &str, delay: u64) -> Step {
    Step {
        device: DeviceId::new(id).unwrap(),
        command: Command::Switch(true),
        delay_ms: delay,
        source: format!("source/{id}/{delay}"),
    }
}
#[test]
fn source_ties_and_absolute_deadlines_do_not_drift() {
    let mut t = Timeline::new(
        vec![step("late", 100), step("a", 0), step("b", 0)],
        E,
        1000,
        200,
    )
    .unwrap();
    assert_eq!(t.status(), Status::Pending);
    for i in [1, 2] {
        let d = t.next(1000, E).unwrap().unwrap();
        assert_eq!(d.index, i);
        t.resolve(i, true).unwrap();
    }
    assert!(t.next(1099, E).unwrap().is_none());
    assert_eq!(t.next(1100, E).unwrap().unwrap().index, 0);
    t.resolve(0, true).unwrap();
    assert_eq!(t.status(), Status::CommandsWritten);
}
#[test]
fn manual_override_only_cancels_selected_device_and_reports_partial() {
    let mut t = Timeline::new(
        vec![step("a", 0), step("a", 100), step("b", 100)],
        E,
        0,
        100,
    )
    .unwrap();
    let d = t.next(0, E).unwrap().unwrap();
    t.resolve(d.index, true).unwrap();
    assert_eq!(t.cancel_device(&DeviceId::new("a").unwrap()), 1);
    assert_eq!(t.next(100, E).unwrap().unwrap().index, 2);
    t.resolve(2, true).unwrap();
    assert_eq!(t.status(), Status::Partial);
}
#[test]
fn cancel_does_not_undo_offered_write_and_late_actions_are_not_replayed() {
    let mut t = Timeline::new(vec![step("a", 0), step("b", 100)], E, 0, 10).unwrap();
    t.next(0, E).unwrap();
    assert_eq!(t.cancel(), 1);
    assert_eq!(t.next(0, E).err(), Some(Error::InFlight));
    t.resolve(0, false).unwrap();
    assert_eq!(t.status(), Status::Failed);
    let mut t = Timeline::new(vec![step("a", 0), step("b", 100)], E, 0, 10).unwrap();
    assert!(t.next(111, E).unwrap().is_none());
    assert_eq!(t.states(), &[State::Expired, State::Expired]);
    assert_eq!(t.status(), Status::Failed);
}
#[test]
fn configuration_change_cancels_future_work_and_clock_reversal_is_rejected() {
    let mut t = Timeline::new(vec![step("a", 100)], E, 10, 100).unwrap();
    assert_eq!(t.next(9, E).err(), Some(Error::Clock));
    assert_eq!(
        t.next(
            20,
            Epochs {
                configuration: 2,
                ..E
            }
        )
        .err(),
        Some(Error::WrongGeneration)
    );
    assert_eq!(t.status(), Status::Cancelled);
    assert!(t.next(110, E).unwrap().is_none());
    assert!(Timeline::new(vec![step("a", 1)], E, u64::MAX, 1).is_err());
}
