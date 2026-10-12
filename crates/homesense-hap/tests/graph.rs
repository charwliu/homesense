use hap::{Config, accessory::HapAccessory, characteristic::HapCharacteristic, storage::Storage};
use homesense_core::{DeviceId, Percent};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    requests::LightRequests,
    storage::IdentityStore,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use uuid::Uuid;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("homesense-graph-{}", Uuid::new_v4())))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn spec(id: &str, dimmable: bool) -> LightSpec {
    LightSpec {
        device: DeviceId::new(id).unwrap(),
        name: format!("Light {id}"),
        dimmable,
        default_on_level: dimmable.then(|| Percent::new(30).unwrap()),
    }
}
fn structure(accessory: &dyn HapAccessory) -> BTreeMap<String, u64> {
    let mut ids = BTreeMap::new();
    let mut used = BTreeSet::new();
    for service in accessory.get_services() {
        let key = serde_json::to_string(&service.get_type()).unwrap();
        assert!(used.insert(service.get_id()));
        ids.insert(key.clone(), service.get_id());
        for c in service.get_characteristics() {
            assert!(used.insert(c.get_id()));
            ids.insert(
                format!("{key}/{}", serde_json::to_string(&c.get_type()).unwrap()),
                c.get_id(),
            );
        }
    }
    ids
}
#[tokio::test]
async fn constructed_graph_survives_reorder_rename_extension_and_reopen() {
    let root = Root::new();
    let mut store = IdentityStore::open(&root.0).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let initial = LightGraph::build(&store, "HomeSense", vec![spec("b", false), spec("a", true)])
        .await
        .unwrap();
    assert_eq!(initial.bridge.get_id(), 1);
    let a = DeviceId::new("a").unwrap();
    let b = DeviceId::new("b").unwrap();
    let ids = structure(&initial.lights[&a]);
    let bridge_ids = structure(&initial.bridge);
    let aid = initial.lights[&a].get_id();
    let bid = initial.lights[&b].get_id();
    drop(store);
    let store = IdentityStore::open(&root.0).await.unwrap();
    let mut renamed = spec("a", true);
    renamed.name = "书房灯".into();
    let updated = LightGraph::build(
        &store,
        "HomeSense upstairs",
        vec![spec("c", false), renamed],
    )
    .await
    .unwrap();
    assert_eq!(updated.lights[&a].get_id(), aid);
    assert_eq!(structure(&updated.lights[&a]), ids);
    assert_eq!(structure(&updated.bridge), bridge_ids);
    assert!(updated.lights[&DeviceId::new("c").unwrap()].get_id() > bid);
    let restored = LightGraph::build(&store, "HomeSense", vec![spec("b", false)])
        .await
        .unwrap();
    assert_eq!(restored.lights[&b].get_id(), bid);
    let json = serde_json::to_value(&updated.lights[&a]).unwrap();
    assert!(json.to_string().contains("书房灯"));
}
#[tokio::test]
async fn bindings_address_actual_characteristics_and_only_expose_supported_light_capabilities() {
    let root = Root::new();
    let mut store = IdentityStore::open(&root.0).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let graph = LightGraph::build(&store, "HomeSense", vec![spec("a", true), spec("b", false)])
        .await
        .unwrap();
    let requests = LightRequests::new(graph.bindings.clone()).unwrap();
    for binding in &graph.bindings {
        let light = &graph.lights[&binding.device];
        assert_eq!(binding.aid, light.get_id());
        assert_eq!(binding.on_iid, light.lightbulb.power_state.get_id());
        assert!(
            light.lightbulb.hue.is_none()
                && light.lightbulb.saturation.is_none()
                && light.lightbulb.color_temperature.is_none()
        );
        assert_eq!(
            binding.brightness_iid,
            light.lightbulb.brightness.as_ref().map(|v| v.get_id())
        );
        let body=serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]})).unwrap();
        let batch = requests.normalize(Uuid::new_v4(), &body, |_| None).unwrap();
        assert_eq!(batch.intents.len(), 1);
        assert_eq!(batch.intents[0].device, binding.device);
        structure(light);
    }
}
#[tokio::test]
async fn invalid_graph_does_not_allocate_identifiers() {
    let root = Root::new();
    let mut store = IdentityStore::open(&root.0).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    assert!(
        LightGraph::build(&store, "HomeSense", vec![spec("a", true), spec("a", false)])
            .await
            .is_err()
    );
    let mut invalid = spec("b", false);
    invalid.default_on_level = Some(Percent::new(30).unwrap());
    assert!(
        LightGraph::build(&store, "HomeSense", vec![invalid])
            .await
            .is_err()
    );
    let db = rusqlite::Connection::open(root.0.join("identity.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM accessory_ids", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn ninety_six_lights_keep_unique_identity_and_capabilities_after_reopen() {
    let root = Root::new();
    let mut store = IdentityStore::open(&root.0).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let specs: Vec<_> = (0..96)
        .map(|index| spec(&format!("capacity:{index:02}"), index >= 56))
        .collect();
    let graph = LightGraph::build(&store, "Capacity fixture", specs.clone())
        .await
        .unwrap();
    assert_eq!(graph.lights.len(), 96);
    assert_eq!(graph.bindings.len(), 96);
    let mut aids = BTreeSet::from([graph.bridge.get_id()]);
    let mut addresses = BTreeSet::new();
    let mut original = BTreeMap::new();
    for binding in &graph.bindings {
        let light = &graph.lights[&binding.device];
        assert!(aids.insert(binding.aid));
        assert!(addresses.insert((binding.aid, binding.on_iid)));
        assert_eq!(light.get_id(), binding.aid);
        assert_eq!(light.lightbulb.power_state.get_id(), binding.on_iid);
        assert_eq!(light.lightbulb.brightness.as_ref().map(|c| c.get_id()), binding.brightness_iid);
        if let Some(iid) = binding.brightness_iid {
            assert!(addresses.insert((binding.aid, iid)));
        }
        original.insert(binding.device.clone(), (binding.aid, structure(light)));
    }
    assert_eq!(aids.len(), 97);
    assert_eq!(addresses.len(), 136);
    assert_eq!(graph.bindings.iter().filter(|b| b.brightness_iid.is_some()).count(), 40);
    assert_eq!(graph.bindings.iter().filter(|b| b.brightness_iid.is_none()).count(), 56);
    LightRequests::new(graph.bindings.clone()).unwrap();
    drop(graph);
    drop(store);
    let store = IdentityStore::open(&root.0).await.unwrap();
    let renamed = specs.into_iter().rev().map(|mut spec| {
        spec.name = format!("Renamed {}", spec.name);
        spec
    }).collect();
    let restored = LightGraph::build(&store, "Renamed capacity fixture", renamed)
        .await.unwrap();
    assert_eq!(restored.lights.len(), 96);
    for (device, light) in &restored.lights {
        assert_eq!((light.get_id(), structure(light)), original[device]);
    }
    LightRequests::new(restored.bindings).unwrap();
}
