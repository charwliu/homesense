use homesense_core::{DeviceId, ExecutionContext};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    management_devices::{DeviceAction, prepare, snapshot},
};
use uuid::Uuid;

#[tokio::test]
async fn administrative_scope_lists_logical_lights_without_exposing_supply_relays() {
    let root = std::env::temp_dir().join(format!("management-combined-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    raw["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","name":"Supply","gateway":"simulation","address":"03","channel":1,"enabled":true}));
    raw["devices"][0]["supply_relay"] = "supply".into();
    raw["devices"][0]["default_on_level"] = 50.into();
    let bytes = serde_json::to_vec(&raw).unwrap();
    let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(None, &bytes, rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let config =
        homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let generation = admission.task_generation().await.unwrap();
    let ids = config.devices().keys().cloned().collect();
    let view = snapshot(&admission, controller, generation.clone(), ids, &feedback)
        .await
        .unwrap();
    assert_eq!(view.devices.len(), 1);
    assert_eq!(view.devices[0].id.as_str(), "simulation:light");
    assert!(view.devices[0].state.observed.is_none());
    let supply = DeviceId::new("supply").unwrap();
    assert!(
        snapshot(
            &admission,
            controller,
            generation.clone(),
            vec![supply.clone()],
            &feedback
        )
        .await
        .unwrap()
        .devices
        .is_empty()
    );
    assert!(
        prepare(
            &admission,
            controller,
            generation.clone(),
            supply,
            DeviceAction::Power { on: false },
            &feedback,
            5000
        )
        .await
        .is_err()
    );
    assert!(
        snapshot(
            &admission,
            controller,
            generation,
            vec![DeviceId::new("missing").unwrap()],
            &feedback
        )
        .await
        .is_err()
    );
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
