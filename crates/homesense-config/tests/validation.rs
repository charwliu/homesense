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

#[test]
fn pro_profiles_preserve_channel_limits_and_live_verification_gate() {
    for (profile, channels) in [("r8i8_din_pro_v2_7_7", 8), ("c4_din_pro_v2_7_7", 4)] {
        let mut d = document();
        d["gateways"][0]["modules"][0]["profile"] = json!(profile);
        d["devices"][0]["channel"] = json!(channels);
        assert!(parse(&d, ExecutionContext::Simulation));
        assert!(!parse(&d, ExecutionContext::Live));
        d["devices"][0]["channel"] = json!(channels + 1);
        assert!(!parse(&d, ExecutionContext::Simulation));
        d["devices"][0]["channel"] = json!(channels);
        d["devices"][0]["enabled"] = json!(false);
        d["gateways"][0]["modules"][0]["verification"] = json!({"status":"pending"});
        assert!(parse(&d, ExecutionContext::Live));
    }
}

#[test]
fn ts6_requires_explicit_six_key_profile_and_bounds() {
    let mut d = document();
    d["gateways"][0]["modules"][0]["profile"] = json!("ts6_86_rs485_v2_6_5_6key");
    d["devices"][0]["channel"] = json!(6);
    assert!(parse(&d, ExecutionContext::Simulation));
    d["devices"][0]["channel"] = json!(7);
    assert!(!parse(&d, ExecutionContext::Simulation));
    d["devices"][0]["channel"] = json!(1);
    d["gateways"][0]["modules"][0]["profile"] = json!("ts6_86_rs485_v2_6_5");
    assert!(!parse(&d, ExecutionContext::Simulation));
}

