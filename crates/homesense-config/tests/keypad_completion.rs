use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{DeviceId, ExecutionContext};
use serde_json::{Value, json};

fn fixture() -> (Value, Value) {
    let devices = json!({"schema_version":2,"revision":1,
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"dimmer","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
            {"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}
        ]}],
        "devices":[
            {"id":"cabinet","name":"Cabinet","module_id":"relay","channel":1,"enabled":true},
            {"id":"entrance","name":"Entrance","module_id":"relay","channel":2,"enabled":true},
            {"id":"entrance-level","name":"Entrance level","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100,
                "supply":{"module_id":"relay","channel":2}}
        ],
        "keypads":[{"id":"panel","name":"Panel","module_id":"panel-module","enabled":true,"keys":[
            {"number":1,"label":"Cabinet","led_endpoint":{"id":"key1","name":"One","enabled":true},
                "action":{"kind":"scene_toggle","scene":"cabinet-on","review_evidence":"Synthetic source mapping"},
                "selection_completion":{"id":"cabinet-off-selection","after":{"device":"cabinet","on":false},
                    "when":[{"device":"entrance","on":true}],"variable":"entrance-mode","value":3,
                    "indicators":[{"device":"key3","on":true},{"device":"key5","on":false}],
                    "max_age_ms":5000,"review_evidence":"Synthetic key-specific conditional completion"}},
            {"number":3,"label":"Daily","led_endpoint":{"id":"key3","name":"Three","enabled":true}},
            {"number":5,"label":"Full","led_endpoint":{"id":"key5","name":"Five","enabled":true}}
        ]}]
    });
    let automation = json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"cabinet-mode","kind":"integer","lifetime":"persistent"},
            {"id":"entrance-mode","kind":"integer","lifetime":"persistent"}],"rules":[],
        "timed_scenes":[
            {"id":"cabinet-on","enabled":true,"review_evidence":"Synthetic On","max_lateness_ms":5000,
                "steps":[{"source":"cabinet/on","device":"cabinet","offset_ms":0,"command":{"kind":"switch","on":true}}]},
            {"id":"cabinet-off","enabled":true,"review_evidence":"Synthetic Off","max_lateness_ms":5000,
                "steps":[{"source":"cabinet/off","device":"cabinet","offset_ms":0,"command":{"kind":"switch","on":false}}]}
        ],
        "scene_switch_groups":[{"id":"cabinet-selection","variable":"cabinet-mode",
            "review_evidence":"Independent physical selection","members":[{"scene":"cabinet-on","value":1,"off_scene":"cabinet-off"}],
            "reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"Cabinet truth only"}}]
    });
    (devices, automation)
}
fn parse(devices: &Value) -> Result<Configuration, homesense_config::Error> {
    Configuration::parse(&serde_json::to_vec(devices).unwrap(), ExecutionContext::Simulation)
}
fn document(devices: &Value, automation: &Value) -> Result<Document, homesense_config::Error> {
    Document::parse(&serde_json::to_vec(automation).unwrap(), &parse(devices)?)
}

#[test]
fn completion_roundtrip_is_key_specific_and_adds_only_explicit_read_and_led_authority() {
    let (devices, automation) = fixture();
    let config = parse(&devices).unwrap();
    document(&devices, &automation).unwrap();
    let key = &config.keypads()[&DeviceId::new("panel").unwrap()].keys[&1];
    assert_eq!(serde_json::to_value(key).unwrap(), devices["keypads"][0]["keys"][0]);
    assert!(config.keypads()[&DeviceId::new("panel").unwrap()].keys[&3].selection_completion.is_none());
    let effect = key.selection_completion.as_ref().unwrap();
    assert_eq!(homesense_config::keypad_completion::devices(&config, effect).unwrap()
        .iter().map(DeviceId::as_str).collect::<Vec<_>>(), ["cabinet", "entrance", "key3", "key5"]);
    // The completed outcome does not silently become part of the source's
    // physical light recipe or affect another key pointing at that scene.
    assert_eq!(key.action.as_ref().unwrap(), &serde_json::from_value(devices["keypads"][0]["keys"][0]["action"].clone()).unwrap());
}

#[test]
fn combined_condition_requires_supply_authority_even_without_a_supply_output() {
    let (mut devices, automation) = fixture();
    devices["keypads"][0]["keys"][0]["selection_completion"]["when"] = json!([{"device":"entrance-level","on":true}]);
    let config = parse(&devices).unwrap();
    document(&devices, &automation).unwrap();
    let effect = config.keypads().values().next().unwrap().keys[&1].selection_completion.as_ref().unwrap();
    assert_eq!(homesense_config::keypad_completion::devices(&config, effect).unwrap()
        .iter().map(DeviceId::as_str).collect::<Vec<_>>(), ["cabinet", "entrance", "entrance-level", "key3", "key5"]);
}

