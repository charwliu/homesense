use homesense_core::{DeviceId, queue::Epochs, scene_observer::*};
use std::collections::BTreeSet;
fn scope(stream: u64) -> Scope {
    Scope {
        boot_id: 1,
        epochs: Epochs {
            configuration: 2,
            transport: 3,
            control: 4,
        },
        stream_generation: stream,
    }
}
fn id(key: &str) -> DeviceId {
    DeviceId::new(key).unwrap()
}
fn make_observer(stream: u64, cursor: u64) -> Observer {
    Observer::new(
        scope(stream),
        cursor,
        0,
        vec![Group {
            id: "room".into(),
            members: BTreeSet::from([1, 2, 3]),
        }],
        ["key1", "key3", "key5"]
            .into_iter()
            .zip(1..=3)
            .map(|(key, member)| Key {
                device: id(key),
                group: "room".into(),
                member,
            })
            .collect(),
        30,
        5000,
    )
    .unwrap()
}
fn event(sequence: u64, key: &str, down: bool, at: u64) -> Press {
    Press {
        scope: scope(1),
        sequence,
        device: id(key),
        down,
        observed_ms: at,
    }
}
fn selected(value: Option<i64>) -> Selection {
    Selection {
        value,
        version: 12,
        authority: Authority::Recorded,
    }
}
#[test]
fn three_mutually_exclusive_members_toggle_without_command_outputs() {
    let mut observer = make_observer(1, 0);
    let mut current = Some(0);
    for (i, (key, expected, basis)) in [
        ("key1", 1, Basis::SelectMember),
        ("key1", 0, Basis::ToggleOff),
        ("key3", 2, Basis::SelectMember),
        ("key5", 3, Basis::SelectMember),
        ("key5", 0, Basis::ToggleOff),
    ]
    .into_iter()
    .enumerate()
    {
        let now = i as u64 * 100;
        let inferred = observer
            .observe(
                &event(i as u64 * 2 + 1, key, true, now),
                selected(current),
                now,
            )
            .unwrap()
            .unwrap();
        assert_eq!(inferred.value, Some(expected));
        assert_eq!(inferred.basis, basis);
        assert_eq!(inferred.expected_version, 12);
        assert_eq!(inferred.scope, scope(1));
        assert!(
            observer
                .observe(
                    &event(i as u64 * 2 + 2, key, false, now + 1),
                    selected(current),
                    now + 1
                )
                .unwrap()
                .is_none()
        );
        current = inferred.value;
    }
}
#[test]
fn unknown_initial_selection_is_preserved_for_every_key() {
    let mut observer = make_observer(1, 0);
    for (i, key) in ["key1", "key3", "key5"].into_iter().enumerate() {
        let inferred = observer
            .observe(&event(i as u64 + 1, key, true, 0), selected(None), 0)
            .unwrap()
            .unwrap();
        assert_eq!(inferred.value, None);
        assert_eq!(inferred.basis, Basis::UnknownInitial);
        assert_eq!(inferred.device, id(key));
        assert_eq!(inferred.sequence, i as u64 + 1);
    }
}
#[test]
fn held_keys_replay_and_debounce_do_not_toggle_again_and_short_release_is_kept() {
    let mut observer = make_observer(1, 0);
    let first = event(1, "key1", true, 0);
    assert_eq!(
        observer
            .observe(&first, selected(Some(0)), 0)
            .unwrap()
            .unwrap()
            .value,
        Some(1)
    );
    assert!(
        observer
            .observe(&first, selected(Some(1)), 0)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(2, "key1", true, 6000), selected(Some(1)), 6000)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(3, "key1", false, 6001), selected(Some(1)), 6001)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        observer
            .observe(&event(4, "key1", true, 6030), selected(Some(1)), 6030)
            .unwrap()
            .unwrap()
            .value,
        Some(0)
    );
    observer
        .observe(&event(5, "key1", false, 6031), selected(Some(0)), 6031)
        .unwrap();
    assert!(
        observer
            .observe(&event(6, "key1", true, 6032), selected(Some(0)), 6032)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(7, "key1", true, 6100), selected(Some(0)), 6100)
            .unwrap()
            .is_none()
    );
    observer
        .observe(&event(8, "key1", false, 6101), selected(Some(0)), 6101)
        .unwrap();
    assert_eq!(
        observer
            .observe(&event(9, "key1", true, 6102), selected(Some(0)), 6102)
            .unwrap()
            .unwrap()
            .value,
        Some(1)
    );
}
#[test]
fn stale_and_future_frames_are_consumed_and_never_replayed_later() {
    let mut observer = make_observer(1, 0);
    assert!(
        observer
            .observe(&event(1, "key1", true, 0), selected(Some(0)), 5000)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(1, "key1", true, 5000), selected(Some(0)), 5000)
            .unwrap()
            .is_none()
    );
    let future = event(2, "key3", true, 6000);
    assert!(
        observer
            .observe(&future, selected(Some(0)), 5000)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&future, selected(Some(0)), 6000)
            .unwrap()
            .is_none()
    );
    observer
        .observe(&event(3, "key3", false, 6001), selected(Some(0)), 6001)
        .unwrap();
    assert_eq!(
        observer
            .observe(&event(4, "key3", true, 6002), selected(Some(0)), 6002)
            .unwrap()
            .unwrap()
            .value,
        Some(2)
    );
}
#[test]
fn old_scope_cannot_advance_cursor_and_old_inferred_selection_becomes_unknown() {
    let mut observer = make_observer(2, 100);
    assert!(
        observer
            .observe(&event(1000, "key1", true, 100), selected(Some(1)), 100)
            .unwrap()
            .is_none()
    );
    let mut fresh = event(101, "key1", true, 101);
    fresh.scope = scope(2);
    let source = Selection {
        value: Some(1),
        version: 22,
        authority: Authority::Inferred(scope(1)),
    };
    let inferred = observer.observe(&fresh, source, 101).unwrap().unwrap();
    assert_eq!(inferred.value, None);
    assert_eq!(inferred.basis, Basis::UnknownInitial);
    assert_eq!(inferred.expected_version, 22);
    let mut observer = make_observer(2, 100);
    assert_eq!(
        observer
            .observe(
                &fresh,
                Selection {
                    authority: Authority::Inferred(scope(2)),
                    ..source
                },
                101
            )
            .unwrap()
            .unwrap()
            .value,
        Some(0)
    );
}
#[test]
fn invalid_configuration_selection_and_backwards_clock_are_rejected() {
    let groups = vec![Group {
        id: "room".into(),
        members: BTreeSet::from([1]),
    }];
    let key = Key {
        device: id("key1"),
        group: "room".into(),
        member: 1,
    };
    assert!(
        Observer::new(
            scope(1),
            0,
            0,
            groups.clone(),
            vec![key.clone(), key.clone()],
            30,
            5000
        )
        .is_err()
    );
    assert!(
        Observer::new(
            scope(1),
            0,
            0,
            groups,
            vec![Key { member: 2, ..key }],
            30,
            5000
        )
        .is_err()
    );
    let mut observer = make_observer(1, 0);
    assert_eq!(
        observer.observe(&event(1, "key1", true, 100), selected(Some(99)), 100),
        Err(Error::InvalidSelection)
    );
    assert_eq!(
        observer.observe(&event(2, "key1", false, 90), selected(Some(0)), 90),
        Err(Error::ClockWentBackwards)
    );
}

