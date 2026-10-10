use hap::{
    HapType,
    server::{EventEmitter, ServerEvent},
    storage::Storage,
};
use homesense_hap::storage::IdentityStore;
use homesensed::simulation_hap::SimulationHap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[tokio::test]
async fn feedback_updates_native_reads_and_notifications_without_premature_acknowledgment() {
    let directory = std::env::temp_dir().join(format!("homesense-bridge-loop-{}", Uuid::new_v4()));
    let mut store = IdentityStore::open(&directory).await.unwrap();
    store.save_config(&hap::Config::default()).await.unwrap();
    let controller = Uuid::new_v4();
    let mut bridge = SimulationHap::new(
        include_bytes!("../../../config/simulation.json"),
        &store,
        controller,
        100,
    )
    .await
    .unwrap();
    let (binding, handle) = bridge.lights[0].clone();
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = output.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |event| {
        if let ServerEvent::CharacteristicValueChanged { aid, iid, value } = event {
            captured.lock().unwrap().push((*aid, *iid, value.clone()));
        }
        Box::pin(async {})
    }));
    {
        let mut light = handle.lock().await;
        light.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
            emitter,
        ))));
        for characteristic in light
            .get_mut_service(HapType::Lightbulb)
            .unwrap()
            .get_mut_characteristics()
        {
            if characteristic.get_id() == binding.on_iid
                || Some(characteristic.get_id()) == binding.brightness_iid
            {
                characteristic.set_event_notifications(Some(true));
            }
        }
    }
    let request = |level| {
        hap::serde_json::to_vec(&hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.brightness_iid.unwrap(),"value":level}]})).unwrap()
    };
    bridge
        .write_handler
        .write(controller, &request(30))
        .unwrap();
    {
        let mut light = handle.lock().await;
        assert!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .is_err()
        );
    }
    assert!(output.lock().unwrap().is_empty());
    bridge.advance(10).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 2);
    bridge
        .write_handler
        .write(controller, &request(60))
        .unwrap();
    {
        let mut light = handle.lock().await;
        assert_eq!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            hap::serde_json::json!(30)
        );
    }
    assert_eq!(output.lock().unwrap().len(), 2);
    bridge.advance(20).await.unwrap();
    assert_eq!(
        output.lock().unwrap().last().unwrap(),
        &(
            binding.aid,
            binding.brightness_iid.unwrap(),
            hap::serde_json::json!(60)
        )
    );
    assert_eq!(output.lock().unwrap().len(), 3);
    bridge.advance(21).await.unwrap();
    assert_eq!(output.lock().unwrap().len(), 3);
    bridge.advance(120).await.unwrap();
    {
        let mut light = handle.lock().await;
        assert!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .is_err()
        );
    }
    drop(bridge);
    drop(handle);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}
