//! 96 logical outputs, real encrypted subscriptions, synthetic physical feedback.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use homesense_core::{ExecutionContext, Observation, Percent};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;
#[tokio::test]
async fn ninety_six_outputs_deliver_all_subscribed_feedback() {
    let root = std::env::temp_dir().join(format!("homesense-capacity-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut modules = vec![];
    let mut devices = vec![];
    for module in 0..12 {
        let address = format!("{:02X}", module + 1);
        modules.push(serde_json::json!({"address":address,"profile":if module<7 {"r8i8_din_pro_v2_7_7"}else{"gt_d8_rl_v2_2_6_2"},"verification":{"status":"simulated"}}));
        for channel in 1..=8 {
            let index = module * 8 + channel - 1;
            devices.push(serde_json::json!({"id":format!("lamp:{index:02}"),"name":format!("Lamp {index:02}"),"gateway":"synthetic","address":address,"channel":channel,"enabled":true}));
        }
    }
    let bytes=serde_json::to_vec(&serde_json::json!({"schema_version":1,"revision":1,"manual_degraded_enabled":false,"gateways":[{"id":"synthetic","endpoint":"127.0.0.1:50007","modules":modules}],"devices":devices})).unwrap();
    let mut store = homesense_storage::Store::open(root.join("runtime.sqlite")).unwrap();
    let config = store
        .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
        .unwrap();
    drop(store);
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[37; 32]);
    let admission = homesensed::admission::Admission::open(
        root.join("runtime.sqlite"),
        homesensed::admission::Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    let mut snapshot = homesensed::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    for (index, state) in snapshot.devices.values_mut().enumerate() {
        state.quality = homesense_state::Quality::Confirmed;
        state.observed_at_ms = Some(0);
        state.observed = Some(if index < 56 {
            Observation::Switch(false)
        } else {
            Observation::Level(Percent::new(0).unwrap())
        });
    }
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mut identity = homesense_hap::storage::IdentityStore::open(root.join("identity"))
        .await
        .unwrap();
    identity
        .save_config(&hap::Config {
            host: address.ip(),
            port: address.port(),
            name: "Capacity".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    identity.initialize_aid_cache().await.unwrap();
    let mut graph = homesensed::runtime_hap::RuntimeHap::new(
        &config,
        snapshot.clone(),
        &identity,
        admission.clone(),
        30_000,
    )
    .await
    .unwrap();
    let settings = identity.load_config().await.unwrap();
    let id = settings.device_id.to_string();
    let key = settings.device_ed25519_keypair.verifying_key();
    let server = Arc::new(
        hap::server::IpServer::new_loopback(settings, identity.clone())
            .await
            .unwrap(),
    );
    graph.install(&server).await.unwrap();
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();

    let (ready, started) = tokio::sync::oneshot::channel();
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle_ready(ready).await });
    started.await.unwrap();
    let mut expected = BTreeMap::new();
    for (index, (binding, _)) in graph.lights.iter().enumerate() {
        expected.insert((binding.aid, binding.on_iid), serde_json::json!(true));
        if let Some(iid) = binding.brightness_iid {
            expected.insert((binding.aid, iid), serde_json::json!(index - 55));
        }
    }
    assert_eq!(expected.len(), 136);
    let (subscribed, wait) = tokio::sync::oneshot::channel();
    let client = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut session = support::verify(&mut stream, controller, &signing, &id, &key);
        let (header, body) = session.request(
            &mut stream,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(header.starts_with("HTTP/1.1 200"));
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["accessories"].as_array().unwrap().len(), 97);
        let addresses = expected.keys().copied().collect::<Vec<_>>();
        for chunk in addresses.chunks(64) {
            let body=serde_json::json!({"characteristics":chunk.iter().map(|&(aid,iid)|serde_json::json!({"aid":aid,"iid":iid,"ev":true})).collect::<Vec<_>>()}).to_string();
            let request = format!(
                "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (header, body) = session.request(&mut stream, request.as_bytes());
            assert!(header.starts_with("HTTP/1.1 204"), "{header} {body:?}");
        }
        subscribed.send(()).unwrap();
        let mut received = BTreeMap::new();
        while received.len() < expected.len() {
            let (header, body) = session.request(&mut stream, b"");
            assert!(header.starts_with("EVENT/1.0"), "{header}");
            let event: serde_json::Value = serde_json::from_slice(&body).unwrap();
            for item in event["characteristics"].as_array().unwrap() {
                let address = (item["aid"].as_u64().unwrap(), item["iid"].as_u64().unwrap());
                assert_eq!(expected.get(&address), Some(&item["value"]));
                received.insert(address, item["value"].clone());
            }
        }
        assert_eq!(received, expected);
        for chunk in addresses.chunks(64) {
            let ids = chunk
                .iter()
                .map(|(aid, iid)| format!("{aid}.{iid}"))
                .collect::<Vec<_>>()
                .join(",");
            let (header, body) = session.request(
                &mut stream,
                format!("GET /characteristics?id={ids} HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .as_bytes(),
            );
            assert!(header.starts_with("HTTP/1.1 200"));
            let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                data["characteristics"].as_array().unwrap().len(),
                chunk.len()
            );
            for item in data["characteristics"].as_array().unwrap() {
                assert_eq!(
                    expected.get(&(item["aid"].as_u64().unwrap(), item["iid"].as_u64().unwrap())),
                    Some(&item["value"])
                );
            }
        }
    });
    wait.await.unwrap();
    snapshot.cursor.sequence += 1;
    snapshot.state_version = snapshot.cursor.sequence;
    for (index, state) in snapshot.devices.values_mut().enumerate() {
        state.observed_at_ms = Some(0);
        state.version += 1;
        state.observed = Some(if index < 56 {
            Observation::Switch(true)
        } else {
            Observation::Level(Percent::new((index - 55) as u8).unwrap())
        });
    }
    graph.publish_snapshot(snapshot.clone()).await.unwrap();
    client.await.unwrap();
    task.abort();
    let _ = task.await;
    let number = identity.load_config().await.unwrap().configuration_number;
    let addresses = graph
        .lights
        .iter()
        .map(|(binding, _)| {
            (
                binding.device.clone(),
                binding.aid,
                binding.on_iid,
                binding.brightness_iid,
            )
        })
        .collect::<Vec<_>>();
    drop((graph, server));
    let rebuilt = homesensed::runtime_hap::RuntimeHap::new(
        &config,
        snapshot,
        &identity,
        admission.clone(),
        30_000,
    )
    .await
    .unwrap();
    let server = hap::server::IpServer::new_loopback(
        identity.load_config().await.unwrap(),
        identity.clone(),
    )
    .await
    .unwrap();
    rebuilt.install(&server).await.unwrap();
    assert_eq!(
        identity.load_config().await.unwrap().configuration_number,
        number
    );
    assert_eq!(
        rebuilt
            .lights
            .iter()
            .map(|(binding, _)| (
                binding.device.clone(),
                binding.aid,
                binding.on_iid,
                binding.brightness_iid
            ))
            .collect::<Vec<_>>(),
        addresses
    );
    drop((rebuilt, server));
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
