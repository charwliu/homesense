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

fn level_targets()->homesense_core::scene_pattern::MemberLevels {
    [(1,[(device("main"),Percent::new(100).unwrap()),(device("down"),Percent::new(0).unwrap())].into()),
     (2,[(device("main"),Percent::new(100).unwrap()),(device("down"),Percent::new(100).unwrap())].into()),
     (3,[(device("main"),Percent::new(0).unwrap()),(device("down"),Percent::new(17).unwrap())].into()),
     (4,[(device("main"),Percent::new(30).unwrap()),(device("down"),Percent::new(0).unwrap())].into())].into()
}
#[test]
fn collision_only_brightness_refinement_has_strict_complete_and_unknown_cases() {
    use homesense_core::scene_pattern::{brightness_reads_for_collision,matching_member_with_levels};
    let mut power=members();power.insert(4,pattern(true,false));let target=level_targets();
    for actual in 1..=100 {
        let observed=[(device("main"),Some(Percent::new(actual).unwrap()))].into();
        assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(actual),Some(0)),&observed),match actual {100=>Some(1),30=>Some(4),_=>None});
    }
    assert_eq!(brightness_reads_for_collision(&power,&target,&observations(Some(30),Some(0))),Some([device("main")].into()));
    for unknown in [BTreeMap::new(),[(device("main"),None)].into()] {
        assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(30),Some(0)),&unknown),None);
    }
    assert_eq!(matching_member_with_levels(&power,&target,&observations(None,Some(0)),&BTreeMap::new()),None);
    let wrong=[(device("main"),Some(Percent::new(30).unwrap())),(device("down"),Some(Percent::new(0).unwrap()))].into();
    assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(30),Some(0)),&wrong),None,"out-of-read-set brightness cannot bypass scope");
}
#[test]
fn refinement_keeps_unique_power_brightness_independent_and_all_off_zero() {
    use homesense_core::scene_pattern::matching_member_with_levels;
    let mut power=members();power.insert(4,pattern(true,false));let target=level_targets();
    for level in 1..=100 {
        assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(level),Some(1)),&BTreeMap::new()),Some(2),"unique power must ignore missing and nonmatching level");
        assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(0),Some(level)),&BTreeMap::new()),Some(3));
    }
    power.insert(5,pattern(false,false));power.insert(6,pattern(false,false));
    assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(0),Some(0)),&BTreeMap::new()),Some(0));
    let mut duplicates=target;duplicates.get_mut(&4).unwrap().insert(device("main"),Percent::new(100).unwrap());
    assert_eq!(matching_member_with_levels(&power,&duplicates,&observations(Some(100),Some(0)),&[(device("main"),Some(Percent::new(100).unwrap()))].into()),None);
}
#[test]
fn multiple_discriminating_levels_must_match_one_complete_candidate() {
    use homesense_core::scene_pattern::{MemberLevels,matching_member_with_levels};
    let power=[(1,pattern(true,true)),(2,pattern(true,true)),(3,pattern(true,true))].into();
    let target:MemberLevels=[(1,(26,38)),(2,(71,82)),(3,(26,82))].map(|(value,(a,b))|(value,[(device("main"),Percent::new(a).unwrap()),(device("down"),Percent::new(b).unwrap())].into())).into();
    for (a,b,expected) in [(26,38,Some(1)),(71,82,Some(2)),(26,82,Some(3)),(71,38,None),(30,30,None)] {
        let actual=[(device("main"),Some(Percent::new(a).unwrap())),(device("down"),Some(Percent::new(b).unwrap()))].into();
        assert_eq!(matching_member_with_levels(&power,&target,&observations(Some(a),Some(b)),&actual),expected);
    }
    let mut bad=target.clone();bad.get_mut(&1).unwrap().remove(&device("main"));
    assert_eq!(matching_member_with_levels(&power,&bad,&observations(Some(26),Some(38)),&BTreeMap::new()),None);
}
