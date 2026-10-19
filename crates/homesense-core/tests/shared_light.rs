use homesense_core::shared_light::{self, Light, Observed, Output};
use homesense_core::{Command, LevelTransition, Percent};

fn percent(value: u8) -> Percent {
    Percent::new(value).unwrap()
}

fn state(
    common: bool,
    left: bool,
    right: bool,
    primary: u8,
    secondary: u8,
    reading: u8,
) -> Observed {
    Observed {
        common_supply: common,
        left_supply: left,
        right_supply: right,
        primary_level: percent(primary),
        secondary_level: percent(secondary),
        reading_level: percent(reading),
    }
}

// An independent physical matrix oracle: supply rows multiply dimmer columns.
// It deliberately does not call the planner's logical projection.
fn fixtures(s: Observed) -> [[u8; 3]; 3] {
    [s.common_supply, s.left_supply, s.right_supply].map(|on| {
        [s.primary_level, s.secondary_level, s.reading_level]
            .map(|value| if on { value.get() } else { 0 })
    })
}

fn apply(s: &mut Observed, output: Output, command: Command) {
    match (output, command) {
        (Output::CommonSupply, Command::Switch(on)) => s.common_supply = on,
        (Output::LeftSupply, Command::Switch(on)) => s.left_supply = on,
        (Output::RightSupply, Command::Switch(on)) => s.right_supply = on,
        (
            Output::PrimaryLevel,
            Command::SetLevel {
                level,
                transition: LevelTransition::Immediate,
            },
        ) => s.primary_level = level,
        (
            Output::SecondaryLevel,
            Command::SetLevel {
                level,
                transition: LevelTransition::Immediate,
            },
        ) => s.secondary_level = level,
        _ => panic!("unexpected physical command"),
    }
}

#[test]
fn secondary_on_then_off_retains_left_reading_for_every_write() {
    let mut s = state(false, true, false, 0, 0, 70);
    for target in [100, 0] {
        let plan = shared_light::plan(s, Light::Secondary, percent(target));
        for (output, command) in plan.writes {
            apply(&mut s, output, command);
            assert_eq!(fixtures(s)[1][2], 70, "reading must not even pulse off");
            assert_eq!(fixtures(s)[0][0], 0, "do not energize the primary");
        }
        assert_eq!(s, plan.expected);
        assert_eq!(fixtures(s)[0][1], target);
        assert_eq!(fixtures(s)[1][1], target, "shared middle follows secondary");
    }
}

#[test]
fn secondary_retains_right_or_both_reading_groups() {
    for (left, right) in [(false, true), (true, true)] {
        let s = state(false, left, right, 0, 0, 40);
        let result = shared_light::plan(s, Light::Secondary, percent(100)).expected;
        assert_eq!(result.left_supply, left);
        assert_eq!(result.right_supply, right);
        assert_eq!(result.reading_level, percent(40));
        assert_eq!(fixtures(result)[0][1], 100);
    }
}

#[test]
fn night_only_secondary_activation_exits_bedside_before_touching_shared_columns() {
    let mut s = state(false, true, true, 10, 0, 0);
    let plan = shared_light::plan(s, Light::Secondary, percent(100));
    assert_eq!(
        &plan.writes[..2],
        &[
            (Output::LeftSupply, Command::Switch(false)),
            (Output::RightSupply, Command::Switch(false)),
        ]
    );
    for (output, command) in plan.writes {
        apply(&mut s, output, command);
        assert_eq!(
            fixtures(s)[0][0],
            0,
            "latent night level must not light primary"
        );
    }
    assert_eq!(s, state(true, false, false, 0, 100, 0));
}

#[test]
fn unused_reading_register_is_not_an_active_reading_fixture() {
    let s = state(false, false, false, 10, 70, 100);
    assert!(!s.reading_active());
    let result = shared_light::plan(s, Light::Secondary, percent(10)).expected;
    assert_eq!(result, state(true, false, false, 0, 10, 100));
}

#[test]
fn off_zeros_target_without_cutting_peer_supply() {
    for light in [Light::Primary, Light::Secondary] {
        let s = state(true, false, false, 70, 40, 0);
        let plan = shared_light::plan(s, light, percent(0));
        assert!(plan.expected.common_supply);
        assert!(
            !plan
                .writes
                .contains(&(Output::CommonSupply, Command::Switch(false)))
        );
        assert_eq!(plan.expected.light_level(light), percent(0));
        let peer = if light == Light::Primary {
            Light::Secondary
        } else {
            Light::Primary
        };
        assert_eq!(plan.expected.light_level(peer), s.light_level(peer));
    }
}

#[test]
fn primary_control_exits_reading_but_preserves_secondary() {
    let mut s = state(true, true, true, 0, 40, 100);
    let plan = shared_light::plan(s, Light::Primary, percent(70));
    for (output, command) in plan.writes {
        apply(&mut s, output, command);
        assert_eq!(fixtures(s)[0][1], 40);
    }
    assert_eq!(s, state(true, false, false, 70, 40, 100));
}

#[test]
fn secondary_clears_latent_primary_even_when_preserving_reading() {
    let mut s = state(false, true, false, 10, 0, 100);
    let plan = shared_light::plan(s, Light::Secondary, percent(100));
    for (output, command) in plan.writes {
        apply(&mut s, output, command);
        assert_eq!(fixtures(s)[1][2], 100);
        assert_eq!(fixtures(s)[0][0], 0);
    }
    assert_eq!(s, state(true, true, false, 0, 100, 100));
}

#[test]
fn complete_matrix_preserves_peer_and_reading_at_every_write_prefix() {
    let mut cases = 0;
    for supply_bits in 0..8 {
        for primary in [0, 10, 100] {
            for secondary in [0, 10, 100] {
                for reading in [0, 10, 100] {
                    let before = state(
                        supply_bits & 1 != 0,
                        supply_bits & 2 != 0,
                        supply_bits & 4 != 0,
                        primary,
                        secondary,
                        reading,
                    );
                    for light in [Light::Primary, Light::Secondary] {
                        for target in [0, 10, 100] {
                            let plan = shared_light::plan(before, light, percent(target));
                            let target_column = if light == Light::Primary { 0 } else { 1 };
                            let peer_column = 1 - target_column;
                            let peer_before = fixtures(before)[0][peer_column];
                            let keep_reading =
                                light == Light::Secondary && reading > 0 && (supply_bits & 6 != 0);
                            let mut after = before;
                            for (output, command) in plan.writes {
                                apply(&mut after, output, command);
                                assert_eq!(
                                    fixtures(after)[0][peer_column],
                                    peer_before,
                                    "{before:?}/{light:?}/{target}"
                                );
                                if keep_reading {
                                    for row in [1, 2] {
                                        assert_eq!(
                                            fixtures(after)[row][2],
                                            fixtures(before)[row][2]
                                        );
                                    }
                                }
                            }
                            assert_eq!(after, plan.expected);
                            assert_eq!(fixtures(after)[0][target_column], target);
                            assert_eq!(after.reading_level, before.reading_level);
                            if !keep_reading {
                                assert!(!after.left_supply && !after.right_supply);
                            }
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 1296);
}
