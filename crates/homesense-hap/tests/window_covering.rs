use hap::{Config, storage::Storage};
use homesense_core::DeviceId;
use homesense_hap::{
    motor_graph::{MotorGraph, MotorSpec},
    storage::IdentityStore,
    window_covering_graph::WindowCoveringGraph,
    window_covering_requests::{WindowCommand, WindowCoveringRequests},
};

fn spec(name: &str) -> MotorSpec {
    MotorSpec {
        device: DeviceId::new("curtain").unwrap(),
        name: name.into(),
    }
}

#[tokio::test]
async fn native_covering_replaces_three_cards_without_reusing_action_identity() {
    let root = std::env::temp_dir().join(format!("covering-graph-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let actions = MotorGraph::build(&store, vec![spec("Curtain")])
        .await
        .unwrap();
    let old_aids: std::collections::BTreeSet<_> =
        actions.accessories.iter().map(|(b, _)| b.aid).collect();
    let first = WindowCoveringGraph::build(&store, vec![spec("Curtain")])
        .await
        .unwrap();
    assert_eq!(first.accessories.len(), 1);
    let binding = first.accessories[0].0.clone();
    assert!(!old_aids.contains(&binding.aid));
    let graph = serde_json::to_value(&first.accessories[0].1).unwrap();
    assert_eq!(
        graph["services"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["type"] == "8C")
            .count(),
        1
    );
    let covering = graph["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["type"] == "8C")
        .unwrap();
    let types: std::collections::BTreeSet<_> = covering["characteristics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["type"].as_str())
        .collect();
    for required in ["6D", "72", "7C", "6F"] {
        assert!(types.contains(required), "missing {required}");
    }
    drop(store);
    let store = IdentityStore::open(&root).await.unwrap();
    let renamed = WindowCoveringGraph::build(&store, vec![spec("Renamed")])
        .await
        .unwrap();
    assert_eq!(renamed.accessories[0].0, binding);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn covering_requests_validate_targets_and_hold_before_action() {
    let device = DeviceId::new("curtain").unwrap();
    let parser = WindowCoveringRequests::new(vec![
        homesense_hap::window_covering_graph::WindowCoveringBinding {
            device: device.clone(),
            aid: 2,
            current_iid: 3,
            state_iid: 4,
            target_iid: 5,
            hold_iid: 6,
        },
    ])
    .unwrap();
    let controller = uuid::Uuid::new_v4();
    let target = parser
        .normalize(
            controller,
            br#"{"characteristics":[{"aid":2,"iid":5,"value":37}]}"#,
        )
        .unwrap();
    assert_eq!(
        target.command,
        Some(WindowCommand::Target {
            device: device.clone(),
            percent: 37
        })
    );
    let hold = parser
        .normalize(
            controller,
            br#"{"characteristics":[{"aid":2,"iid":6,"value":true}]}"#,
        )
        .unwrap();
    assert_eq!(hold.command, Some(WindowCommand::Hold { device }));
    assert_eq!(
        parser
            .normalize(
                controller,
                br#"{"characteristics":[{"aid":2,"iid":3,"ev":true}]}"#
            )
            .unwrap()
            .subscriptions
            .len(),
        1
    );
    for body in [
        br#"{"characteristics":[{"aid":2,"iid":5,"value":101}]}"#.as_slice(),
        br#"{"characteristics":[{"aid":2,"iid":3,"value":50}]}"#,
        br#"{"characteristics":[{"aid":2,"iid":5,"value":20},{"aid":2,"iid":6,"value":true}]}"#,
        br#"{"characteristics":[{"aid":2,"iid":5,"value":20},{"aid":2,"iid":5,"value":80}]}"#,
    ] {
        assert!(parser.normalize(controller, body).is_err());
    }
}