#[test]
fn rejects_unreviewed_or_unbounded_contracts_and_nonlight_or_ambiguous_conditions() {
    for case in 0..15 {
        let (mut devices, _) = fixture();
        let effect = &mut devices["keypads"][0]["keys"][0]["selection_completion"];
        match case {
            0 => effect["review_evidence"] = json!(""),
            1 => effect["max_age_ms"] = json!(0),
            2 => effect["max_age_ms"] = json!(u64::MAX),
            3 => effect["after"]["device"] = json!("missing"),
            4 => effect["after"]["device"] = json!("key3"),
            5 => effect["when"] = json!([]),
            6 => effect["when"] = json!([{"device":"cabinet","on":true}]),
            7 => effect["when"] = json!([{"device":"entrance","on":true},{"device":"entrance","on":false}]),
            8 => effect["when"] = json!([{"device":"entrance","on":true,"level":100}]),
            9 => effect["when"] = json!([{"device":"entrance-level","on":false,"level":20}]),
            10 => effect["when"] = json!([{"device":"entrance-level","on":true,"level":101}]),
            11 => effect["when"] = Value::Array(vec![json!({"device":"entrance","on":true});17]),
            12 => effect["command"] = json!({"kind":"switch","on":false}),
            13 => effect["deadline_ms"] = json!(60_000),
            _ => devices["keypads"][0]["keys"][0]["action"] = json!({"kind":"scene_execute","scene":"cabinet-on","review_evidence":"Synthetic"}),
        }
        assert!(parse(&devices).is_err(), "case {case}");
    }
}

#[test]
fn effects_cannot_replace_current_key_cue_or_control_lighting_or_self_reset_leds() {
    for case in 0..6 {
        let (mut devices, _) = fixture();
        match case {
            0 => devices["keypads"][0]["keys"][0]["selection_completion"]["indicators"] = json!([{"device":"key1","on":false}]),
            1 => devices["keypads"][0]["keys"][0]["selection_completion"]["indicators"] = json!([{"device":"entrance","on":false}]),
            2 => devices["keypads"][0]["keys"][0]["selection_completion"]["indicators"] = json!([{"device":"key3","on":true},{"device":"key3","on":false}]),
            3 => devices["keypads"][0]["keys"][0]["selection_completion"]["indicators"] = json!([]),
            4 => devices["gateways"][0]["modules"][2]["led_mode"] = json!("self_reset"),
            _ => devices["keypads"][0]["keys"][1]["led_endpoint"]["enabled"] = json!(false),
        }
        assert!(parse(&devices).is_err(), "case {case}");
    }
}

#[test]
fn document_requires_typed_persistent_nonrecursive_variable_and_own_source_recipe() {
    for case in 0..6 {
        let (mut devices, mut automation) = fixture();
        match case {
            0 => devices["keypads"][0]["keys"][0]["selection_completion"]["variable"] = json!("missing"),
            1 => devices["keypads"][0]["keys"][0]["selection_completion"]["value"] = json!(false),
            2 => automation["variables"][1]["lifetime"] = json!("temporary"),
            3 => devices["keypads"][0]["keys"][0]["selection_completion"]["variable"] = json!("cabinet-mode"),
            4 => {
                devices["keypads"][0]["keys"][0]["selection_completion"]["after"] = json!({"device":"entrance","on":false});
                devices["keypads"][0]["keys"][0]["selection_completion"]["when"] = json!([{"device":"cabinet","on":false}]);
            },
            _ => automation["rules"] = json!([{"id":"cascade","source":"synthetic","enabled":false,
                "trigger":{"kind":"variable_changed","variable":"entrance-mode"},"actions":[]}]),
        }
        assert!(document(&devices, &automation).is_err(), "case {case}");
    }
}

#[test]
fn legacy_key_json_keeps_exact_absent_completion_shape() {
    let (mut devices, _) = fixture();
    devices["keypads"][0]["keys"][0].as_object_mut().unwrap().remove("selection_completion");
    let config = parse(&devices).unwrap();
    let key = &config.keypads().values().next().unwrap().keys[&1];
    assert_eq!(serde_json::to_value(key).unwrap(), devices["keypads"][0]["keys"][0]);
}

#[test]
fn observation_freshness_uses_the_module_policy_and_does_not_create_a_new_execution_deadline() {
    let (mut devices, automation) = fixture();
    let maximum = parse(&devices).unwrap().refresh_policy().observation_max_age_ms();
    devices["keypads"][0]["keys"][0]["selection_completion"]["max_age_ms"] = json!(maximum);
    document(&devices, &automation).unwrap();
    devices["keypads"][0]["keys"][0]["selection_completion"]["max_age_ms"] = json!(maximum + 1);
    assert!(parse(&devices).is_err());
    // A long-idle module's valid observations and an operation's short
    // original deadline are separate concepts; the contract cannot set TTLs.
    devices["keypads"][0]["keys"][0]["selection_completion"]["max_age_ms"] = json!(maximum);
    devices["keypads"][0]["keys"][0]["selection_completion"]["timeout_ms"] = json!(maximum);
    assert!(parse(&devices).is_err());
}

#[test]
fn shared_scene_keys_require_separate_reviewed_effect_identity_and_mapping() {
    let (mut devices, automation) = fixture();
    let source = devices["keypads"][0]["keys"][0].clone();
    let alias = &mut devices["keypads"][0]["keys"][1];
    alias["action"] = source["action"].clone();
    alias["selection_completion"] = source["selection_completion"].clone();
    alias["selection_completion"]["indicators"] = json!([{"device":"key1","on":true},{"device":"key5","on":false}]);
    assert!(parse(&devices).is_err(), "two key contracts must not share an effect ID");
    devices["keypads"][0]["keys"][1]["selection_completion"]["id"] = json!("explicit-alias-completion");
    document(&devices, &automation).unwrap();
}
