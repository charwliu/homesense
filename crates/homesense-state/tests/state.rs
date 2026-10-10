use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
use homesense_state::{Change, Cursor, Quality, StateError, StateStore};
fn id() -> DeviceId {
    DeviceId::new("c4:336").unwrap()
}
fn level(n: u8) -> Percent {
    Percent::new(n).unwrap()
}
fn setup(capacity: usize) -> StateStore {
    let mut store = StateStore::new(42, 1, 10, capacity).unwrap();
    store.register(id(), ModuleKind::Dimmer).unwrap();
    store
}

#[test]
fn command_intent_never_confirms_state_and_old_completions_cannot_clear_new_intent() {
    let mut store = setup(20);
    for (command_id, n) in [(1, 30), (2, 60)] {
        store
            .set_desired(
                &id(),
                command_id,
                Command::SetLevel {
                    level: level(n),
                    transition: LevelTransition::Immediate,
                },
                command_id,
            )
            .unwrap();
    }
    store.finish_pending(&id(), 1, 3).unwrap();
    let state = store.get(&id()).unwrap();
    assert_eq!(state.observed, None);
    assert_eq!(state.quality, Quality::Unknown);
    assert_eq!(state.pending_command, Some(2));
    store
        .observe(&id(), Observation::Level(level(30)), 1, 4)
        .unwrap();
    assert_eq!(store.get(&id()).unwrap().pending_command, Some(2));
    store.finish_pending(&id(), 2, 5).unwrap();
    assert_eq!(store.get(&id()).unwrap().quality, Quality::Confirmed);
    assert_eq!(
        store.get(&id()).unwrap().observed,
        Some(Observation::Level(level(30)))
    );
}

#[test]
fn duplicate_feedback_refreshes_freshness_without_triggering_change_events() {
    let mut store = setup(10);
    store
        .observe(&id(), Observation::Level(level(30)), 1, 10)
        .unwrap();
    let cursor = store.cursor();
    store
        .observe(&id(), Observation::Level(level(30)), 1, 20)
        .unwrap();
    assert!(store.events_since(cursor).unwrap().is_empty());
    assert_eq!(store.get(&id()).unwrap().observed_at_ms, Some(20));
    store.mark_stale(25, 10).unwrap();
    assert_eq!(store.get(&id()).unwrap().quality, Quality::Confirmed);
    store.mark_stale(30, 10).unwrap();
    assert_eq!(store.get(&id()).unwrap().quality, Quality::Stale);
    store
        .observe(&id(), Observation::Level(level(30)), 1, 31)
        .unwrap();
    assert_eq!(store.events_since(cursor).unwrap().len(), 2);
}

#[test]
fn disconnect_preserves_observation_but_rejects_old_connection_feedback() {
    let mut store = setup(10);
    store
        .observe(&id(), Observation::Level(level(80)), 1, 10)
        .unwrap();
    store.disconnect(2, 11).unwrap();
    let before = store.snapshot();
    assert_eq!(before.devices[&id()].quality, Quality::Stale);
    assert_eq!(
        store.observe(&id(), Observation::Level(level(0)), 1, 12),
        Err(StateError::WrongTransportEpoch)
    );
    assert_eq!(store.snapshot(), before);
    store
        .observe(&id(), Observation::Level(level(0)), 2, 12)
        .unwrap();
    assert_eq!(store.get(&id()).unwrap().quality, Quality::Confirmed);
}

#[test]
fn subscription_gaps_restart_and_future_cursors_require_snapshot() {
    let mut store = setup(2);
    let snapshot = store.snapshot();
    store
        .observe(&id(), Observation::Level(level(10)), 1, 1)
        .unwrap();
    store
        .observe(&id(), Observation::Level(level(20)), 1, 2)
        .unwrap();
    assert_eq!(store.events_since(snapshot.cursor).unwrap().len(), 2);
    store
        .observe(&id(), Observation::Level(level(30)), 1, 3)
        .unwrap();
    assert_eq!(
        store.events_since(snapshot.cursor),
        Err(StateError::ResyncRequired)
    );
    for cursor in [
        Cursor {
            boot_id: 1,
            sequence: 4,
        },
        Cursor {
            boot_id: 42,
            sequence: 100,
        },
    ] {
        assert_eq!(store.events_since(cursor), Err(StateError::ResyncRequired));
    }
    assert!(
        store
            .events_since(store.snapshot().cursor)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn invalid_observations_are_atomic_and_motors_never_gain_fake_positions() {
    let mut store = setup(10);
    store
        .observe(&id(), Observation::Level(level(10)), 1, 5)
        .unwrap();
    let before = store.snapshot();
    assert_eq!(
        store.observe(&id(), Observation::Switch(true), 1, 6),
        Err(StateError::WrongKind)
    );
    assert_eq!(
        store.observe(&id(), Observation::Level(level(20)), 1, 4),
        Err(StateError::ClockWentBackwards)
    );
    assert_eq!(store.snapshot(), before);
    let motor = DeviceId::new("c4:290").unwrap();
    store.register(motor.clone(), ModuleKind::Motor).unwrap();
    assert_eq!(
        store.observe(&motor, Observation::Level(level(50)), 1, 7),
        Err(StateError::WrongKind)
    );
    assert_eq!(store.get(&motor).unwrap().observed, None);
}

#[test]
fn identical_button_edges_remain_events_for_the_gesture_layer() {
    let mut store = StateStore::new(1, 0, 1, 8).unwrap();
    store.register(id(), ModuleKind::Keypad).unwrap();
    let cursor = store.cursor();
    for now in [1, 2] {
        store
            .observe(&id(), Observation::Button { pressed: true }, 0, now)
            .unwrap();
    }
    assert_eq!(
        store
            .events_since(cursor)
            .unwrap()
            .iter()
            .filter(|e| e.change == Change::Button)
            .count(),
        2
    );
    assert_eq!(
        store.register(DeviceId::new("extra").unwrap(), ModuleKind::Switch),
        Err(StateError::Capacity)
    );
}
