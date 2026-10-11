use hap::{Config, accessory::HapAccessory, characteristic::HapCharacteristic, storage::Storage};
use homesense_core::DeviceId;
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};
fn spec(id: &str, name: &str) -> SceneSpec {
    SceneSpec {
        id: id.into(),
        name: name.into(),
    }
}
#[tokio::test]
async fn scene_identity_survives_rename_removal_reopen_and_does_not_alias_lights() {
    let root = std::env::temp_dir().join(format!("homesense-scene-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let lights = LightGraph::build(
        &store,
        "Test",
        vec![LightSpec {
            device: DeviceId::new("reading").unwrap(),
            name: "Light".into(),
            dimmable: false,
            default_on_level: None,
        }],
    )
    .await
    .unwrap();
    let first = SceneGraph::build(
        &store,
        vec![spec("reading", "Reading"), spec("daily", "Daily")],
    )
    .await
    .unwrap();
    let binding = first
        .bindings
        .iter()
        .find(|b| b.scene == "reading")
        .unwrap()
        .clone();
    assert_ne!(binding.aid, lights.bindings[0].aid);
    assert_eq!(binding.aid, first.scenes["reading"].get_id());
    assert_eq!(
        binding.on_iid,
        first.scenes["reading"].switch.power_state.get_id()
    );
    SceneGraph::build(&store, vec![]).await.unwrap();
    drop(store);
    let store = IdentityStore::open(&root).await.unwrap();
    let again = SceneGraph::build(&store, vec![spec("reading", "Renamed")])
        .await
        .unwrap();
    assert_eq!(again.bindings, vec![binding]);
    assert!(
        SceneGraph::build(&store, vec![spec("x", "X"), spec("x", "Y")])
            .await
            .is_err()
    );
    assert!(
        SceneGraph::build(&store, vec![spec("\n", "X")])
            .await
            .is_err()
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
