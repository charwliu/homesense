use hap::{
    accessory::{
        AccessoryInformation, HapAccessorySetup,
        stateless_programmable_switch::StatelessProgrammableSwitchAccessory,
    },
    characteristic::HapCharacteristic,
    server::{AccessoryHandle, EventEmitter, ServerEvent},
};
use homesense_core::{DeviceId, ModuleKind, Observation, gesture::Settings};
use homesense_hap::{
    panel_events::{PanelEvents, PublishError},
    panel_graph::PanelKeyBinding,
    panel_input::Generation,
};
use homesense_state::{Change, Cursor, DeviceState, Event, Quality};
use std::sync::{Arc, Mutex};
fn generation() -> Generation {
    Generation {
        configuration: 1,
        transport: 1,
    }
}
fn events() -> Vec<Event> {
    [
        (1, 10, true),
        (2, 60, false),
        (3, 110, true),
        (4, 160, false),
    ]
    .into_iter()
    .map(|(sequence, at, pressed)| Event {
        cursor: Cursor {
            boot_id: 1,
            sequence,
        },
        device_id: DeviceId::new("key").unwrap(),
        change: Change::Button,
        state: DeviceState {
            kind: ModuleKind::Keypad,
            desired: None,
            observed: Some(Observation::Button { pressed }),
            quality: Quality::Confirmed,
            observed_at_ms: Some(at),
            last_command_id: None,
            pending_command: None,
            version: sequence,
        },
    })
    .collect()
}
async fn fixture_with_pause(
    pause: bool,
) -> (
    PanelEvents,
    AccessoryHandle,
    Arc<Mutex<Vec<serde_json::Value>>>,
) {
    let mut accessory =
        StatelessProgrammableSwitchAccessory::new(2, AccessoryInformation::default()).unwrap();
    let binding = PanelKeyBinding {
        device: DeviceId::new("key").unwrap(),
        key: 2,
        aid: 2,
        event_iid: accessory
            .stateless_programmable_switch
            .programmable_switch_event
            .get_id(),
    };
    let output = Arc::new(Mutex::new(Vec::new()));
    let values = output.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |event| {
        if let ServerEvent::CharacteristicValueChanged { value, .. } = event {
            values.lock().unwrap().push(value.clone());
        }
        Box::pin(async move {
            if pause {
                std::future::pending::<()>().await;
            }
        })
    }));
    accessory.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
        emitter,
    ))));
    let handle: AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
    let delivery = PanelEvents::new(
        vec![(binding, Settings::default(), handle.clone())],
        generation(),
        Cursor {
            boot_id: 1,
            sequence: 0,
        },
        0,
        1000,
    )
    .await
    .unwrap();
    (delivery, handle, output)
}
#[tokio::test]
async fn live_edges_reach_emitter_once_without_snapshot_replay() {
    let (mut delivery, _, output) = fixture_with_pause(false).await;
    assert!(output.lock().unwrap().is_empty());
    assert_eq!(
        delivery
            .publish(generation(), &events(), 160)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        *output.lock().unwrap(),
        vec![serde_json::json!(0), serde_json::json!(0)]
    );
    assert_eq!(
        delivery
            .publish(generation(), &events(), 170)
            .await
            .unwrap(),
        0
    );
    assert_eq!(output.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn cancelled_delivery_cannot_retry_ambiguous_batch() {
    let (mut delivery, handle, output) = fixture_with_pause(false).await;
    let guard = handle.lock().await;
    let events = events();
    let future = delivery.publish(generation(), &events, 160);
    // Poll until it reaches the held accessory lock, then cancel deterministically.
    let mut future = Box::pin(future);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending()))
            .await
    );
    drop(future);
    drop(guard);
    assert_eq!(
        delivery.publish(generation(), &events, 170).await,
        Err(PublishError::Stopped)
    );
    assert!(output.lock().unwrap().is_empty());
}
#[tokio::test]
async fn changed_target_is_rejected_and_stays_stopped() {
    let (mut delivery, handle, output) = fixture_with_pause(false).await;
    handle.lock().await.set_id(9);
    assert_eq!(
        delivery.publish(generation(), &events(), 160).await,
        Err(PublishError::InvalidBinding)
    );
    assert_eq!(
        delivery.publish(generation(), &[], 170).await,
        Err(PublishError::Stopped)
    );
    assert!(output.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_after_first_notification_does_not_replay_or_finish_the_batch() {
    let (mut delivery, _, output) = fixture_with_pause(true).await;
    let events = events();
    let mut future = Box::pin(delivery.publish(generation(), &events, 160));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending()))
            .await
    );
    assert_eq!(output.lock().unwrap().len(), 1);
    drop(future);
    assert_eq!(
        delivery.publish(generation(), &events, 170).await,
        Err(PublishError::Stopped)
    );
    assert_eq!(output.lock().unwrap().len(), 1);
}
