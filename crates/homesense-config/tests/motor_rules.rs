use homesense_core::{Command, ExecutionContext, MotorMotion, rule::Action};
fn config(direction: &str, model: bool) -> serde_json::Value {
    let mut d = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"37","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Synthetic","module_id":"motor-module","channel":3,"enabled":true,"motor_calibration":{"open_direction":direction,"stop_verified":true,"evidence":"synthetic calibration","verified_at_unix_ms":1}}]});
    if model {
        d["devices"][0]["curtain_state"] = serde_json::json!({"open_travel_ms":1000,"close_travel_ms":1000,"evidence":"synthetic model review","reviewed_at_unix_ms":1});
    }
    d
}
fn doc() -> serde_json::Value {
    serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"scope","enabled":false,"source":"synthetic","review_evidence":"overall rule review is not motor evidence","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"guard","action":{"kind":"if","condition":{"kind":"motor_state","device":"motor","state":"stopped","mapping_review":"synthetic source mapping"},"then_actions":[{"source":"open-source","action":{"kind":"motor","device":"motor","action":"open","mapping_review":"synthetic mapping"}},{"source":"stop-source","action":{"kind":"motor","device":"motor","action":"stop","mapping_review":"synthetic mapping"}}],"else_actions":[]}}]}]})
}
fn parse_config(d: &serde_json::Value) -> homesense_config::Configuration {
    homesense_config::Configuration::parse(
        &serde_json::to_vec(d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn parse(
    d: &serde_json::Value,
    c: &homesense_config::Configuration,
) -> Result<homesense_config::rule_document::Document, homesense_config::Error> {
    homesense_config::rule_document::Document::parse(&serde_json::to_vec(d).unwrap(), c)
}
#[test]
fn disabled_actual_only_draft_keeps_guard_order_and_calibrates_logical_direction() {
    for (direction, expected) in [("up", MotorMotion::Up), ("down", MotorMotion::Down)] {
        let parsed = parse(&doc(), &parse_config(&config(direction, true))).unwrap();
        assert!(!parsed.rules[0].enabled);
        let Action::If {
            condition,
            then_actions,
            ..
        } = &parsed.rules[0].actions[0].action
        else {
            panic!()
        };
        assert!(
            matches!(condition,homesense_core::rule::Condition::MotorState(p) if p.evidence==homesense_core::rule_motor::Evidence::ActualOnly)
        );
        assert_eq!(then_actions[0].source, "open-source");
        assert_eq!(then_actions[1].source, "stop-source");
        assert!(
            matches!(then_actions[0].action,Action::Command{command:Command::Motor(m),..} if m==expected)
        );
        assert!(matches!(
            then_actions[1].action,
            Action::Command {
                command: Command::Motor(MotorMotion::Stop),
                ..
            }
        ));
    }
}
#[test]
fn enabled_condition_requires_specific_evidence_review_model_and_valid_freshness() {
    let cfg = parse_config(&config("up", true));
    let mut d = doc();
    d["rules"][0]["enabled"] = true.into();
    assert!(
        parse(&d, &cfg).is_err(),
        "overall review cannot supply an actual motion provider"
    );
    d["rules"][0]["actions"][0]["action"]["condition"]["evidence_policy"] =
        "reviewed_command_estimate".into();
    assert!(parse(&d, &cfg).is_err());
    d["rules"][0]["actions"][0]["action"]["condition"]["evidence_review"] =
        "only this synthetic source/node/target".into();
    assert!(parse(&d, &cfg).is_ok());
    assert!(parse(&d, &parse_config(&config("up", false))).is_err());
    for age in [0, 30001] {
        let mut bad = d.clone();
        bad["rules"][0]["actions"][0]["action"]["condition"]["max_age_ms"] = age.into();
        assert!(parse(&bad, &cfg).is_err());
    }
    let mut bad = d.clone();
    bad["rules"][0]["actions"][0]["action"]["condition"]["mapping_review"] = "".into();
    assert!(parse(&bad, &cfg).is_err());
}
#[test]
fn absent_calibration_and_unreviewed_logical_intent_are_rejected_even_disabled() {
    let mut cfg = config("up", true);
    cfg["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("curtain_state");
    cfg["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("motor_calibration");
    assert!(parse(&doc(), &parse_config(&cfg)).is_err());
    let mut d = doc();
    d["rules"][0]["actions"][0]["action"]["then_actions"][0]["action"]["mapping_review"] =
        "".into();
    assert!(parse(&d, &parse_config(&config("up", true))).is_err());
}
#[test]
fn evidence_gate_covers_nested_and_scene_referenced_motor_conditions() {
    let cfg = parse_config(&config("up", true));
    let mut d = doc();
    let node = d["rules"][0]["actions"][0].clone();
    d["rules"][0]["enabled"] = true.into();
    d["immediate_scenes"] = serde_json::json!([{"id":"nested","review_evidence":"synthetic scene review","activate":[node],"deactivate":[]}]);
    d["rules"][0]["actions"] = serde_json::json!([{"source":"invoke","action":{"kind":"scene","id":"nested","activate":true}}]);
    assert!(parse(&d, &cfg).is_err());
    d["immediate_scenes"][0]["activate"][0]["action"]["condition"]["evidence_policy"] =
        "reviewed_command_estimate".into();
    d["immediate_scenes"][0]["activate"][0]["action"]["condition"]["evidence_review"] =
        "specific synthetic evidence".into();
    let parsed = parse(&d, &cfg).unwrap();
    assert_eq!(
        parsed.enabled_motor_requests().unwrap()[0].source,
        "invoke -> guard"
    );
}
