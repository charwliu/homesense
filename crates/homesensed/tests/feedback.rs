use hb_protocol::RawFrame;
use hb_transport::Receive;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext, Observation, Percent};
use homesense_state::Quality;
use homesensed::{feedback::Feedback, transport_owner::GatewayRead};
fn read(epoch: u64, time: u64, value: u8) -> GatewayRead {
    GatewayRead {
        gateway_id: "simulation".into(),
        transport_epoch: epoch,
        received_at_ms: time,
        result: Receive::Frames {
            transport_epoch: epoch,
            frames: vec![RawFrame {
                address: 0x15,
                command: 0xD1,
                value,
            }],
        },
    }
}
#[test]
fn configured_feedback_rejects_wrong_source_and_epoch_and_stales_on_disconnect() {
    let config = Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 123, 1).unwrap();
    let id = DeviceId::new("simulation:light").unwrap();
    assert!(feedback.apply(&read(2, 10, 30)).is_err());
    let mut wrong = read(1, 10, 30);
    wrong.gateway_id = "other".into();
    assert!(feedback.apply(&wrong).is_err());
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Unknown);
    assert_eq!(feedback.apply(&read(1, 10, 101)).unwrap(), 0);
    assert_eq!(feedback.apply(&read(1, 11, 30)).unwrap(), 1);
    let snapshot = feedback.snapshot();
    assert_eq!(
        snapshot.devices[&id].observed,
        Some(Observation::Level(Percent::new(30).unwrap()))
    );
    assert_eq!(snapshot.devices[&id].pending_command, None);
    assert!(feedback.apply(&read(1, 9, 0)).is_err());
    let disconnected = GatewayRead {
        gateway_id: "simulation".into(),
        transport_epoch: 1,
        received_at_ms: 12,
        result: Receive::Disconnected {
            discarded_partial_bytes: 0,
        },
    };
    feedback.apply(&disconnected).unwrap();
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Stale);
    assert!(feedback.apply(&read(1, 13, 0)).is_err());
}

#[test]
fn explicit_invalidation_emits_one_quality_event_and_is_idempotent() {
    let config = Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 321, 1).unwrap();
    feedback.apply(&read(1, 10, 30)).unwrap();
    let cursor = feedback.snapshot().cursor;
    feedback.invalidate().unwrap();
    feedback.invalidate().unwrap();
    let events = feedback.events_since(cursor).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].change, homesense_state::Change::Quality);
    assert_eq!(events[0].state.quality, Quality::Stale);
    assert!(feedback.apply(&read(1, 11, 40)).is_err());
}

#[test]
fn silent_feedback_expires_at_boundary_once_and_fresh_feedback_recovers() {
    let config = Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 456, 1).unwrap();
    let id = DeviceId::new("simulation:light").unwrap();
    feedback.apply(&read(1, 10, 30)).unwrap();
    let cursor = feedback.snapshot().cursor;
    feedback.expire(19, 10).unwrap();
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Confirmed);
    feedback.expire(20, 10).unwrap();
    feedback.expire(21, 10).unwrap();
    assert_eq!(feedback.events_since(cursor).unwrap().len(), 1);
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Stale);
    assert!(feedback.expire(20, 10).is_err());
    assert_eq!(feedback.apply(&read(1, 22, 27)).unwrap(), 1);
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Confirmed);
    assert_eq!(
        feedback.snapshot().devices[&id].observed,
        Some(Observation::Level(Percent::new(27).unwrap()))
    );
    assert_eq!(feedback.snapshot().devices[&id].pending_command, None);
}

#[test]
fn recovery_preserves_stale_snapshot_and_rejects_prior_generation_feedback() {
    let config = Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    for eof in [false, true] {
        let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 789, 1).unwrap();
        let id = DeviceId::new("simulation:light").unwrap();
        feedback.apply(&read(1, 10, 30)).unwrap();
        assert!(feedback.resume(2, 11).is_err());
        if eof {
            feedback
                .apply(&GatewayRead {
                    gateway_id: "simulation".into(),
                    transport_epoch: 1,
                    received_at_ms: 11,
                    result: Receive::Disconnected {
                        discarded_partial_bytes: 2,
                    },
                })
                .unwrap();
        } else {
            feedback.invalidate().unwrap();
        }
        let cursor = feedback.snapshot().cursor;
        assert!(feedback.resume(1, 12).is_err());
        assert!(feedback.resume(2, 9).is_err());
        feedback.resume(2, 12).unwrap();
        assert_eq!(feedback.snapshot().cursor, cursor);
        assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Stale);
        assert!(feedback.apply(&read(1, 13, 100)).is_err());
        assert!(feedback.apply(&read(2, 11, 100)).is_err());
        assert_eq!(feedback.apply(&read(2, 13, 27)).unwrap(), 1);
        assert_eq!(
            feedback.snapshot().devices[&id].observed,
            Some(Observation::Level(Percent::new(27).unwrap()))
        );
        assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Confirmed);
        assert_eq!(feedback.events_since(cursor).unwrap().len(), 1);
    }
}

#[test]
fn keypad_recovery_requires_fresh_events_and_never_replays_old_press() {
    let mut document: hap::serde_json::Value = hap::serde_json::from_slice(include_bytes!(
        "../../../config/test/legacy-study/panel.json"
    ))
    .unwrap();
    document["gateways"][0]["modules"][0]["verification"] =
        hap::serde_json::json!({"status":"simulated"});
    for device in document["devices"].as_array_mut().unwrap() {
        device["enabled"] = hap::serde_json::json!(true);
    }
    let config = Configuration::parse(
        &hap::serde_json::to_vec(&document).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 987, 1).unwrap();
    let id = DeviceId::new("control4:63:key:1").unwrap();
    let button = |epoch, time, value| GatewayRead {
        gateway_id: "hopebeauty".into(),
        transport_epoch: epoch,
        received_at_ms: time,
        result: Receive::Frames {
            transport_epoch: epoch,
            frames: vec![RawFrame {
                address: 0x41,
                command: 0xD1,
                value,
            }],
        },
    };
    feedback.apply(&button(1, 10, 0xF1)).unwrap();
    feedback.invalidate().unwrap();
    let cursor = feedback.snapshot().cursor;
    feedback.resume(2, 11).unwrap();
    assert!(feedback.events_since(cursor).unwrap().is_empty());
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Stale);
    assert!(feedback.apply(&button(1, 12, 0xF1)).is_err());
    assert_eq!(feedback.apply(&button(2, 12, 0xF0)).unwrap(), 0);
    assert_eq!(feedback.snapshot().devices[&id].quality, Quality::Stale);
    feedback.apply(&button(2, 13, 0xF2)).unwrap();
    feedback.apply(&button(2, 14, 0xF1)).unwrap();
    feedback.apply(&button(2, 15, 0xF1)).unwrap();
    let events = feedback.events_since(cursor).unwrap();
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .all(|e| e.change == homesense_state::Change::Button)
    );
    assert_eq!(
        events[0].state.observed,
        Some(Observation::Button { pressed: false })
    );
    assert_eq!(
        events[1].state.observed,
        Some(Observation::Button { pressed: true })
    );
    assert_eq!(
        events[2].state.observed,
        Some(Observation::Button { pressed: true })
    );
}
