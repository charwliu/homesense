use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use serde_json::{Value, json};
fn fixture() -> Value {
    json!({"schema_version":2,"revision":1,"rooms":[{"id":"room","name":"Room"}],
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
            {"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],
        "devices":[],"keypads":[{"id":"panel","name":"Panel","module_id":"panel-module","room":"room","enabled":true,
            "keys":[{"number":1,"label":"One","led_endpoint":{"id":"legacy:1","name":"Legacy LED","enabled":true}}]}]})
}
fn parse(value: &Value) -> Result<Configuration, homesense_config::Error> {
    Configuration::parse(
        &serde_json::to_vec(value).unwrap(),
        ExecutionContext::Simulation,
    )
}
#[test]
fn parent_is_logical_and_alias_binding_survives_address_change() {
    let mut value = fixture();
    let config = parse(&value).unwrap();
    let alias = DeviceId::new("legacy:1").unwrap();
    let parent = DeviceId::new("panel").unwrap();
    assert!(config.binding(&parent).is_none());
    let (before, _) = config.binding(&alias).unwrap();
    assert_eq!(before.name, "Legacy LED");
    assert_eq!(before.channel.get(), 1);
    assert_eq!(config.keypads()[&parent].module_id, "panel-module");
    value["gateways"][0]["modules"][0]["address"] = json!("51");
    let moved = parse(&value).unwrap();
    let (after, _) = moved.binding(&alias).unwrap();
    assert_ne!(before.address, after.address);
    assert_eq!(config.keypads(), moved.keypads());
    assert_eq!(
        config.logical_devices().keys().collect::<Vec<_>>(),
        moved.logical_devices().keys().collect::<Vec<_>>()
    );
}
#[test]
fn duplicate_unknown_cross_module_and_unreviewed_actions_are_rejected() {
    for mutate in [0, 1, 2, 3, 4, 5, 6, 7] {
        let mut v = fixture();
        match mutate {
            0 => {
                let key = v["keypads"][0]["keys"][0].clone();
                v["keypads"][0]["keys"].as_array_mut().unwrap().push(key);
            }
            1 => v["keypads"][0]["module_id"] = json!("missing"),
            2 => v["keypads"][0]["keys"][0]["number"] = json!(7),
            3 => v["keypads"][0]["keys"][0]["module_id"] = json!("other"),
            4 => {
                v["devices"] = json!([{"id":"standalone","name":"Duplicate module","module_id":"panel-module","channel":2}])
            }
            5 => {
                v["keypads"][0]["keys"][0]["action"] =
                    json!({"kind":"scene_toggle","scene":"scene","review_evidence":""})
            }
            6 => v["gateways"][0]["modules"][0]["profile"] = json!("unverified_keypad"),
            _ => v["keypads"][0]["id"] = json!("legacy:1"),
        }
        assert!(parse(&v).is_err(), "case {mutate}");
    }
}
#[test]
fn legacy_configuration_retains_exact_internal_aliases_and_hap_inputs() {
    let parent = fixture();
    let mut legacy = parent.clone();
    legacy.as_object_mut().unwrap().remove("keypads");
    legacy["devices"] = json!([{"id":"legacy:1","name":"Legacy LED","module_id":"panel-module","room":"room","channel":1,"enabled":true}]);
    let before = parse(&legacy).unwrap();
    let after = parse(&parent).unwrap();
    assert!(before.keypads().is_empty());
    assert_eq!(after.keypads().len(), 1);
    for (id, d) in before.devices() {
        let next = &after.devices()[id];
        assert_eq!(
            (
                &d.name,
                &d.room,
                d.enabled,
                &d.module_id,
                d.channel,
                d.address,
                &d.gateway
            ),
            (
                &next.name,
                &next.room,
                next.enabled,
                &next.module_id,
                next.channel,
                next.address,
                &next.gateway
            )
        );
    }
    assert_eq!(before.devices().len(), after.devices().len());
    let automation =
        br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    assert!(homesense_config::rule_document::Document::parse(automation, &after).is_ok());
}

#[test]
fn reviewed_motor_actions_require_calibration_and_unknown_scene_actions_cannot_activate() {
    let mut value = fixture();
    value["keypads"][0]["keys"][0]["action"] = json!({"kind":"motor","device":"motor","action":"stop","review_evidence":"Synthetic review"});
    assert!(parse(&value).is_err());
    value["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"curtain","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    value["devices"] =
        json!([{"id":"motor","name":"Motor","module_id":"curtain","channel":1,"enabled":true}]);
    assert!(parse(&value).is_err());
    value["devices"][0]["motor_calibration"] = json!({"open_direction":"up","stop_verified":true,"evidence":"Synthetic calibration","verified_at_unix_ms":1});
    assert!(parse(&value).is_ok());
    value["keypads"][0]["keys"][0]["action"] =
        json!({"kind":"scene_toggle","scene":"missing","review_evidence":"Synthetic review"});
    let configuration = parse(&value).unwrap();
    let automation =
        br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    assert!(homesense_config::rule_document::Document::parse(automation, &configuration).is_err());
}

#[test]
fn motor_card_press_is_generic_explicit_opt_in_and_stop_cannot_toggle() {
    use homesense_config::keypad::{KeyAction, MotorPressBehavior};
    let mut value=fixture();
    value["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"curtain","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    value["devices"]=json!([{"id":"any-curtain","name":"Motor","module_id":"curtain","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"Synthetic calibration","verified_at_unix_ms":1}}]);
    let original=json!({"kind":"motor","device":"any-curtain","action":"open","review_evidence":"Synthetic manual card mapping"});
    value["keypads"][0]["keys"][0]["action"]=original.clone();
    let config=parse(&value).unwrap();
    let action=config.keypads().values().next().unwrap().keys[&1].action.as_ref().unwrap();
    assert!(matches!(action,KeyAction::Motor{press_behavior:MotorPressBehavior::Absolute,..}));
    assert_eq!(serde_json::to_value(action).unwrap(),original,"omitted default serializes exactly as before");
    value["devices"][0]["curtain_state"]=json!({"evidence":"Synthetic shared model without travel timing","reviewed_at_unix_ms":1});
    for direction in ["open","close"] {
        value["keypads"][0]["keys"][0]["action"]["action"]=json!(direction);
        value["keypads"][0]["keys"][0]["action"]["press_behavior"]=json!("repeat_to_stop");
        let config=parse(&value).unwrap();
        assert!(matches!(config.keypads().values().next().unwrap().keys[&1].action,Some(KeyAction::Motor{press_behavior:MotorPressBehavior::RepeatToStop,..})));
    }
    let mut missing_model=value.clone();missing_model["devices"][0].as_object_mut().unwrap().remove("curtain_state");
    assert!(parse(&missing_model).is_err(),"opt-in must install a trajectory owner; travel times may remain absent");
    value["keypads"][0]["keys"][0]["action"]["action"]=json!("stop");
    assert!(parse(&value).is_err());
    value["keypads"][0]["keys"][0]["action"]["press_behavior"]=json!("absolute");
    assert!(parse(&value).is_ok());
    for invalid in [json!("toggle"),json!(false),Value::Null] {
        value["keypads"][0]["keys"][0]["action"]["press_behavior"]=invalid;
        assert!(parse(&value).is_err());
    }
    value["keypads"][0]["keys"][0]["action"]=json!({"kind":"scene_toggle","scene":"scene","review_evidence":"Synthetic review","press_behavior":"repeat_to_stop"});
    assert!(parse(&value).is_err(),"motor opt-in cannot alter scene or physical button semantics");
}

#[test]
fn native_away_mapping_and_same_key_green_are_explicit_bounded_and_roundtrip() {
    let mut v=fixture();v["keypads"][0]["keys"][0]["action"]=json!({"kind":"away_off","review_evidence":"Native reviewed operation","indicator_reset":"green"});
    let c=parse(&v).unwrap();let action=c.keypads().values().next().unwrap().keys[&1].action.as_ref().unwrap();assert_eq!(serde_json::to_value(action).unwrap(),v["keypads"][0]["keys"][0]["action"]);
    for action in [json!({"kind":"away_off","review_evidence":""}),json!({"kind":"away_off","review_evidence":"review","indicator_reset":"red"}),json!({"kind":"away_off","review_evidence":"review","indicator_reset":"green","targets":["anything"]})] {
        v["keypads"][0]["keys"][0]["action"]=action;assert!(parse(&v).is_err());
    }
}
#[test]
fn catalog_visibility_does_not_disable_physical_binding_and_absent_defaults_true() {
    let mut v=fixture();v["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"relay","address":"28","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    v["devices"]=json!([{"id":"normal","name":"Normal","module_id":"relay","channel":1,"enabled":true},{"id":"hidden","name":"Hidden","module_id":"relay","channel":2,"enabled":true,"catalog_visible":false}]);
    let c=parse(&v).unwrap();assert!(c.binding(&DeviceId::new("normal").unwrap()).unwrap().0.catalog_visible);let hidden=c.binding(&DeviceId::new("hidden").unwrap()).unwrap().0;assert!(hidden.enabled);assert!(!hidden.catalog_visible);
    v["devices"][1]["catalog_visible"]=json!("false");assert!(parse(&v).is_err());
}
