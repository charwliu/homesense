use crate::{
    admission::{Admission, Options},
    runtime_hap::RuntimeHap,
    unified_hap::UnifiedHap,
};
use hap::{
    HapType,
    server::{EventEmitter, ServerEvent},
    storage::Storage,
};
use homesense_core::{DeviceId, ExecutionContext, ModuleKind, Observation, Percent};
use homesense_hap::storage::IdentityStore;
use homesense_state::{Quality, StateStore};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

async fn check(combined: bool, relay: bool, time: &'static AtomicU64) {
    time.store(0, Ordering::Release);
    let dir = std::env::temp_dir().join(format!("hap-idle-expiry-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("runtime.sqlite");
    let mut doc = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"fixture","endpoint":"127.0.0.1:1","modules":[{"id":"dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]});
    if relay {
        doc["gateways"][0]["modules"][0]["profile"] = serde_json::json!("r8i8_din_pro_v2_7_7");
    }
    if combined {
        doc["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"id":"switch","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
        doc["devices"][0]["supply"] = serde_json::json!({"module_id":"switch","channel":1});
        doc["devices"][0]["default_on_level"] = 30.into();
        doc["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","name":"Synthetic supply","module_id":"switch","channel":1,"enabled":true}));
    }
    let config = homesense_storage::Store::open(&path)
        .unwrap()
        .publish_configuration(
            None,
            &serde_json::to_vec(&doc).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let admission = Admission::open_with_test_time(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
        time,
    )
    .await
    .unwrap();
    let mut identity = IdentityStore::open(dir.join("identity")).await.unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let id = DeviceId::new("light").unwrap();
    let supply = DeviceId::new("supply").unwrap();
    let mut states = StateStore::new(
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
        2,
        32,
    )
    .unwrap();
    let kind = if relay {
        ModuleKind::Switch
    } else {
        ModuleKind::Dimmer
    };
    let observed = if relay {
        Observation::Switch(true)
    } else {
        Observation::Level(Percent::new(30).unwrap())
    };
    let characteristics = if relay {
        vec![HapType::PowerState]
    } else {
        vec![HapType::PowerState, HapType::Brightness]
    };
    let unavailable = vec![serde_json::Value::Null; characteristics.len()];
    let mut recovered = unavailable.clone();
    recovered.push(serde_json::json!(true));
    if !relay {
        recovered.push(serde_json::json!(30));
    }
    states.register(id.clone(), kind).unwrap();
    if combined {
        states.register(supply.clone(), ModuleKind::Switch).unwrap();
        states
            .observe(&supply, Observation::Switch(true), 1, 0)
            .unwrap();
        time.store(4900, Ordering::Release);
    }
    states
        .observe(&id, observed, 1, if combined { 4900 } else { 0 })
        .unwrap();
    let original = states.snapshot();
    let graph = RuntimeHap::new(
        &config,
        original.clone(),
        &identity,
        admission.clone(),
        5000,
    )
    .await
    .unwrap();
    let handle = graph.lights[0].1.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let out = events.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |e| {
        if let ServerEvent::CharacteristicValueChanged { value, .. } = e {
            out.lock().unwrap().push(value.clone());
        }
        Box::pin(async {})
    }));
    {
        let mut a = handle.lock().await;
        a.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
            emitter,
        ))));
        let service = a.get_mut_service(HapType::Lightbulb).unwrap();
        for &kind in &characteristics {
            service
                .get_mut_characteristic(kind)
                .unwrap()
                .set_event_notifications(Some(true));
        }
        assert_eq!(
            service
                .get_mut_characteristic(HapType::PowerState)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            true
        );
    }
    let (send, receive) = tokio::sync::watch::channel(original.clone());
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let mut graph = UnifiedHap {
        lights: graph,
        scenes: None,
        motors: None,
        away: None,
    };
    let task = tokio::spawn(async move { graph.project(receive, stopping).await });
    time.store(4999, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(events.lock().unwrap().is_empty());
    time.store(5000, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(2), async {
        while events.lock().unwrap().len() < unavailable.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle light expiration must emit unavailable without a physical update");
    assert_eq!(*events.lock().unwrap(), unavailable);
    assert_eq!(states.snapshot(), original);
    assert_eq!(original.devices[&id].quality, Quality::Confirmed);
    {
        let mut a = handle.lock().await;
        let service = a.get_mut_service(HapType::Lightbulb).unwrap();
        for &kind in &characteristics {
            assert!(
                service
                    .get_mut_characteristic(kind)
                    .unwrap()
                    .get_value()
                    .await
                    .is_err()
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert_eq!(
        events.lock().unwrap().len(),
        unavailable.len(),
        "unavailable is not repeatedly emitted"
    );
    states.observe(&id, observed, 1, 5000).unwrap();
    if combined {
        states
            .observe(&supply, Observation::Switch(true), 1, 5000)
            .unwrap();
    }
    send.send_replace(states.snapshot());
    tokio::time::timeout(Duration::from_secs(2), async {
        while events.lock().unwrap().len() < recovered.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*events.lock().unwrap(), recovered);
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert_eq!(
        events.lock().unwrap().len(),
        recovered.len(),
        "unchanged fresh values are not repeated"
    );
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        db.query_row("select count(*) from command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    admission.shutdown().await.unwrap();
    drop(identity);
    drop(handle);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn idle_light_publishes_expiration_and_unchanged_feedback_recovery() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    check(false, false, &TIME).await;
}
#[tokio::test]
async fn idle_combined_light_expires_on_older_supply_feedback() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    check(true, false, &TIME).await;
}

#[tokio::test]
async fn idle_relay_publishes_expiration_and_unchanged_feedback_recovery() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    check(false, true, &TIME).await;
}
