use hap::{
    Config, HapType, accessory::HapAccessory, characteristic::HapCharacteristic, storage::Storage,
};
use homesense_core::{Command, DeviceId, ModuleKind, Observation};
use homesense_hap::{
    events::ObservedEvents,
    graph::{LightGraph, LightSpec},
    observed::ObservedView,
    requests::LightRequests,
    storage::IdentityStore,
};
use homesense_state::StateStore;
use std::{
    collections::BTreeSet,
    sync::{Arc, RwLock},
};
use uuid::Uuid;
fn spec(id: &str) -> LightSpec {
    LightSpec {
        device: DeviceId::new(id).unwrap(),
        name: format!("Test {id}"),
        dimmable: false,
        default_on_level: None,
    }
}
type ServiceResources = Vec<(HapType, u64, Vec<(HapType, u64)>)>;
fn resources(a: &dyn HapAccessory) -> ServiceResources {
    a.get_services()
        .into_iter()
        .map(|s| {
            (
                s.get_type(),
                s.get_id(),
                s.get_characteristics()
                    .into_iter()
                    .map(|c| (c.get_type(), c.get_id()))
                    .collect(),
            )
        })
        .collect()
}
#[tokio::test]
async fn fan_conversion_preserves_accessory_pairing_other_ids_and_reversible_old_service() {
    let root = std::env::temp_dir().join(format!("homesense-fan-{}", Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let fan = DeviceId::new("exhaust").unwrap();
    let lamp = DeviceId::new("lamp").unwrap();
    let old = LightGraph::build(&store, "Test", vec![spec("exhaust"), spec("lamp")])
        .await
        .unwrap();
    let old_fan = resources(&old.lights[&fan]);
    let old_lamp = resources(&old.lights[&lamp]);
    let aid = old.lights[&fan].get_id();
    let old_on = old
        .bindings
        .iter()
        .find(|b| b.device == fan)
        .unwrap()
        .on_iid;
    let controller = Uuid::new_v4();
    store
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            [8; 32],
        ))
        .await
        .unwrap();
    let before = store.load_config().await.unwrap();
    drop(old);
    let mut graph = LightGraph::build_with_fans(
        &store,
        "Test",
        vec![spec("lamp"), spec("exhaust")],
        [fan.clone()].into(),
    )
    .await
    .unwrap();
    assert_eq!(graph.fans[&fan].get_id(), aid);
    assert!(!graph.lights.contains_key(&fan));
    assert_eq!(resources(&graph.lights[&lamp]), old_lamp);
    assert_eq!(
        graph
            .fans
            .get_mut(&fan)
            .unwrap()
            .accessory_information
            .serial_number
            .get_value()
            .await
            .unwrap(),
        format!("HS-{aid}")
    );
    let f = &graph.fans[&fan].fan;
    assert!(f.rotation_speed.is_none() && f.rotation_direction.is_none() && f.name.is_none());
    assert!(graph.fans[&fan].get_service(HapType::Fan).is_some());
    assert!(graph.fans[&fan].get_service(HapType::Lightbulb).is_none());
    let binding = graph.bindings.iter().find(|b| b.device == fan).unwrap();
    assert_ne!(binding.on_iid, old_on);
    assert!(binding.brightness_iid.is_none());
    let new_ids = resources(&graph.fans[&fan]);
    drop(graph);
    let again = LightGraph::build_with_fans(
        &store,
        "Test",
        vec![spec("exhaust"), spec("lamp")],
        [fan.clone()].into(),
    )
    .await
    .unwrap();
    assert_eq!(resources(&again.fans[&fan]), new_ids);
    drop(again);
    let rollback = LightGraph::build(&store, "Test", vec![spec("exhaust"), spec("lamp")])
        .await
        .unwrap();
    assert_eq!(resources(&rollback.lights[&fan]), old_fan);
    let after = store.load_config().await.unwrap();
    assert_eq!(before.device_id, after.device_id);
    assert_eq!(
        before.device_ed25519_keypair.to_bytes(),
        after.device_ed25519_keypair.to_bytes()
    );
    assert_eq!(
        store.load_pairing(&controller).await.unwrap(),
        hap::pairing::Pairing::new(controller, hap::pairing::Permissions::Admin, [8; 32])
    );
    drop(rollback);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn fan_reads_and_events_are_real_switch_feedback_and_grouped_writes_only() {
    let root = std::env::temp_dir().join(format!("homesense-fan-observed-{}", Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let id = DeviceId::new("exhaust").unwrap();
    let mut states = StateStore::new(1, 1, 1, 16).unwrap();
    states.register(id.clone(), ModuleKind::Switch).unwrap();
    let snapshot = Arc::new(RwLock::new(states.snapshot()));
    let now = Arc::new(std::sync::atomic::AtomicU64::new(20));
    let clock = now.clone();
    let view = ObservedView::new(snapshot.clone(), 100, move || {
        clock.load(std::sync::atomic::Ordering::SeqCst)
    })
    .unwrap();
    let mut graph =
        LightGraph::build_with_fans(&store, "Test", vec![spec("exhaust")], [id.clone()].into())
            .await
            .unwrap();
    graph.install_observed_reads(view);
    let binding = graph.bindings[0].clone();
    let fan = &mut graph.fans.get_mut(&id).unwrap().fan;
    assert!(fan.power_state.get_value().await.is_err());
    states
        .set_desired(&id, 1, Command::Switch(true), 1)
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert!(fan.power_state.get_value().await.is_err());
    states
        .observe(&id, Observation::Switch(true), 1, 10)
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    assert_eq!(fan.power_state.get_value().await.unwrap(), true);
    assert!(fan.power_state.set_value(false.into()).await.is_err());
    let requests = LightRequests::new(graph.bindings.clone()).unwrap();
    let controller = Uuid::new_v4();
    let body=serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":false}]})).unwrap();
    let batch = requests.normalize(controller, &body, |_| None).unwrap();
    assert_eq!(batch.intents[0].command, Command::Switch(false));
    let subscription=serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"ev":true}]})).unwrap();
    assert!(
        requests
            .normalize(controller, &subscription, |_| None)
            .unwrap()
            .intents
            .is_empty()
    );
    let handle: hap::server::AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(
        graph.fans.remove(&id).unwrap(),
    )));
    let mut projection = ObservedEvents::new(vec![(binding.clone(), handle.clone())], 100)
        .await
        .unwrap();
    projection
        .resynchronize(&states.snapshot(), 20)
        .await
        .unwrap();
    now.store(22, std::sync::atomic::Ordering::SeqCst);
    states
        .observe(&id, Observation::Switch(false), 1, 21)
        .unwrap();
    *snapshot.write().unwrap() = states.snapshot();
    projection
        .refresh_snapshot(&states.snapshot(), 22)
        .await
        .unwrap();
    assert_eq!(
        handle
            .lock()
            .await
            .get_mut_service(HapType::Fan)
            .unwrap()
            .get_mut_characteristic(HapType::PowerState)
            .unwrap()
            .get_value()
            .await
            .unwrap(),
        false
    );
    let mut bad = binding;
    bad.brightness_iid = Some(bad.on_iid);
    assert!(
        ObservedEvents::new(vec![(bad, handle.clone())], 100)
            .await
            .is_err()
    );
    now.store(121, std::sync::atomic::Ordering::SeqCst);
    projection
        .refresh_snapshot(&states.snapshot(), 121)
        .await
        .unwrap();
    assert!(
        handle
            .lock()
            .await
            .get_mut_service(HapType::Fan)
            .unwrap()
            .get_mut_characteristic(HapType::PowerState)
            .unwrap()
            .get_value()
            .await
            .is_err()
    );
    drop(projection);
    drop(handle);
    drop(graph);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn invalid_fan_specs_are_rejected_before_identity_allocation() {
    let root = std::env::temp_dir().join(format!("homesense-fan-invalid-{}", Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let mut dimmer = spec("dimmer");
    dimmer.dimmable = true;
    assert!(
        LightGraph::build_with_fans(
            &store,
            "Test",
            vec![dimmer],
            [DeviceId::new("dimmer").unwrap()].into()
        )
        .await
        .is_err()
    );
    assert!(
        LightGraph::build_with_fans(
            &store,
            "Test",
            vec![spec("lamp")],
            [DeviceId::new("missing").unwrap()].into()
        )
        .await
        .is_err()
    );
    let graph = LightGraph::build_with_fans(
        &store,
        "Test",
        vec![spec("fan")],
        BTreeSet::from([DeviceId::new("fan").unwrap()]),
    )
    .await
    .unwrap();
    assert_eq!(graph.bindings[0].aid, 2);
    drop(graph);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
