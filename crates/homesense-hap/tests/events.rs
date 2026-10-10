use hap::{
    accessory::{AccessoryInformation, HapAccessorySetup, lightbulb::LightbulbAccessory},
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
    server::{AccessoryHandle, EventEmitter, ServerEvent},
};
use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
use homesense_hap::{
    events::{ObservedEvents, ProjectionError},
    requests::LightBinding,
};
use homesense_state::StateStore;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
async fn fixture() -> (
    DeviceId,
    StateStore,
    ObservedEvents,
    Arc<Mutex<Vec<(u64, u64, serde_json::Value)>>>,
    Arc<AtomicUsize>,
) {
    let id = DeviceId::new("light").unwrap();
    let mut states = StateStore::new(1, 1, 1, 32).unwrap();
    states.register(id.clone(), ModuleKind::Dimmer).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let values = output.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |event| {
        if let ServerEvent::CharacteristicValueChanged { aid, iid, value } = event {
            values.lock().unwrap().push((*aid, *iid, value.clone()));
        }
        Box::pin(async {})
    }));
    let mut light = LightbulbAccessory::new(8, AccessoryInformation::default()).unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    light
        .lightbulb
        .power_state
        .on_update(Some(move |_: &bool, _: &bool| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
    light
        .lightbulb
        .power_state
        .set_event_notifications(Some(true));
    light
        .lightbulb
        .brightness
        .as_mut()
        .unwrap()
        .set_event_notifications(Some(true));
    light.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
        emitter,
    ))));
    let binding = LightBinding {
        device: id.clone(),
        aid: 8,
        on_iid: light.lightbulb.power_state.get_id(),
        brightness_iid: Some(light.lightbulb.brightness.as_ref().unwrap().get_id()),
        default_on_level: None,
    };
    let handle: AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(light)));
    let mut events = ObservedEvents::new(vec![(binding, handle)], 100)
        .await
        .unwrap();
    events.resynchronize(&states.snapshot(), 0).await.unwrap();
    (id, states, events, output, writes)
}
#[tokio::test]
async fn only_changed_confirmed_feedback_emits_without_control_callbacks() {
    let (id, mut states, mut events, output, writes) = fixture().await;
    let mut cursor = states.cursor();
    states
        .set_desired(
            &id,
            1,
            Command::SetLevel {
                level: Percent::new(73).unwrap(),
                transition: LevelTransition::Immediate,
            },
            1,
        )
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 1).await.unwrap();
    }
    cursor = states.cursor();
    assert!(output.lock().unwrap().is_empty());
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 2)
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 2).await.unwrap();
    }
    cursor = states.cursor();
    assert_eq!(
        output
            .lock()
            .unwrap()
            .iter()
            .map(|v| v.2.clone())
            .collect::<Vec<_>>(),
        vec![serde_json::json!(true), serde_json::json!(30)]
    );
    assert!(output.lock().unwrap().iter().all(|v| v.0 == 8));
    states
        .observe(&id, Observation::Level(Percent::new(73).unwrap()), 1, 3)
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 3).await.unwrap();
    }
    cursor = states.cursor();
    assert_eq!(output.lock().unwrap().len(), 3);
    states
        .observe(&id, Observation::Level(Percent::new(73).unwrap()), 1, 4)
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 4).await.unwrap();
    }
    cursor = states.cursor();
    assert_eq!(output.lock().unwrap().len(), 3);
    states.disconnect(2, 5).unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 5).await.unwrap();
    }
    cursor = states.cursor();
    assert_eq!(output.lock().unwrap().len(), 3);
    states
        .observe(&id, Observation::Level(Percent::new(73).unwrap()), 2, 6)
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 6).await.unwrap();
    }
    assert_eq!(output.lock().unwrap().len(), 5);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn gaps_and_duplicate_events_require_snapshot_resynchronization() {
    let (id, mut states, mut events, output, _) = fixture().await;
    let cursor = states.cursor();
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 1)
        .unwrap();
    states
        .observe(&id, Observation::Level(Percent::new(40).unwrap()), 1, 2)
        .unwrap();
    let pending = states.events_since(cursor).unwrap();
    assert_eq!(
        events.apply(&pending[1], 2).await,
        Err(ProjectionError::ResyncRequired)
    );
    assert!(output.lock().unwrap().is_empty());
    assert_eq!(
        events.apply(&pending[0], 2).await,
        Err(ProjectionError::ResyncRequired)
    );
    events.resynchronize(&states.snapshot(), 2).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 2);
    assert_eq!(
        events.apply(&pending[1], 2).await,
        Err(ProjectionError::ResyncRequired)
    );
    events.resynchronize(&states.snapshot(), 102).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 2);
}
