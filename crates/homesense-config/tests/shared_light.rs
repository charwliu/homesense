use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use serde_json::{Value, json};

fn document() -> Value {
    let mut devices = vec![];
    for (index, id) in ["common", "left", "right", "primary", "secondary", "reading"]
        .into_iter()
        .enumerate()
    {
        let mut device = json!({"id":id,"name":id,"module_id":if index<3 {"relay"}else{"level"},"channel":index%3+1,"enabled":true});
        if index >= 3 {
            device["default_on_level"] = json!(100);
        }
        devices.push(device);
    }
    json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
        {"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
        {"id":"level","address":"32","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}
    ]}],"devices":devices,"shared_lights":[{"id":"shared-pair","common_supply":"common","left_supply":"left","right_supply":"right","primary":"primary","secondary":"secondary","reading":"reading","review_evidence":"isolated physical matrix and explicit control policy"}]})
}

#[test]
fn logical_dependency_does_not_replace_physical_bindings_or_exclusive_supply() {
    let c = Configuration::parse(
        &serde_json::to_vec(&document()).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(c.devices().len(), 6);
    let primary = DeviceId::new("primary").unwrap();
    assert_eq!(c.logical_supply(&primary).unwrap().as_str(), "common");
    assert!(c.binding(&primary).unwrap().0.supply_relay.is_none());
    assert_eq!(c.shared_light(&primary).unwrap().id, "shared-pair");
    let document = homesense_config::rule_document::Document::parse(
        br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
        &c,
    )
    .unwrap();
    assert_eq!(
        document.shared_light_devices,
        [
            DeviceId::new("primary").unwrap(),
            DeviceId::new("secondary").unwrap()
        ]
        .into()
    );
}

#[test]
fn malformed_missing_duplicate_unreviewed_or_wrong_kind_groups_are_rejected() {
    for (pointer, value) in [
        ("/shared_lights/0/primary", json!("missing")),
        ("/shared_lights/0/reading", json!("primary")),
        ("/shared_lights/0/primary", json!("left")),
        ("/shared_lights/0/review_evidence", json!(" ")),
        ("/devices/3/default_on_level", Value::Null),
        ("/devices/2/enabled", json!(false)),
    ] {
        let mut d = document();
        *d.pointer_mut(pointer)
            .unwrap_or_else(|| panic!("{pointer}")) = value;
        assert!(
            Configuration::parse(
                &serde_json::to_vec(&d).unwrap(),
                ExecutionContext::Simulation
            )
            .is_err(),
            "{pointer}"
        );
    }
    let mut d = document();
    d["devices"][3]["supply_relay"] = json!("common");
    assert!(
        Configuration::parse(
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation
        )
        .is_err()
    );
    let mut d = document();
    let mut duplicate = d["shared_lights"][0].clone();
    duplicate["id"] = json!("other");
    d["shared_lights"].as_array_mut().unwrap().push(duplicate);
    assert!(
        Configuration::parse(
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation
        )
        .is_err()
    );
}

#[test]
fn legacy_configuration_without_shared_groups_keeps_existing_behavior() {
    let c = Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(c.shared_lights().is_empty());
}
