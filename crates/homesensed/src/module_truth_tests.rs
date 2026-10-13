use super::*;
use homesense_core::{ExecutionContext, Percent};
use serde_json::{Value, json};
fn input() -> Value {
    json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"stable-dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"id":"stable-supply","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"stable-dimmer","channel":1,"enabled":true,"default_on_level":50,"supply":{"module_id":"stable-supply","channel":1}},{"id":"supply","name":"Synthetic supply","module_id":"stable-supply","channel":1,"enabled":true}]})
}
async fn hap_projector() -> homesense_hap::events::ObservedEvents {
    use hap::{
        accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
        characteristic::HapCharacteristic,
        server::AccessoryHandle,
    };
    let light = LightbulbAccessory::new(8, AccessoryInformation::default()).unwrap();
    let binding = homesense_hap::requests::LightBinding {
        device: DeviceId::new("light").unwrap(),
        aid: 8,
        on_iid: light.lightbulb.power_state.get_id(),
        brightness_iid: Some(light.lightbulb.brightness.as_ref().unwrap().get_id()),
        default_on_level: None,
    };
    let handle: AccessoryHandle =
        std::sync::Arc::new(hap::futures::lock::Mutex::new(Box::new(light)));
    homesense_hap::events::ObservedEvents::new(vec![(binding, handle)], 5000)
        .await
        .unwrap()
}
#[tokio::test]
async fn actual_hap_snapshot_and_events_use_logical_versions_without_rewriting_physical_versions() {
    let mut value = input();
    value["devices"].as_array_mut().unwrap().truncate(1);
    value["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("supply");
    let config = config(&value);
    let mut truth = ModuleTruth::new(&config, 7, 2).unwrap();
    let mut publisher = hap_projector().await;
    let initial = truth.snapshot();
    assert_eq!(initial.cursor.sequence, 1);
    assert_eq!(initial.state_version, initial.cursor.sequence);
    publisher.resynchronize(&initial, 0).await.unwrap();
    let mut cursor = initial.cursor;
    for channel in 2..=8 {
        truth
            .observe(
                "bus",
                0x15,
                channel,
                Observation::Level(Percent::new(37).unwrap()),
                2,
                10,
            )
            .unwrap();
    }
    assert_eq!(truth.snapshot(), initial);
    publisher
        .refresh_snapshot(&truth.snapshot(), 10)
        .await
        .unwrap();
    for (at, level) in [(11, 30), (12, 50)] {
        truth
            .observe(
                "bus",
                0x15,
                1,
                Observation::Level(Percent::new(level).unwrap()),
                2,
                at,
            )
            .unwrap();
        let physical = truth.module_state("stable-dimmer", 1).unwrap();
        let events = truth.events_since(cursor).unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.state.version, event.cursor.sequence);
        assert!(physical.version > event.state.version);
        publisher.apply(event, at).await.unwrap();
        publisher
            .refresh_snapshot(&truth.snapshot(), at)
            .await
            .unwrap();
        assert_eq!(truth.module_state("stable-dimmer", 1).unwrap(), physical);
        cursor = truth.cursor();
    }
    truth.invalidate_port("bus", 0x15, 1).unwrap();
    for event in truth.events_since(cursor).unwrap() {
        publisher.apply(&event, 13).await.unwrap();
    }
    cursor = truth.cursor();
    truth
        .observe(
            "bus",
            0x15,
            1,
            Observation::Level(Percent::new(50).unwrap()),
            2,
            14,
        )
        .unwrap();
    for event in truth.events_since(cursor).unwrap() {
        publisher.apply(&event, 14).await.unwrap();
    }
    publisher
        .refresh_snapshot(&truth.snapshot(), 14)
        .await
        .unwrap();
    assert_eq!(truth.snapshot().state_version, truth.cursor().sequence);
}
#[tokio::test]
async fn actual_hap_resync_after_large_quality_batch_accepts_subsequent_logical_event() {
    let mut value = input();
    value["devices"].as_array_mut().unwrap().truncate(1);
    value["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("supply");
    let modules = value["gateways"][0]["modules"].as_array_mut().unwrap();
    modules.clear();
    for address in 1..=40 {
        modules.push(json!({"id":format!("m{address}"),"address":format!("{address:02X}"),"profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}));
    }
    value["devices"][0]["module_id"] = "m1".into();
    let config = config(&value);
    let mut truth = ModuleTruth::new(&config, 7, 2).unwrap();
    let mut publisher = hap_projector().await;
    publisher.resynchronize(&truth.snapshot(), 0).await.unwrap();
    let mut cursor = truth.cursor();
    for address in 1..=40 {
        for channel in 1..=8 {
            truth
                .observe(
                    "bus",
                    address,
                    channel,
                    Observation::Level(Percent::new(30).unwrap()),
                    2,
                    10,
                )
                .unwrap();
        }
    }
    for event in truth.events_since(cursor).unwrap() {
        publisher.apply(&event, 10).await.unwrap();
    }
    cursor = truth.cursor();
    truth.mark_stale(20, 1).unwrap();
    assert_eq!(
        truth.events_since(cursor),
        Err(homesense_state::StateError::ResyncRequired)
    );
    let resync = truth.snapshot();
    publisher.resynchronize(&resync, 20).await.unwrap();
    truth
        .observe(
            "bus",
            1,
            1,
            Observation::Level(Percent::new(50).unwrap()),
            2,
            21,
        )
        .unwrap();
    for event in truth.events_since(resync.cursor).unwrap() {
        publisher.apply(&event, 21).await.unwrap();
    }
    publisher
        .refresh_snapshot(&truth.snapshot(), 21)
        .await
        .unwrap();
}
fn config(input: &Value) -> Configuration {
    Configuration::parse(
        &serde_json::to_vec(input).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn receive(
    feedback: &mut crate::feedback::Feedback,
    address: u8,
    channel: u8,
    value: u8,
    epoch: u64,
    at: u64,
) -> Result<usize, String> {
    feedback.apply(&crate::transport_owner::GatewayRead {
        gateway_id: "bus".into(),
        transport_epoch: epoch,
        received_at_ms: at,
        result: hb_transport::Receive::Frames {
            transport_epoch: epoch,
            frames: vec![hb_protocol::RawFrame {
                address,
                command: 0xd0 + channel,
                value,
            }],
        },
    })
}
#[test]
fn valid_unbound_channel_is_owned_without_creating_a_logical_device_or_control_permission() {
    let config = config(&input());
    let mut feedback =
        crate::feedback::Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    let before = feedback.snapshot();
    assert_eq!(receive(&mut feedback, 0x15, 2, 37, 2, 10).unwrap(), 1);
    let channel = feedback
        .module_truth()
        .module_state("stable-dimmer", 2)
        .unwrap();
    assert_eq!(
        channel.observed,
        Some(Observation::Level(Percent::new(37).unwrap()))
    );
    assert_eq!(channel.quality, homesense_state::Quality::Confirmed);
    assert_eq!(channel.observed_at_ms, Some(10));
    assert_eq!(feedback.snapshot().devices, before.devices);
    assert_eq!(feedback.snapshot().cursor, before.cursor);
    assert_eq!(config.logical_devices().len(), 2);
    assert!(crate::management_modules::control_device(&config, "stable-dimmer", 2).is_err());
    assert_eq!(feedback.module_truth().module_objects().len(), 2);
    for ch in 1..=8 {
        assert!(
            feedback
                .module_truth()
                .module_state("stable-dimmer", ch)
                .is_some()
        );
    }
}
#[test]
fn disabled_channel_receives_truth_but_stays_out_of_logical_snapshot_and_authorization() {
    let mut value = input();
    value["devices"].as_array_mut().unwrap().push(json!({"id":"disabled","name":"Disabled fixture","module_id":"stable-dimmer","channel":2,"enabled":false}));
    let config = config(&value);
    let mut feedback =
        crate::feedback::Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    receive(&mut feedback, 0x15, 2, 37, 2, 10).unwrap();
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 2)
            .unwrap()
            .observed,
        Some(Observation::Level(Percent::new(37).unwrap()))
    );
    assert!(
        !feedback
            .snapshot()
            .devices
            .contains_key(&DeviceId::new("disabled").unwrap())
    );
    assert!(crate::management_modules::control_device(&config, "stable-dimmer", 2).is_err());
    let page = crate::management_modules::project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        11,
        5000,
        true,
        0,
        64,
    )
    .unwrap();
    let module = page
        .iter()
        .find(|m| m.module_id.as_deref() == Some("stable-dimmer"))
        .unwrap();
    assert_eq!(module.slots[1].actual, Some(json!({"on":true,"level":37})));
    assert_eq!(module.slots[1].quality, "confirmed");
    assert!(module.slots[1].binding.as_ref().unwrap().actions.is_empty());
}
#[test]
fn invalid_raw_feedback_preserves_last_sample_without_publishing_it_as_fresh_truth() {
    let config = config(&input());
    let mut feedback =
        crate::feedback::Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    receive(&mut feedback, 0x15, 1, 30, 2, 10).unwrap();
    receive(&mut feedback, 0x15, 2, 37, 2, 10).unwrap();
    assert_eq!(receive(&mut feedback, 0x15, 1, 101, 2, 11).unwrap(), 0);
    assert_eq!(receive(&mut feedback, 0x15, 2, 101, 2, 11).unwrap(), 0);
    let page = crate::management_modules::project(
        &config,
        &feedback.snapshot(),
        Some(feedback.module_truth()),
        12,
        5000,
        true,
        0,
        64,
    )
    .unwrap();
    let module = page
        .iter()
        .find(|m| m.module_id.as_deref() == Some("stable-dimmer"))
        .unwrap();
    for (index, level) in [(0, 30), (1, 37)] {
        assert_eq!(module.slots[index].quality, "unknown");
        assert_eq!(module.slots[index].observed_ms.as_deref(), Some("10"));
        assert_eq!(
            module.slots[index].actual,
            Some(json!({"on":true,"level":level}))
        );
    }
    assert_eq!(module.slots[0].binding.as_ref().unwrap().quality, "unknown");
    assert!(
        !feedback
            .module_truth()
            .rule_feedback(2, 12, 5000)
            .unwrap()
            .contains_key(&DeviceId::new("light").unwrap())
    );
    assert!(
        feedback
            .native_scene_capture(&[DeviceId::new("light").unwrap()], 12, 5000)
            .is_err()
    );
    receive(&mut feedback, 0x15, 1, 30, 2, 13).unwrap();
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 1)
            .unwrap()
            .quality,
        homesense_state::Quality::Confirmed
    );
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 2)
            .unwrap()
            .quality,
        homesense_state::Quality::Unknown
    );
}
#[test]
fn logical_renaming_and_references_are_read_only_views_over_unchanged_module_objects() {
    let config1 = config(&input());
    let mut truth = ModuleTruth::new(&config1, 7, 2).unwrap();
    truth
        .observe(
            "bus",
            0x15,
            1,
            Observation::Level(Percent::new(30).unwrap()),
            2,
            10,
        )
        .unwrap();
    truth
        .observe(
            "bus",
            0x15,
            3,
            Observation::Level(Percent::new(37).unwrap()),
            2,
            11,
        )
        .unwrap();
    let before1 = truth.module_state("stable-dimmer", 1);
    let before3 = truth.module_state("stable-dimmer", 3);
    let mut renamed = input();
    renamed["devices"][0]["id"] = "renamed-light".into();
    renamed["devices"][0]["name"] = "Renamed fixture".into();
    renamed["devices"][0]["channel"] = 3.into();
    let renamed = config(&renamed);
    let projected = truth.project_logical(&renamed).unwrap();
    assert_eq!(
        projected.devices[&DeviceId::new("renamed-light").unwrap()].observed,
        Some(Observation::Level(Percent::new(37).unwrap()))
    );
    assert!(
        !projected
            .devices
            .contains_key(&DeviceId::new("light").unwrap())
    );
    assert_eq!(truth.module_state("stable-dimmer", 1), before1);
    assert_eq!(truth.module_state("stable-dimmer", 3), before3);
    assert_eq!(
        config1.logical_devices()[&DeviceId::new("light").unwrap()]
            .module_id
            .as_deref(),
        Some("stable-dimmer")
    );
}
#[test]
fn combined_projection_uses_exactly_the_owned_two_ports_without_mutating_physical_level() {
    let config = config(&input());
    let mut feedback =
        crate::feedback::Feedback::new(&config, ExecutionContext::Simulation, 7, 2).unwrap();
    receive(&mut feedback, 0x15, 1, 50, 2, 10).unwrap();
    receive(&mut feedback, 3, 1, 0xf2, 2, 11).unwrap();
    let before = feedback
        .module_truth()
        .module_state("stable-dimmer", 1)
        .unwrap();
    let projected = crate::combined_light::project(&config, &feedback.snapshot()).unwrap();
    assert_eq!(
        projected.devices[&DeviceId::new("light").unwrap()].observed,
        Some(Observation::Level(Percent::new(0).unwrap()))
    );
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 1)
            .unwrap(),
        before
    );
}
#[test]
fn replacement_requires_fresh_owner_and_rejects_old_locator_and_epoch() {
    let old = config(&input());
    let truth = ModuleTruth::new(&old, 7, 1).unwrap();
    let mut moved = input();
    moved["revision"] = 2.into();
    moved["gateways"][0]["modules"][0]["address"] = "61".into();
    let moved = config(&moved);
    assert!(truth.project_logical(&moved).is_err());
    let mut feedback =
        crate::feedback::Feedback::new(&moved, ExecutionContext::Simulation, 7, 2).unwrap();
    assert_eq!(receive(&mut feedback, 0x15, 2, 37, 2, 10).unwrap(), 0);
    assert!(receive(&mut feedback, 0x61, 2, 37, 1, 11).is_err());
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 2)
            .unwrap()
            .observed,
        None
    );
    assert_eq!(receive(&mut feedback, 0x61, 2, 37, 2, 12).unwrap(), 1);
    assert_eq!(
        feedback
            .module_truth()
            .module_state("stable-dimmer", 2)
            .unwrap()
            .observed_at_ms,
        Some(12)
    );
}
#[test]
fn unbound_events_do_not_create_false_gaps_in_logical_event_projections() {
    let config = config(&input());
    let mut truth = ModuleTruth::new(&config, 7, 2).unwrap();
    let start = truth.cursor();
    for ch in 2..=8 {
        truth
            .observe(
                "bus",
                0x15,
                ch,
                Observation::Level(Percent::new(37).unwrap()),
                2,
                10,
            )
            .unwrap();
    }
    assert_eq!(truth.cursor(), start);
    assert!(truth.events_since(start).unwrap().is_empty());
    truth
        .observe(
            "bus",
            0x15,
            1,
            Observation::Level(Percent::new(30).unwrap()),
            2,
            11,
        )
        .unwrap();
    let events = truth.events_since(start).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].device_id.as_str(), "light");
    assert_eq!(events[0].cursor.sequence, start.sequence + 1);
}
#[test]
fn bulk_physical_staleness_invalidates_projection_cursor_without_losing_truth() {
    let mut input = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[]}],"devices":[{"id":"light","name":"Fixture","module_id":"module-1","channel":1,"enabled":true}]});
    for address in 1..=40 {
        input["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":format!("module-{address}"),"address":format!("{address:02X}"),"profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}));
    }
    let config = config(&input);
    let mut truth = ModuleTruth::new(&config, 7, 2).unwrap();
    for address in 1..=40 {
        for ch in 1..=8 {
            truth
                .observe(
                    "bus",
                    address,
                    ch,
                    Observation::Level(Percent::new(30).unwrap()),
                    2,
                    10,
                )
                .unwrap();
        }
    }
    let cursor = truth.cursor();
    truth.mark_stale(20, 1).unwrap();
    assert!(truth.events_since(cursor).is_err());
    assert_eq!(
        truth.module_state("module-40", 8).unwrap().quality,
        homesense_state::Quality::Stale
    );
    assert_eq!(
        truth.snapshot().devices[&DeviceId::new("light").unwrap()].quality,
        homesense_state::Quality::Stale
    );
}
