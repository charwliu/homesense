use homesense_import::study::{Action, Evaluation, Rule, evaluate};
fn rules() -> Vec<Rule> {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../config/test/legacy-study/scenes.json")).unwrap();
    serde_json::from_value(v["rules"].clone()).unwrap()
}
#[test]
fn all_modes_keys_and_unknown_preserve_source_order() {
    let rules = rules();
    for (key, target, scene) in [(1, 1, "10"), (3, 2, "6"), (5, 3, "8")] {
        assert_eq!(
            evaluate(&rules, key, true, None).unwrap(),
            Evaluation::Unknown
        );
        assert_eq!(
            evaluate(&rules, key, false, Some(0)).unwrap(),
            Evaluation::Ignored
        );
        for mode in 0..=3 {
            let Evaluation::Planned { next_mode, actions } =
                evaluate(&rules, key, true, Some(mode)).unwrap()
            else {
                panic!()
            };
            assert_eq!(next_mode, if mode == target { 0 } else { target });
            let at=actions.iter().position(|a|matches!(a,Action::Scene{scene_id,activate,..} if scene_id==scene && *activate==(mode!=target))).unwrap();
            assert_eq!(
                at,
                if key == 5 {
                    if mode == target { 2 } else { 4 }
                } else {
                    1
                }
            );
        }
    }
    assert_eq!(
        evaluate(&rules, 2, true, Some(0)).unwrap(),
        Evaluation::Ignored
    );
    assert!(evaluate(&rules, 1, true, Some(4)).is_err());
}
#[test]
fn three_scene_levels_and_confirmed_deactivation_are_exact() {
    let mut d: serde_json::Value =
        serde_json::from_str(include_str!("../../../config/test/legacy-study/scenes.json")).unwrap();
    for (id, levels) in [
        ("10", [100, 0, 0, 100]),
        ("6", [100, 100, 100, 100]),
        ("8", [0, 100, 17, 0]),
    ] {
        let plan = homesense_import::study::targets(&d, id, true).unwrap();
        assert_eq!(plan.iter().map(|x| x.level).collect::<Vec<_>>(), levels);
        assert_eq!(plan[2].source_rate, 1000);
        assert!(
            homesense_import::study::targets(&d, id, false)
                .unwrap()
                .iter()
                .all(|x| x.level == 0)
        );
    }
    d["proxy_to_device"]["211"] = "control4:999".into();
    assert!(homesense_import::study::targets(&d, "10", true).is_err());
}
