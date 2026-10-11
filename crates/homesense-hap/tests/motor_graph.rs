use hap::{Config, storage::Storage};
use homesense_core::DeviceId;
use homesense_hap::{
    identifiers::MotorOperation,
    motor_graph::{MotorGraph, MotorSpec},
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};
fn spec(name: &str) -> MotorSpec {
    MotorSpec {
        device: DeviceId::new("motor:open:close").unwrap(),
        name: name.into(),
    }
}
#[tokio::test]
async fn actions_have_distinct_stable_ids_and_no_position() {
    let root = std::env::temp_dir().join(format!("motor-graph-{}", uuid::Uuid::new_v4()));
    let mut store = IdentityStore::open(&root).await.unwrap();
    store.save_config(&Config::default()).await.unwrap();
    let first = MotorGraph::build(&store, vec![spec("Curtain")])
        .await
        .unwrap();
    assert_eq!(first.accessories.len(), 3);
    let bindings: Vec<_> = first.accessories.iter().map(|(b, _)| b.clone()).collect();
    let ids: std::collections::BTreeSet<_> = bindings.iter().map(|b| b.aid).collect();
    assert_eq!(ids.len(), 3);
    assert!(!ids.contains(&1));
    assert_eq!(
        bindings.iter().map(|b| b.operation).collect::<Vec<_>>(),
        vec![
            MotorOperation::Open,
            MotorOperation::Close,
            MotorOperation::Stop
        ]
    );
    for (_, accessory) in &first.accessories {
        let value = serde_json::to_value(accessory).unwrap();
        for service in value["services"].as_array().unwrap() {
            assert_ne!(service["type"], "8C");
            for characteristic in service["characteristics"].as_array().unwrap() {
                assert!(
                    !["6D", "7C"]
                        .iter()
                        .any(|kind| characteristic["type"] == *kind)
                );
            }
        }
    }
    let scenes = SceneGraph::build(
        &store,
        vec![SceneSpec {
            id: "motor:open:close".into(),
            name: "Scene".into(),
        }],
    )
    .await
    .unwrap();
    assert!(!ids.contains(&scenes.bindings[0].aid));
    MotorGraph::build(&store, vec![]).await.unwrap();
    drop(store);
    let store = IdentityStore::open(&root).await.unwrap();
    let renamed = MotorGraph::build(&store, vec![spec("Renamed")])
        .await
        .unwrap();
    assert_eq!(
        renamed
            .accessories
            .iter()
            .map(|(b, _)| b.clone())
            .collect::<Vec<_>>(),
        bindings
    );
    assert!(
        MotorGraph::build(&store, vec![spec("A"), spec("B")])
            .await
            .is_err()
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn motor_writes_preserve_caller_and_reject_multiple_intents_before_acceptance() {
    use homesense_hap::{motor_graph::MotorBinding, motor_requests::MotorRequests};
    let device = DeviceId::new("curtain").unwrap();
    let parser = MotorRequests::new(
        [
            MotorOperation::Open,
            MotorOperation::Close,
            MotorOperation::Stop,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, operation)| MotorBinding {
            device: device.clone(),
            operation,
            aid: index as u64 + 2,
            on_iid: 10,
        })
        .collect(),
    )
    .unwrap();
    let controller = uuid::Uuid::new_v4();
    let batch=parser.normalize(controller,br#"{"characteristics":[{"aid":2,"iid":10,"value":false},{"aid":4,"iid":10,"value":true,"ev":true}]}"#).unwrap();
    assert_eq!(batch.controller, controller);
    assert_eq!(batch.addressed.len(), 2);
    assert_eq!(batch.action.unwrap().operation, MotorOperation::Stop);
    assert_eq!(batch.subscriptions.len(), 1);
    let reset = parser
        .normalize(
            controller,
            br#"{"characteristics":[{"aid":2,"iid":10,"value":false}]}"#,
        )
        .unwrap();
    assert!(reset.action.is_none());
    assert_eq!(reset.addressed.len(), 1);
    for body in [
        br#"{"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":3,"iid":10,"value":true}]}"#
            .as_slice(),
        br#"{"characteristics":[{"aid":2,"iid":10,"value":2}]}"#,
        br#"{"characteristics":[{"aid":9,"iid":10,"value":true}]}"#,
    ] {
        assert!(parser.normalize(controller, body).is_err());
    }
}
