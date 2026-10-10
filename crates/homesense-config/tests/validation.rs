use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use serde_json::{Value, json};
const SAMPLE: &[u8] = include_bytes!("../../../config/simulation.json");
fn document() -> Value {
    serde_json::from_slice(SAMPLE).unwrap()
}
fn parse(value: &Value, context: ExecutionContext) -> bool {
    Configuration::parse(&serde_json::to_vec(value).unwrap(), context).is_ok()
}
#[test]
fn sample_only_enables_device_in_explicit_simulation_context() {
    let c = Configuration::parse(SAMPLE, ExecutionContext::Simulation).unwrap();
    assert_eq!(c.revision(), 1);
    assert!(!c.manual_degraded_enabled());
    assert!(
        c.binding(&DeviceId::new("simulation:light").unwrap())
            .is_some()
    );
    assert!(Configuration::parse(SAMPLE, ExecutionContext::Live).is_err());
    let mut d = document();
    d["devices"][0].as_object_mut().unwrap().remove("enabled");
    let c = Configuration::parse(&serde_json::to_vec(&d).unwrap(), ExecutionContext::Live).unwrap();
    assert!(
        c.binding(&DeviceId::new("simulation:light").unwrap())
            .is_none()
    );
}
#[test]
fn pending_and_unknown_keypad_never_gain_live_control() {
    let mut d = document();
    d["gateways"][0]["modules"][0]["verification"] = json!({"status":"pending"});
    assert!(!parse(&d, ExecutionContext::Simulation));
    d["devices"][0]["enabled"] = json!(false);
    d["gateways"][0]["modules"][0]["profile"] = json!("unverified_keypad");
    d["devices"][0]["channel"] = json!(12);
    assert!(parse(&d, ExecutionContext::Live));
    d["devices"][0]["enabled"] = json!(true);
    assert!(!parse(&d, ExecutionContext::Live));
}
#[test]
fn duplicate_ids_addresses_endpoints_and_channels_are_rejected() {
    let d = document();
    for path in ["device", "module", "gateway", "channel", "endpoint"] {
        let mut v = d.clone();
        match path {
            "device" => {
                let mut x = v["devices"][0].clone();
                x["channel"] = json!(2);
                v["devices"].as_array_mut().unwrap().push(x);
            }
            "channel" => {
                let mut x = v["devices"][0].clone();
                x["id"] = json!("other");
                v["devices"].as_array_mut().unwrap().push(x);
            }
            "module" => {
                let x = v["gateways"][0]["modules"][0].clone();
                v["gateways"][0]["modules"].as_array_mut().unwrap().push(x);
            }
            _ => {
                let mut x = v["gateways"][0].clone();
                if path == "endpoint" {
                    x["id"] = json!("other");
                } else {
                    x["endpoint"] = json!("127.0.0.1:50008");
                }
                v["gateways"].as_array_mut().unwrap().push(x);
            }
        }
        assert!(!parse(&v, ExecutionContext::Simulation), "{path}");
    }
}
#[test]
fn invalid_references_ranges_profiles_and_unknown_fields_fail_closed() {
    for (pointer, value) in [
        ("/devices/0/gateway", json!("missing")),
        ("/devices/0/address", json!("16")),
        ("/devices/0/channel", json!(9)),
        ("/devices/0/id", json!(" spaced ")),
        ("/gateways/0/modules/0/address", json!("00")),
        ("/gateways/0/modules/0/profile", json!("guess")),
        ("/gateways/0/endpoint", json!("0.0.0.0:50007")),
        ("/schema_version", json!(2)),
        ("/revision", json!(0)),
        ("/revision", json!(u64::MAX)),
    ] {
        let mut d = document();
        *d.pointer_mut(pointer).unwrap() = value;
        assert!(!parse(&d, ExecutionContext::Simulation), "{pointer}");
    }
    let mut d = document();
    d["allow_unverified"] = json!(true);
    assert!(!parse(&d, ExecutionContext::Simulation));
    let mut d = document();
    d["gateways"][0]["modules"][0]["verification"]["extra"] = json!(true);
    assert!(!parse(&d, ExecutionContext::Simulation));
    let duplicate = String::from_utf8(SAMPLE.to_vec()).unwrap().replacen(
        "\"revision\": 1",
        "\"revision\": 1, \"revision\": 2",
        1,
    );
    assert!(Configuration::parse(duplicate.as_bytes(), ExecutionContext::Simulation).is_err());
}
#[test]
fn verified_live_binding_requires_evidence_and_valid_panel_led_mode() {
    let mut d = document();
    d["gateways"][0]["modules"][0]["verification"] =
        json!({"status":"verified","evidence":"test fixture only","verified_at_unix_ms":1});
    assert!(parse(&d, ExecutionContext::Live));
    d["gateways"][0]["modules"][0]["led_mode"] = json!("manual");
    assert!(!parse(&d, ExecutionContext::Live));
    d["gateways"][0]["modules"][0]["profile"] = json!("bt86c_rs485_v2_9_6_2");
    assert!(parse(&d, ExecutionContext::Live));
    d["gateways"][0]["modules"][0]["verification"]["evidence"] = json!("");
    assert!(!parse(&d, ExecutionContext::Live));
}
