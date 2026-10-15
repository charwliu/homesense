use homesense_core::{
    DeviceId, Percent,
    scene_pattern::{LightPower, MemberPatterns, Observations, matching_member},
};
use std::collections::BTreeMap;

fn device(id: &str) -> DeviceId {
    DeviceId::new(id).unwrap()
}
fn pattern(main: bool, down: bool) -> BTreeMap<DeviceId, LightPower> {
    [(device("main"), main.into()), (device("down"), down.into())].into()
}
fn members() -> MemberPatterns {
    [
        (1, pattern(true, false)),
        (2, pattern(true, true)),
        (3, pattern(false, true)),
    ]
    .into()
}
fn observations(main: Option<u8>, down: Option<u8>) -> Observations {
    [
        (
            device("main"),
            main.map(|p| Percent::new(p).unwrap().into()),
        ),
        (
            device("down"),
            down.map(|p| Percent::new(p).unwrap().into()),
        ),
    ]
    .into()
}

#[test]
fn complete_binary_vectors_ignore_brightness_and_keep_partial_feedback_unknown() {
    let targets = members();
    // All 102 physical possibilities for each lamp: unknown or 0..100 percent.
    for main in std::iter::once(None).chain((0..=100).map(Some)) {
        for down in std::iter::once(None).chain((0..=100).map(Some)) {
            let expected = match (main, down) {
                (None, _) | (_, None) => None,
                (Some(0), Some(0)) => Some(0),
                (Some(_), Some(0)) => Some(1),
                (Some(0), Some(_)) => Some(3),
                (Some(_), Some(_)) => Some(2),
            };
            assert_eq!(
                matching_member(&targets, &observations(main, down)),
                expected,
                "main={main:?}, down={down:?}"
            );
        }
    }
}

#[test]
fn same_on_pattern_is_ambiguous_and_member_order_does_not_break_the_tie() {
    let mut targets = members();
    // A scene at 100% and one at 30% have the same target power pattern.
    targets.insert(4, pattern(true, false));
    for level in [1, 17, 30, 50, 100] {
        assert_eq!(
            matching_member(&targets, &observations(Some(level), Some(0))),
            None
        );
    }
    assert_eq!(
        matching_member(&targets, &observations(Some(30), Some(17))),
        Some(2)
    );
    assert_eq!(
        matching_member(&targets, &observations(Some(0), Some(0))),
        Some(0)
    );
}

#[test]
fn incomplete_or_inconsistent_group_vectors_never_invent_off() {
    let actual = observations(Some(0), Some(0));
    assert_eq!(matching_member(&BTreeMap::new(), &actual), None);
    assert_eq!(
        matching_member(&[(1, BTreeMap::new())].into(), &BTreeMap::new()),
        None
    );
    let mut targets = members();
    targets.get_mut(&2).unwrap().remove(&device("down"));
    assert_eq!(matching_member(&targets, &actual), None);
    let mut missing = actual.clone();
    missing.remove(&device("down"));
    assert_eq!(matching_member(&members(), &missing), None);
    let mut extra = actual;
    extra.insert(device("unrelated"), Some(LightPower::Off));
    assert_eq!(matching_member(&members(), &extra), None);
    for invalid in [0, -1] {
        let mut targets = members();
        targets.insert(invalid, pattern(true, false));
        assert_eq!(
            matching_member(&targets, &observations(Some(0), Some(0))),
            None
        );
    }
}

#[test]
fn full_off_and_unmatched_complete_vector_have_no_selected_scene() {
    let targets: MemberPatterns = [(1, pattern(true, false)), (2, pattern(false, false))].into();
    assert_eq!(
        matching_member(&targets, &observations(Some(0), Some(0))),
        Some(0)
    );
    assert_eq!(
        matching_member(&targets, &observations(Some(0), Some(17))),
        Some(0)
    );
    assert_eq!(
        matching_member(&targets, &observations(Some(30), Some(17))),
        Some(0)
    );
}

#[test]
fn switch_and_dimmer_power_conversion_have_the_same_binary_meaning() {
    assert_eq!(
        LightPower::from(false),
        LightPower::from(Percent::new(0).unwrap())
    );
    for level in 1..=100 {
        assert_eq!(
            LightPower::from(true),
            LightPower::from(Percent::new(level).unwrap())
        );
    }
}
