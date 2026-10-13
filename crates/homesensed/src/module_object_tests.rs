use super::*;
use homesense_core::{ExecutionContext, Observation, Percent};
use serde_json::{Value, json};
fn document() -> Value {
    json!({"schema_version":2,"revision":2,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"switch-module","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"dimmer-module","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"},{"id":"motor-module","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"supply","name":"Supply fixture","gateway":null,"module_id":"switch-module","channel":1,"enabled":true},{"id":"main","name":"Main fixture","module_id":"dimmer-module","channel":1,"enabled":true,"supply":{"module_id":"switch-module","channel":1},"default_on_level":50},{"id":"disabled","name":"Disabled fixture","module_id":"dimmer-module","channel":2,"enabled":false},{"id":"key","name":"Key fixture","module_id":"panel-module","channel":1,"enabled":true},{"id":"motor","name":"Motor fixture","module_id":"motor-module","channel":1,"enabled":true}]})
}
fn config(value: &Value) -> homesense_config::Configuration {
    homesense_config::Configuration::parse(
        &serde_json::to_vec(value).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn sample(feedback: &mut Feedback, epoch: u64, address: u8, command: u8, value: u8, time: u64) {
    let _ = feedback.apply(&crate::transport_owner::GatewayRead {
        gateway_id: "bus".into(),
        transport_epoch: epoch,
        received_at_ms: time,
        result: hb_transport::Receive::Frames {
            transport_epoch: epoch,
            frames: vec![hb_protocol::RawFrame {
                address,
                command,
                value,
            }],
        },
    });
}
#[test]
fn physical_module_truth_is_raw_once_while_combined_light_is_a_derived_projection() {
    let config = config(&document());
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    sample(&mut feedback, 2, 0x15, 0xd1, 30, 10);
    sample(&mut feedback, 2, 3, 0xd1, 0xf2, 10);
    let raw = feedback.snapshot();
    let view = project(
        &config,
        &raw,
        Some(feedback.module_truth()),
        11,
        5000,
        true,
        0,
        64,
    )
    .unwrap();
    let dim = view
        .iter()
        .find(|m| m.module_id.as_deref() == Some("dimmer-module"))
        .unwrap();
    assert_eq!(dim.slots.len(), 8);
    assert_eq!(
        dim.slots[0].binding.as_ref().unwrap().actual,
        Some(json!({"level":30,"on":true}))
    );
    assert_eq!(dim.slots[1].binding.as_ref().unwrap().quality, "disabled");
    assert!(dim.slots[7].binding.is_none());
    let derived = crate::combined_light::project(&config, &raw).unwrap();
    assert_eq!(
        derived.devices[&DeviceId::new("main").unwrap()].observed,
        Some(Observation::Level(Percent::new(0).unwrap()))
    );
    assert_eq!(feedback.snapshot(), raw);
    assert_eq!(
        view.iter()
            .find(|m| m.module_id.as_deref() == Some("switch-module"))
            .unwrap()
            .slots[0]
            .binding
            .as_ref()
            .unwrap()
            .supply_for,
        vec!["main"]
    );
}
#[test]
fn capability_configuration_and_actual_fresh_responses_are_distinct() {
    let config = config(&document());
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    let view = project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        1,
        100,
        true,
        0,
        64,
    )
    .unwrap();
    assert!(
        view.iter()
            .filter(|m| m.queryable_bindings > 0)
            .all(|m| m.fresh_responses == 0)
    );
    assert!(
        view.iter()
            .filter(|m| matches!(m.kind.as_str(), "Motor" | "Keypad"))
            .all(|m| m.response == "unsupported_feedback"
                && m.slots[0].binding.as_ref().unwrap().actual.is_none())
    );
    sample(&mut feedback, 2, 0x15, 0xd1, 40, 10);
    let fresh = project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        11,
        100,
        true,
        0,
        64,
    )
    .unwrap();
    assert_eq!(
        fresh
            .iter()
            .find(|m| m.module_id.as_deref() == Some("dimmer-module"))
            .unwrap()
            .response,
        "fresh"
    );
    let stale = project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        110,
        100,
        true,
        0,
        64,
    )
    .unwrap();
    assert_eq!(
        stale
            .iter()
            .find(|m| m.module_id.as_deref() == Some("dimmer-module"))
            .unwrap()
            .slots[0]
            .binding
            .as_ref()
            .unwrap()
            .quality,
        "stale"
    );
    let stopped = project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        11,
        100,
        false,
        0,
        64,
    )
    .unwrap();
    assert_eq!(
        stopped
            .iter()
            .find(|m| m.module_id.as_deref() == Some("dimmer-module"))
            .unwrap()
            .response,
        "unavailable"
    );
    assert!(
        project(
            &config,
            &feedback.snapshot(),
            Some(feedback.module_truth()),
            11,
            100,
            true,
            0,
            65
        )
        .is_err()
    );
    assert_eq!(
        project(
            &config,
            &feedback.snapshot(),
            Some(feedback.module_truth()),
            11,
            100,
            true,
            1,
            1
        )
        .unwrap()
        .len(),
        1
    );
}
#[test]
fn stable_channel_resolution_rejects_unbound_disabled_hidden_and_invalid_commands() {
    let config = config(&document());
    assert_eq!(
        control_device(&config, "dimmer-module", 1)
            .unwrap()
            .as_str(),
        "main"
    );
    for (module, ch) in [
        ("03", 1),
        ("missing", 1),
        ("dimmer-module", 2),
        ("dimmer-module", 8),
        ("dimmer-module", 0),
        ("dimmer-module", 9),
        ("switch-module", 1),
        ("panel-module", 1),
    ] {
        assert!(
            control_device(&config, module, ch).is_err(),
            "{module} {ch}"
        );
    }
}
#[test]
fn replaced_physical_mapping_rejects_old_address_and_old_epoch_feedback() {
    let old = config(&document());
    let mut updated = document();
    updated["revision"] = 3.into();
    updated["gateways"][0]["modules"][1]["address"] = "61".into();
    let new = config(&updated);
    assert_eq!(
        old.devices()[&DeviceId::new("main").unwrap()].module_id,
        new.devices()[&DeviceId::new("main").unwrap()].module_id
    );
    let mut feedback = Feedback::new(&new, ExecutionContext::Simulation, 7, 2).unwrap();
    sample(&mut feedback, 2, 0x15, 0xd1, 30, 10);
    sample(&mut feedback, 1, 0x61, 0xd1, 50, 11);
    assert_eq!(
        feedback.observation_count(&DeviceId::new("main").unwrap()),
        Some(0)
    );
    sample(&mut feedback, 2, 0x61, 0xd1, 40, 12);
    assert_eq!(
        feedback.observation_count(&DeviceId::new("main").unwrap()),
        Some(1)
    );
    let view = project(
        &new,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        13,
        5000,
        true,
        0,
        64,
    )
    .unwrap();
    assert_eq!(
        view.iter()
            .find(|m| m.module_id.as_deref() == Some("dimmer-module"))
            .unwrap()
            .slots[0]
            .binding
            .as_ref()
            .unwrap()
            .actual,
        Some(json!({"level":40,"on":true}))
    );
}
