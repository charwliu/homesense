use homesense_core::ExecutionContext;
fn document() -> serde_json::Value {
    serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"26","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"panel-module","address":"4A","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"stop_led_idle_ms":200,"evidence":"synthetic review","reviewed_at_unix_ms":1,"leds":{"open":["open-led"],"close":["close-led"],"stop":["stop-led"]}}},{"id":"open-led","name":"Open LED","module_id":"panel-module","channel":2,"enabled":true},{"id":"stop-led","name":"Stop LED","module_id":"panel-module","channel":4,"enabled":true},{"id":"close-led","name":"Close LED","module_id":"panel-module","channel":6,"enabled":true}]})
}
fn parse(
    d: &serde_json::Value,
) -> Result<homesense_config::Configuration, homesense_config::Error> {
    homesense_config::Configuration::parse(
        &serde_json::to_vec(d).unwrap(),
        ExecutionContext::Simulation,
    )
}
#[test]
fn explicit_generic_binding_validates_and_old_config_has_no_implicit_estimate() {
    let mut d = document();
    let config = parse(&d).unwrap();
    let state = config
        .devices()
        .values()
        .find_map(|d| d.curtain_state.as_ref())
        .unwrap();
    assert_eq!(state.timing.open_travel_ms, Some(1000));
    assert_eq!(state.leds.stop[0].as_str(), "stop-led");
    d["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("curtain_state");
    assert!(
        parse(&d)
            .unwrap()
            .devices()
            .values()
            .all(|d| d.curtain_state.is_none())
    );
}
#[test]
fn inference_requires_calibration_review_and_bounded_timing() {
    for field in ["motor_calibration", "enabled"] {
        let mut d = document();
        d["devices"][0].as_object_mut().unwrap().remove(field);
        assert!(parse(&d).is_err());
    }
    for (key, value) in [
        ("open_travel_ms", serde_json::json!(0)),
        ("close_travel_ms", serde_json::json!(86400001)),
        ("evidence", serde_json::json!("")),
        ("reviewed_at_unix_ms", serde_json::json!(0)),
    ] {
        let mut d = document();
        d["devices"][0]["curtain_state"][key] = value;
        assert!(parse(&d).is_err(), "{key}");
    }
}
#[test]
fn leds_cannot_bind_missing_shared_disabled_nonmanual_or_nonkeypad_targets() {
    for target in ["missing", "curtain", "stop-led"] {
        let mut d = document();
        d["devices"][0]["curtain_state"]["leds"]["open"] = serde_json::json!([target]);
        assert!(parse(&d).is_err(), "{target}");
    }
    for mode in ["unknown", "self_reset"] {
        let mut d = document();
        d["gateways"][0]["modules"][1]["led_mode"] = serde_json::json!(mode);
        assert!(parse(&d).is_err());
    }
    let mut d = document();
    d["devices"][1]["enabled"] = false.into();
    assert!(parse(&d).is_err());
    let mut d = document();
    let mut second = d["devices"][0].clone();
    second["id"] = "another-curtain".into();
    second["channel"] = 2.into();
    d["devices"].as_array_mut().unwrap().push(second);
    assert!(parse(&d).is_err());
}
#[test]
fn curtain_led_owner_is_exclusive_across_enabled_rules_and_scene_outputs() {
    let config = parse(&document()).unwrap();
    let node = serde_json::json!({"source":"synthetic","action":{"kind":"command","device":"stop-led","command":{"kind":"panel_led","on":true}}});
    let mut rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"independent","source":"synthetic","enabled":false,"review_evidence":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[node.clone()]}]});
    let parse_rules = |rules: &serde_json::Value| {
        homesense_config::rule_document::Document::parse(
            &serde_json::to_vec(rules).unwrap(),
            &config,
        )
    };
    assert!(
        parse_rules(&rules).is_ok(),
        "disabled rule remains local review data"
    );
    rules["rules"][0]["enabled"] = true.into();
    assert!(parse_rules(&rules).is_err());
    rules["rules"][0]["actions"] = serde_json::json!([{"source":"branch","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"integer","value":1},"right":{"kind":"integer","value":2}},"then_actions":[node.clone()],"else_actions":[]}}]);
    assert!(
        parse_rules(&rules).is_err(),
        "unreachable condition is not LED ownership permission"
    );
    rules["rules"] = serde_json::json!([]);
    rules["timed_scenes"] = serde_json::json!([{"id":"independent","enabled":false,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"stop-led","command":{"kind":"panel_led","on":true}}]}]);
    assert!(parse_rules(&rules).is_ok());
    rules["timed_scenes"][0]["enabled"] = true.into();
    assert!(parse_rules(&rules).is_err());
    rules["timed_scenes"] = serde_json::json!([]);
    rules["immediate_scenes"] = serde_json::json!([{"id":"independent","review_evidence":"synthetic","activate":[node],"deactivate":[]}]);
    assert!(
        parse_rules(&rules).is_err(),
        "available immediate scene cannot be a second LED writer"
    );
}
