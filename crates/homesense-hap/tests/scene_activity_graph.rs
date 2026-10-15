use hap::{
    characteristic::{HapCharacteristic, Perm},
    storage::Storage,
};
use homesense_hap::{
    scene_graph::{SceneActivityGraph, SceneGraph, SceneSpec},
    storage::IdentityStore,
};
#[tokio::test]
async fn activity_graph_is_read_only_from_construction_and_keeps_independent_stable_role_identity()
{
    let root = std::env::temp_dir().join(format!("scene-role-graph-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&hap::Config::default()).await.unwrap();
    let ids = ["plain", "plain:activity", "sact:plain", "scene:plain"];
    let specs = |names: bool| {
        ids.iter()
            .map(|id| SceneSpec {
                id: (*id).into(),
                name: if names {
                    format!("Renamed {id}")
                } else {
                    (*id).into()
                },
            })
            .collect()
    };
    let original = SceneGraph::build(&store, specs(false)).await.unwrap();
    let statuses = SceneActivityGraph::build(&store, specs(false))
        .await
        .unwrap();
    let mut aids = std::collections::BTreeSet::new();
    for binding in &original.bindings {
        assert!(aids.insert(binding.aid));
        assert!(
            original.scenes[&binding.scene]
                .switch
                .power_state
                .get_perms()
                .contains(&Perm::PairedWrite)
        );
    }
    for binding in &statuses.bindings {
        assert!(aids.insert(binding.aid));
        let perms = statuses.activities[&binding.scene]
            .switch
            .power_state
            .get_perms();
        assert_eq!(perms, vec![Perm::PairedRead, Perm::Events]);
    }
    let actions = SceneGraph::build(&store, specs(true)).await.unwrap();
    let active = SceneActivityGraph::build(&store, specs(true))
        .await
        .unwrap();
    assert_eq!(actions.bindings, original.bindings);
    assert_eq!(active.bindings, statuses.bindings);
    drop(store);
    let reopened = IdentityStore::open(&root).await.unwrap();
    assert_eq!(
        SceneActivityGraph::build(&reopened, specs(false))
            .await
            .unwrap()
            .bindings,
        statuses.bindings
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}