#[test]
fn stream_start_holds_do_not_become_new_presses_until_release() {
    let mut observer = make_observer(1, 0);
    observer.initially_held(&[id("key1")]).unwrap();
    assert!(
        observer
            .observe(&event(1, "key1", true, 50), selected(Some(0)), 50)
            .unwrap()
            .is_none()
    );
    assert!(observer.initially_held(&[id("key3")]).is_err());
    assert!(
        observer
            .observe(&event(2, "key1", false, 51), selected(Some(0)), 51)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        observer
            .observe(&event(3, "key1", true, 52), selected(Some(0)), 52)
            .unwrap()
            .unwrap()
            .value,
        Some(1)
    );
    let mut other = make_observer(1, 0);
    assert!(other.initially_held(&[id("unknown")]).is_err());
    assert!(other.initially_held(&[id("key1"), id("key1")]).is_err());
}

#[test]
fn reviewed_release_keys_wait_through_long_hold_and_use_release_time_and_selection() {
    let mut observer = make_observer(1, 0);
    observer.released_keys(&[id("key1")]).unwrap();
    assert!(
        observer
            .observe(&event(1, "key1", false, 0), selected(Some(0)), 0)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(2, "key1", true, 100), selected(Some(0)), 100)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(3, "key1", true, 8000), selected(Some(0)), 8000)
            .unwrap()
            .is_none()
    );
    let inferred = observer
        .observe(&event(4, "key1", false, 8001), selected(Some(2)), 8001)
        .unwrap()
        .unwrap();
    assert_eq!(inferred.value, Some(1));
    assert_eq!(inferred.observed_ms, 8001);
    assert_eq!(inferred.sequence, 4);
    assert!(
        observer
            .observe(&event(5, "key1", false, 8002), selected(Some(1)), 8002)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(6, "key1", true, 8100), selected(Some(1)), 8100)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        observer
            .observe(&event(7, "key1", false, 8101), selected(Some(1)), 8101)
            .unwrap()
            .unwrap()
            .value,
        Some(0)
    );
    // A different key keeps the legacy press edge.
    assert_eq!(
        observer
            .observe(&event(8, "key3", true, 8200), selected(Some(0)), 8200)
            .unwrap()
            .unwrap()
            .value,
        Some(2)
    );
    assert!(observer.released_keys(&[id("key5")]).is_err());
}
#[test]
fn release_edge_initial_holds_stale_cycles_and_debounce_cannot_create_inferences() {
    let mut observer = make_observer(1, 0);
    observer.released_keys(&[id("key1")]).unwrap();
    observer.initially_held(&[id("key1")]).unwrap();
    assert!(
        observer
            .observe(&event(1, "key1", true, 10), selected(Some(0)), 10)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(2, "key1", false, 11), selected(Some(0)), 11)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(3, "key1", true, 0), selected(Some(0)), 5000)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(4, "key1", false, 5001), selected(Some(0)), 5001)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(5, "key1", true, 5100), selected(Some(0)), 5100)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(6, "key1", false, 5101), selected(Some(0)), 10101)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(7, "key1", true, 10200), selected(Some(0)), 10200)
            .unwrap()
            .is_none()
    );
    let result = observer
        .observe(&event(8, "key1", false, 10201), selected(None), 10201)
        .unwrap()
        .unwrap();
    assert_eq!(result.basis, Basis::UnknownInitial);
    assert!(
        observer
            .observe(&event(9, "key1", true, 10202), selected(Some(0)), 10202)
            .unwrap()
            .is_none()
    );
    assert!(
        observer
            .observe(&event(10, "key1", false, 10203), selected(Some(0)), 10203)
            .unwrap()
            .is_none()
    );
    let mut other = make_observer(1, 0);
    assert!(other.released_keys(&[id("missing")]).is_err());
    assert!(other.released_keys(&[id("key1"), id("key1")]).is_err());
}
