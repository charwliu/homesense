use super::*;
use hap::{accessory::HapAccessory, server::Server, storage::Storage};
use homesense_core::{DeviceId, ExecutionContext, Observation};
use homesense_state::Quality;
#[tokio::test]
async fn fan_migration_encrypted_hap_keeps_aid_pairing_and_feedback_authority() {
    let root = std::env::temp_dir().join(format!("runtime-fan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let raw = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"fan","name":"Exhaust","module_id":"relay","channel":1,"enabled":true},{"id":"lamp","name":"Lamp","module_id":"relay","channel":2,"enabled":true}]});
    let path = root.join("runtime.sqlite");
    let mut store = homesense_storage::Store::open(&path).unwrap();
    let config = store
        .publish_configuration(
            None,
            &serde_json::to_vec(&raw).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    drop(store);
    let admission = Admission::open(
        path.clone(),
        crate::admission::Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut physical = crate::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    for state in physical.devices.values_mut() {
        state.quality = Quality::Confirmed;
        state.observed_at_ms = Some(admission.monotonic_ms());
        state.observed = Some(Observation::Switch(false));
    }
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    let cfg = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    identity.save_config(&cfg).await.unwrap();
    let fan = DeviceId::new("fan").unwrap();
    let lamp = DeviceId::new("lamp").unwrap();
    let old = LightGraph::build(
        &identity,
        "Test",
        vec![
            LightSpec {
                device: fan.clone(),
                name: "Exhaust".into(),
                dimmable: false,
                default_on_level: None,
            },
            LightSpec {
                device: lamp.clone(),
                name: "Lamp".into(),
                dimmable: false,
                default_on_level: None,
            },
        ],
    )
    .await
    .unwrap();
    let aid = old.lights[&fan].get_id();
    let old_iid = old
        .bindings
        .iter()
        .find(|b| b.device == fan)
        .unwrap()
        .on_iid;
    drop(old);
    identity.initialize_aid_cache().await.unwrap();
    let controller = uuid::Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[79; 32]);
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let saved = identity.load_config().await.unwrap();
    let accessory_id = saved.device_id.to_string();
    let key = saved.device_ed25519_keypair.verifying_key();
    let graph = RuntimeHap::new_selected_with_fans(
        &config,
        physical,
        &identity,
        admission.clone(),
        5000,
        Some([fan.clone(), lamp].into()),
        [fan.clone()].into(),
        None,
    )
    .await
    .unwrap();
    let on_iid = graph
        .lights
        .iter()
        .find(|(b, _)| b.device == fan)
        .unwrap()
        .0
        .on_iid;
    assert_ne!(old_iid, on_iid);
    let server = hap::server::IpServer::new_loopback(saved, identity.clone())
        .await
        .unwrap();
    let handles = std::iter::once(graph.bridge.clone())
        .chain(graph.lights.iter().map(|(_, h)| h.clone()))
        .collect();
    server
        .install_startup_graph(handles, graph.write_handler.clone())
        .await
        .unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    tokio::task::spawn_blocking(move ||{
        let mut socket=std::net::TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();let mut session=crate::hap_test_support::verify(&mut socket,controller,&signing,&accessory_id,&key);
        let (h,body)=session.request(&mut socket,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(h.starts_with("HTTP/1.1 200"));let v:serde_json::Value=serde_json::from_slice(&body).unwrap();let a=v["accessories"].as_array().unwrap().iter().find(|a|a["aid"]==aid).unwrap();let service=a["services"].as_array().unwrap().iter().find(|s|s["type"]=="40").unwrap();assert!(!a["services"].as_array().unwrap().iter().any(|s|s["type"]=="43"));assert_eq!(service["characteristics"].as_array().unwrap().len(),1);assert_eq!(service["characteristics"][0]["iid"],on_iid);assert_eq!(service["characteristics"][0]["type"],"25");
        let count=||rusqlite::Connection::open_with_flags(&path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap().query_row("SELECT count(*) FROM command_journal WHERE device_id='fan'",[],|r|r.get::<_,i64>(0)).unwrap();
        let put=|session:&mut crate::hap_test_support::Session,socket:&mut std::net::TcpStream,body:serde_json::Value|{let body=body.to_string();session.request(socket,format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",body.len(),body).as_bytes())};
        let (h,_)=put(&mut session,&mut socket,serde_json::json!({"characteristics":[{"aid":aid,"iid":on_iid,"ev":true}]}));assert!(h.starts_with("HTTP/1.1 204"));assert_eq!(count(),0,"subscription must not start fan");
        let (h,b)=put(&mut session,&mut socket,serde_json::json!({"characteristics":[{"aid":aid,"iid":on_iid,"value":true}]}));assert!(h.starts_with("HTTP/1.1 204"),"{h} {b:?}");assert_eq!(count(),1);
        let (h,b)=session.request(&mut socket,format!("GET /characteristics?id={aid}.{on_iid} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());assert!(h.starts_with("HTTP/1.1 200"));let b:serde_json::Value=serde_json::from_slice(&b).unwrap();assert_eq!(b["characteristics"][0]["value"],false,"accepted On must not replace real Off feedback");
        let (h,_)=put(&mut session,&mut socket,serde_json::json!({"characteristics":[{"aid":aid,"iid":old_iid,"value":true}]}));assert!(h.starts_with("HTTP/1.1 207"));assert_eq!(count(),1,"retired light characteristic must not control fan");
    }).await.unwrap();
    task.abort();
    let _ = task.await;
    drop(server);
    drop(graph);
    admission.shutdown().await.unwrap();
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
