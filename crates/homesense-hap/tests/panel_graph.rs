use hap::{
    Config,
    characteristic::{HapCharacteristic, Perm},
    storage::Storage,
};
use homesense_core::{DeviceId, gesture::Event};
use homesense_hap::{
    panel_graph::{PanelGraph, PanelKeySpec, switch_event},
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};

fn spec(key: u8, name: &str) -> PanelKeySpec {
    PanelKeySpec {
        device: DeviceId::new("panel:test").unwrap(),
        key,
        name: name.into(),
    }
}

#[tokio::test]
async fn selected_keys_keep_identity_without_exposing_writes_or_other_keys() {
    let root = std::env::temp_dir().join(format!("homesense-panel-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let graph = PanelGraph::build(&store, vec![spec(6, "Six"), spec(2, "Two")])
        .await
        .unwrap();
    assert_eq!(graph.keys.len(), 2);
    assert_eq!(
        graph.bindings.iter().map(|b| b.key).collect::<Vec<_>>(),
        vec![2, 6]
    );
    assert_ne!(graph.bindings[0].aid, graph.bindings[1].aid);
    for button in graph.keys.values() {
        let perms = button
            .stateless_programmable_switch
            .programmable_switch_event
            .get_perms();
        assert!(perms.contains(&Perm::Events));
        assert!(!perms.contains(&Perm::PairedWrite));
        let json = serde_json::to_value(button).unwrap();
        assert_eq!(json["services"].as_array().unwrap().len(), 2);
    }
    let scenes = SceneGraph::build(
        &store,
        vec![SceneSpec {
            id: "panel:test".into(),
            name: "Scene".into(),
        }],
    )
    .await
    .unwrap();
    assert!(
        graph
            .bindings
            .iter()
            .all(|b| b.aid != scenes.bindings[0].aid)
    );
    assert!(
        PanelGraph::build(&store, vec![])
            .await
            .unwrap()
            .bindings
            .is_empty()
    );
    drop(store);
    let store = IdentityStore::open(&root).await.unwrap();
    let reopened = PanelGraph::build(&store, vec![spec(2, "Renamed"), spec(6, "Six")])
        .await
        .unwrap();
    assert_eq!(reopened.bindings, graph.bindings);
    for invalid in [
        vec![spec(0, "Bad")],
        vec![spec(7, "Bad")],
        vec![spec(1, "A"), spec(1, "B")],
        vec![spec(1, "\n")],
    ] {
        assert!(PanelGraph::build(&store, invalid).await.is_err());
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn gesture_edges_do_not_duplicate_click_or_hold_notifications() {
    assert_eq!(
        [Event::Pressed, Event::Released, Event::Click]
            .into_iter()
            .filter_map(switch_event)
            .collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        [
            Event::Pressed,
            Event::LongStart,
            Event::Released,
            Event::LongEnd
        ]
        .into_iter()
        .filter_map(switch_event)
        .collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(switch_event(Event::DoubleClick), Some(1));
    assert_eq!(switch_event(Event::Cancelled), None);
}
