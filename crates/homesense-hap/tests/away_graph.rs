use hap::storage::Storage;
use homesense_hap::{
    away_graph,
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};
#[tokio::test]
async fn away_operation_has_independent_persistent_identity_and_never_reuses_scene_keys() {
    let root = std::env::temp_dir().join(format!("away-identity-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&hap::Config::default()).await.unwrap();
    let initial = away_graph::build(&store).await.unwrap().0;
    let specs = vec![
        SceneSpec {
            id: "away-off".into(),
            name: "Synthetic scene".into(),
        },
        SceneSpec {
            id: "operation:away-off".into(),
            name: "Similar synthetic scene".into(),
        },
    ];
    let scenes = SceneGraph::build(&store, specs).await.unwrap();
    assert!(scenes.bindings.iter().all(|b| b.aid != initial.aid));
    let controller = uuid::Uuid::new_v4();
    store
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            [7; 32],
        ))
        .await
        .unwrap();
    drop(store);
    let store = IdentityStore::open(&root).await.unwrap();
    assert_eq!(away_graph::build(&store).await.unwrap().0, initial);
    assert!(store.controller_is_paired(controller).unwrap());
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
