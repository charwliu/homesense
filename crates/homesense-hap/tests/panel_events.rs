use hap::{
    accessory::{
        AccessoryInformation, HapAccessorySetup,
        stateless_programmable_switch::StatelessProgrammableSwitchAccessory,
    },
    characteristic::{HapCharacteristic, power_state::PowerStateCharacteristic},
    server::{EventEmitter, ServerEvent},
};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn repeated_clicks_notify_but_reads_never_replay_a_click() {
    let mut panel =
        StatelessProgrammableSwitchAccessory::new(2, AccessoryInformation::default()).unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |event| {
        if let ServerEvent::CharacteristicValueChanged { aid, iid, value } = event {
            output.lock().unwrap().push((*aid, *iid, value.clone()));
        }
        Box::pin(async {})
    }));
    panel.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
        emitter,
    ))));
    let event = &mut panel
        .stateless_programmable_switch
        .programmable_switch_event;
    let iid = event.get_id();
    assert_eq!(event.get_value().await.unwrap(), serde_json::Value::Null);
    assert!(serde_json::to_value(&*event).unwrap()["value"].is_null());
    assert!(received.lock().unwrap().is_empty());
    for value in [0, 0, 1, 2, 2] {
        event
            .set_observed_value(serde_json::json!(value))
            .await
            .unwrap();
        assert_eq!(event.get_value().await.unwrap(), serde_json::Value::Null);
        assert!(serde_json::to_value(&*event).unwrap()["value"].is_null());
    }
    assert_eq!(
        *received.lock().unwrap(),
        [0, 0, 1, 2, 2].map(|v| (2, iid, serde_json::json!(v)))
    );
    // The event-only exception must not erase ordinary readable switch state.
    let mut power = PowerStateCharacteristic::new(3, 4);
    power
        .set_observed_value(serde_json::json!(true))
        .await
        .unwrap();
    assert_eq!(power.get_value().await.unwrap(), serde_json::json!(true));
    assert_eq!(serde_json::to_value(power).unwrap()["value"], true);
}
