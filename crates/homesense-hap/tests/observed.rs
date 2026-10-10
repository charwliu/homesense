use hap::{Config, characteristic::HapCharacteristic, storage::Storage};
use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    observed::{ObservedView, ReadError},
    storage::IdentityStore,
};
use homesense_state::StateStore;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use uuid::Uuid;
fn fixture() -> (DeviceId, StateStore) {
    let id = DeviceId::new("light").unwrap();
    let mut states = StateStore::new(1, 1, 1, 16).unwrap();
    states.register(id.clone(), ModuleKind::Dimmer).unwrap();
    (id, states)
}
#[test]
fn only_fresh_confirmed_matching_observations_are_readable() {
    let (id, mut states) = fixture();
    let snapshot = Arc::new(RwLock::new(states.snapshot()));
    let now = Arc::new(AtomicU64::new(10));
    let clock = now.clone();
    let view =
        ObservedView::new(snapshot.clone(), 100, move || clock.load(Ordering::SeqCst)).unwrap();
    assert_eq!(view.power(&id, true), Err(ReadError::Unavailable));
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 10)
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert_eq!(view.brightness(&id).unwrap().get(), 30);
    assert_eq!(view.power(&id, true), Ok(true));
    assert_eq!(view.power(&id, false), Err(ReadError::Unavailable));
    now.store(109, Ordering::SeqCst);
    assert!(view.brightness(&id).is_ok());
    now.store(110, Ordering::SeqCst);
    assert_eq!(view.brightness(&id), Err(ReadError::Unavailable));
    now.store(9, Ordering::SeqCst);
    assert_eq!(view.brightness(&id), Err(ReadError::Unavailable));
    now.store(11, Ordering::SeqCst);
    states.disconnect(2, 11).unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert_eq!(view.brightness(&id), Err(ReadError::Unavailable));
    let _guard = snapshot.write().unwrap();
    assert_eq!(view.brightness(&id), Err(ReadError::Unavailable));
}
#[tokio::test]
async fn actual_hap_reads_ignore_desired_and_reject_individual_writes() {
    let root = std::env::temp_dir().join(format!("homesense-observed-{}", Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let (id, mut states) = fixture();
    let snapshot = Arc::new(RwLock::new(states.snapshot()));
    let view = ObservedView::new(snapshot.clone(), 100, || 20).unwrap();
    let mut graph = LightGraph::build(
        &store,
        "HomeSense",
        vec![LightSpec {
            device: id.clone(),
            name: "Light".into(),
            dimmable: true,
            default_on_level: Some(Percent::new(30).unwrap()),
        }],
    )
    .await
    .unwrap();
    graph.install_observed_reads(view);
    let light = &mut graph.lights.get_mut(&id).unwrap().lightbulb;
    assert!(light.power_state.get_value().await.is_err());
    states
        .observe(&id, Observation::Level(Percent::new(30).unwrap()), 1, 10)
        .unwrap();
    states
        .set_desired(
            &id,
            1,
            Command::SetLevel {
                level: Percent::new(73).unwrap(),
                transition: LevelTransition::Immediate,
            },
            11,
        )
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert_eq!(
        light.power_state.get_value().await.unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        light
            .brightness
            .as_mut()
            .unwrap()
            .get_value()
            .await
            .unwrap(),
        serde_json::json!(30)
    );
    assert!(
        light
            .power_state
            .set_value(serde_json::json!(false))
            .await
            .is_err()
    );
    assert!(
        light
            .brightness
            .as_mut()
            .unwrap()
            .set_value(serde_json::json!(73))
            .await
            .is_err()
    );
    assert_eq!(
        light
            .brightness
            .as_mut()
            .unwrap()
            .get_value()
            .await
            .unwrap(),
        serde_json::json!(30)
    );
    states
        .observe(&id, Observation::Level(Percent::new(0).unwrap()), 1, 12)
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert_eq!(
        light.power_state.get_value().await.unwrap(),
        serde_json::json!(false)
    );
    states.disconnect(2, 13).unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert!(light.power_state.get_value().await.is_err());
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
