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
    assert_eq!(output.lock().unwrap().len(), 5);
    assert!(
        output.lock().unwrap()[3..]
            .iter()
            .all(|event| event.2.is_null())
    );
    states
        .observe(&id, Observation::Level(Percent::new(73).unwrap()), 2, 6)
        .unwrap();
    for event in states.events_since(cursor).unwrap() {
        events.apply(&event, 6).await.unwrap();
    }
    assert_eq!(output.lock().unwrap().len(), 7);
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
    assert_eq!(output.lock().unwrap().len(), 4);
    assert!(
        output.lock().unwrap()[2..]
            .iter()
            .all(|event| event.2.is_null())
    );
    events.resynchronize(&states.snapshot(), 103).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn ordinary_snapshot_refresh_does_not_replay_old_brightness_after_target_change() {
    let (id, mut states, mut events, output, writes) = fixture().await;
    states
        .observe(&id, Observation::Level(Percent::new(60).unwrap()), 1, 1)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 1)
        .await
        .unwrap();
    output.lock().unwrap().clear();
    states
        .set_desired(
            &id,
            1,
            Command::SetLevel {
                level: Percent::new(40).unwrap(),
                transition: LevelTransition::Ramp,
            },
            2,
        )
        .unwrap();
    for now in 2..10 {
        events
            .refresh_snapshot(&states.snapshot(), now)
            .await
            .unwrap();
    }
    assert!(
        output.lock().unwrap().is_empty(),
        "unchanged 60 must not be replayed over target 40"
    );
    states
        .observe(&id, Observation::Level(Percent::new(40).unwrap()), 1, 10)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 10)
        .await
        .unwrap();
    assert_eq!(output.lock().unwrap().len(), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_ramp_suppresses_intermediate_events_but_releases_target_and_timeout() {
    let (id, mut states, mut events, output, _) = fixture().await;
    states
        .observe(&id, Observation::Level(Percent::new(50).unwrap()), 1, 1)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 1)
        .await
        .unwrap();
    output.lock().unwrap().clear();
    let transitions = events.transitions();
    let command = |n| Command::SetLevel {
        level: Percent::new(n).unwrap(),
        transition: LevelTransition::Ramp,
    };
    transitions.accepted(id.clone(), command(30), 2);
    for (now, level) in [(3, 47), (4, 36)] {
        states
            .observe(
                &id,
                Observation::Level(Percent::new(level).unwrap()),
                1,
                now,
            )
            .unwrap();
        events
            .refresh_snapshot(&states.snapshot(), now)
            .await
            .unwrap();
    }
    assert!(output.lock().unwrap().is_empty());
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 5)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 5)
        .await
        .unwrap();
    assert_eq!(
        output.lock().unwrap().last().unwrap().2,
        serde_json::json!(30)
    );
    output.lock().unwrap().clear();
    transitions.accepted(id.clone(), command(60), 6);
    states
        .observe(&id, Observation::Level(Percent::new(42).unwrap()), 1, 16006)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 16006)
        .await
        .unwrap();
    assert_eq!(
        output.lock().unwrap().last().unwrap().2,
        serde_json::json!(42)
    );
    output.lock().unwrap().clear();
    transitions.accepted(id.clone(), command(70), 16007);
    states.disconnect(2, 16008).unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 16008)
        .await
        .unwrap();
    assert!(output.lock().unwrap().iter().all(|v| v.2.is_null()));
    assert_eq!(output.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn latest_target_wins_off_cancels_and_timeout_reasserts_unchanged_actual() {
    let (id, mut states, mut events, output, _) = fixture().await;
    let command = |n| Command::SetLevel {
        level: Percent::new(n).unwrap(),
        transition: LevelTransition::Ramp,
    };
    states
        .observe(&id, Observation::Level(Percent::new(50).unwrap()), 1, 1)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 1)
        .await
        .unwrap();
    output.lock().unwrap().clear();
    let transitions = events.transitions();
    transitions.accepted(id.clone(), command(30), 2);
    transitions.accepted(id.clone(), command(40), 3);
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 4)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 4)
        .await
        .unwrap();
    assert!(output.lock().unwrap().is_empty());
    transitions.accepted(id.clone(), Command::Switch(false), 5);
    states
        .observe(&id, Observation::Level(Percent::new(0).unwrap()), 1, 6)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 6)
        .await
        .unwrap();
    assert_eq!(
        output.lock().unwrap().last().unwrap().2,
        serde_json::json!(0)
    );
    output.lock().unwrap().clear();
    transitions.accepted(id.clone(), command(70), 7);
    states
        .observe(&id, Observation::Level(Percent::new(0).unwrap()), 1, 16007)
        .unwrap();
    events
        .refresh_snapshot(&states.snapshot(), 16007)
        .await
        .unwrap();
    assert_eq!(
        output.lock().unwrap().last().unwrap().2,
        serde_json::json!(0),
        "timeout must correct optimistic target even when actual never changed"
    );
}

#[tokio::test]
async fn off_event_reasserts_retained_brightness_instead_of_zero() {
    let (id, mut states, mut events, output, _) = fixture().await;
    events.set_restore_levels(Arc::new(std::sync::RwLock::new([(id.clone(), Percent::new(50).unwrap())].into())));
    states.observe(&id, Observation::Level(Percent::new(50).unwrap()), 1, 1).unwrap();
    events.refresh_snapshot(&states.snapshot(), 1).await.unwrap();
    output.lock().unwrap().clear();
    states.observe(&id, Observation::Level(Percent::new(0).unwrap()), 1, 2).unwrap();
    events.refresh_snapshot(&states.snapshot(), 2).await.unwrap();
    assert_eq!(output.lock().unwrap().iter().map(|e| e.2.clone()).collect::<Vec<_>>(), vec![serde_json::json!(false), serde_json::json!(50)]);
    output.lock().unwrap().clear();
    states.disconnect(2, 3).unwrap();
    events.refresh_snapshot(&states.snapshot(), 3).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 2);
    assert!(output.lock().unwrap().iter().all(|e| e.2.is_null()));
}
