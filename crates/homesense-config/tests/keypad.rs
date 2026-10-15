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