#[test]
fn default_on_level_is_explicit_positive_and_dimmer_only() {
    for level in [1, 30, 100] {
        let mut d = document();
        d["devices"][0]["default_on_level"] = json!(level);
        let c = Configuration::parse(
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap();
        assert_eq!(
            c.devices()
                .values()
                .next()
                .unwrap()
                .default_on_level
                .unwrap()
                .get(),
            level
        );
    }
    for level in [json!(0), json!(101), json!(-1), json!(1.5), json!("30")] {
        let mut d = document();
        d["devices"][0]["default_on_level"] = level;
        assert!(!parse(&d, ExecutionContext::Simulation));
    }
    let mut d = document();
    d["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("default_on_level");
    let c = Configuration::parse(
        &serde_json::to_vec(&d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        c.devices()
            .values()
            .next()
            .unwrap()
            .default_on_level
            .is_none()
    );
    d["gateways"][0]["modules"][0]["profile"] = json!("r8i8_din_pro_v2_7_7");
    d["devices"][0]["default_on_level"] = json!(30);
    assert!(!parse(&d, ExecutionContext::Simulation));
}

#[test]
fn motor_actions_require_per_channel_direction_and_stop_evidence() {
    use homesense_core::{Command, MotorAction as A, MotorMotion as M};
    let mut d = json!({
        "schema_version":1,"revision":1,
        "gateways":[{"id":"test","endpoint":"127.0.0.1:50007","modules":[{
            "address":"22","profile":"c4_din_pro_v2_7_7",
            "verification":{"status":"simulated"}
        }]}],
        "devices":[{"id":"motor","name":"Fixture only","gateway":"test","address":"22","channel":2,"enabled":true}]
    });
    let id = DeviceId::new("motor").unwrap();
    let config = Configuration::parse(
        &serde_json::to_vec(&d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    for action in [A::Open, A::Close, A::Stop] {
        assert!(config.motor_command(&id, action).is_err());
    }
    for (direction, open, close) in [("up", M::Up, M::Down), ("down", M::Down, M::Up)] {
        d["devices"][0]["motor_calibration"] = json!({
            "open_direction":direction,"stop_verified":true,
            "evidence":"offline fixture; not field calibration","verified_at_unix_ms":1
        });
        let config = Configuration::parse(
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap();
        for (action, motion) in [(A::Open, open), (A::Close, close), (A::Stop, M::Stop)] {
            assert_eq!(
                config.motor_command(&id, action).unwrap(),
                Command::Motor(motion)
            );
        }
    }
    for (key, value) in [
        ("open_direction", json!("stop")),
        ("stop_verified", json!(false)),
        ("evidence", json!(" ")),
        ("verified_at_unix_ms", json!(0)),
    ] {
        let mut invalid = d.clone();
        invalid["devices"][0]["motor_calibration"][key] = value;
        assert!(!parse(&invalid, ExecutionContext::Simulation), "{key}");
    }
    let mut second = d["devices"][0].clone();
    second["id"] = json!("uncalibrated");
    second["channel"] = json!(3);
    second.as_object_mut().unwrap().remove("motor_calibration");
    d["devices"].as_array_mut().unwrap().push(second);
    let config = Configuration::parse(
        &serde_json::to_vec(&d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        config
            .motor_command(&DeviceId::new("uncalibrated").unwrap(), A::Open)
            .is_err()
    );
    d["devices"][0]["enabled"] = json!(false);
    let config = Configuration::parse(
        &serde_json::to_vec(&d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(config.motor_command(&id, A::Stop).is_err());
    d["gateways"][0]["modules"][0]["profile"] = json!("r8i8_din_pro_v2_7_7");
    assert!(!parse(&d, ExecutionContext::Simulation));
}

fn combined_document() -> Value {
    let mut d = document();
    d["gateways"][0]["modules"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "address":"03", "profile":"r8i8_din_pro_v2_7_7", "verification":{"status":"simulated"}
        }));
    d["devices"][0]["supply_relay"] = json!("relay");
    d["devices"][0]["default_on_level"] = json!(100);
    d["devices"].as_array_mut().unwrap().push(json!({
        "id":"relay", "name":"Supply", "gateway":"simulation", "address":"03", "channel":4, "enabled":true
    }));
    d
}
#[test]
fn combined_light_references_are_validated_without_enabling_pending_hardware() {
    let d = combined_document();
    assert!(parse(&d, ExecutionContext::Simulation));
    assert!(!parse(&d, ExecutionContext::Live));
    for bad in ["missing", "simulation:light"] {
        let mut v = d.clone();
        v["devices"][0]["supply_relay"] = json!(bad);
        assert!(!parse(&v, ExecutionContext::Simulation));
    }
    let mut v = d.clone();
    v["devices"][1]["enabled"] = json!(false);
    assert!(!parse(&v, ExecutionContext::Simulation));
    let mut v = d.clone();
    v["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("default_on_level");
    assert!(!parse(&v, ExecutionContext::Simulation));
    let mut v = d.clone();
    let mut second = v["devices"][0].clone();
    second["id"] = json!("second");
    second["channel"] = json!(2);
    v["devices"].as_array_mut().unwrap().push(second);
    assert!(!parse(&v, ExecutionContext::Simulation));
    let mut v = d.clone();
    v["devices"][1]["supply_relay"] = json!("simulation:light");
    assert!(!parse(&v, ExecutionContext::Simulation));
}

#[test]
fn optional_rooms_validate_labels_unique_ids_and_references() {
    let mut value=document();
    assert!(Configuration::parse(SAMPLE,ExecutionContext::Simulation).unwrap().rooms().is_empty());
    value["rooms"]=json!([{"id":"study","name":"书房","floor":"二层"}]);
    value["devices"][0]["room"]=json!("study");
    let config=Configuration::parse(&serde_json::to_vec(&value).unwrap(),ExecutionContext::Simulation).unwrap();
    assert_eq!(config.rooms()["study"].floor.as_deref(),Some("二层"));
    assert_eq!(config.devices().values().next().unwrap().room.as_deref(),Some("study"));
    for rooms in [json!([{"id":"study","name":""}]),json!([{"id":"study","name":"Study","floor":"\n"}]),json!([{"id":"study","name":"A"},{"id":"study","name":"B"}]),json!([{"id":"other","name":"Other"}]),json!([{"id":"study","name":"A","unknown":true}])] {
        let mut bad=value.clone();bad["rooms"]=rooms;assert!(!parse(&bad,ExecutionContext::Simulation));
    }
    value["rooms"]=json!((0..257).map(|i|json!({"id":format!("r{i}"),"name":"Room"})).collect::<Vec<_>>());
    value["devices"][0].as_object_mut().unwrap().remove("room");assert!(!parse(&value,ExecutionContext::Simulation));
}
