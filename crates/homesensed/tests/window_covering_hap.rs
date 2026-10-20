#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{server::WriteStatus, storage::Storage};
use homesense_core::{Command, DeviceId, ExecutionContext, MotorMotion};
use homesense_hap::{motor_graph::MotorSpec, storage::IdentityStore};
use homesensed::{
    admission::{Admission, Options},
    runtime_window_covering_hap::RuntimeWindowCoveringHap,
};
use uuid::Uuid;

#[tokio::test]
async fn native_covering_reports_fifty_then_auto_stops_at_intermediate_target() {
    let root = std::env::temp_dir().join(format!("window-covering-hap-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let document = serde_json::json!({
        "schema_version":1,"revision":1,
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],
        "devices":[{"id":"motor","name":"Curtain","gateway":"bus","address":"22","channel":2,"enabled":true,
            "motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}]
    });
    let bytes = serde_json::to_vec(&document).unwrap();
    let config =
        homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
    let db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        db,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let device = DeviceId::new("motor").unwrap();
    let mut covering = RuntimeWindowCoveringHap::new(
        &config,
        &identity,
        admission.clone(),
        controller,
        vec![MotorSpec {
            device: device.clone(),
            name: "Curtain".into(),
        }],
        move |id, requested| id == controller && requested == &DeviceId::new("motor").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(covering.accessories.len(), 1);
    let binding = &covering.accessories[0].0;
    let covering_aid = binding.aid;
    let target_iid = binding.target_iid;
    let body = serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.target_iid,"value":55}]}).to_string();
    assert_eq!(
        covering
            .write_handler
            .write_async(Uuid::new_v4(), body.as_bytes())
            .await,
        Err(WriteStatus::InsufficientPrivileges)
    );
    assert!(admission.dispatch().await.unwrap().is_none());
    covering
        .write_handler
        .write_async(controller, body.as_bytes())
        .await
        .unwrap();
    let open = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(open.command.command, Command::Motor(MotorMotion::Down));
    admission.finish_write(open.command.id, true).await.unwrap();
    covering.refresh().await.unwrap();
    // From the unknown 50% start, 55% is one tenth of the remaining journey.
    tokio::time::sleep(std::time::Duration::from_millis(1_150)).await;
    covering.refresh().await.unwrap();
    let stop = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(stop.command.command, Command::Motor(MotorMotion::Stop));
    admission.finish_write(stop.command.id, true).await.unwrap();
    covering.refresh().await.unwrap();
    let handle = covering.accessories[0].1.lock().await;
    let serialized = serde_json::to_value(&**handle).unwrap();
    let service = serialized["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["type"] == "8C")
        .unwrap();
    let value = |kind| {
        service["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == kind)
            .unwrap()["value"]
            .clone()
    };
    // The 100 ms HAP projection interval can submit STOP just after the
    // nominal percentage boundary; report that inferred position faithfully.
    let stopped = value("6D").as_u64().unwrap();
    assert!((55..=56).contains(&stopped));
    assert_eq!(value("7C"), stopped);
    assert_eq!(value("72"), 2);
    drop(handle);
    let initial = homesensed::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    let lights = homesensed::runtime_hap::RuntimeHap::new_selected(
        &config,
        initial,
        &identity,
        admission.clone(),
        5000,
        Some(std::collections::BTreeSet::new()),
    )
    .await
    .unwrap();
    let unified = homesensed::unified_hap::UnifiedHap {
        lights,
        scenes: None,
        motors: None,
        window_coverings: Some(covering),
        away: None,
    };
    let server = hap::server::IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    unified.install(&server).await.unwrap();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    drop(reservation);
    let (ready, started) = tokio::sync::oneshot::channel();
    let running = server.clone();
    let listener = tokio::spawn(async move { running.run_handle_ready(ready).await });
    started.await.unwrap();
    let (sent, mut incoming) = tokio::sync::mpsc::channel(1);
    let client = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut session = support::verify(
            &mut stream,
            controller,
            &signing,
            &accessory_id,
            &accessory_key,
        );
        let (header, body) = session.request(
            &mut stream,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(header.starts_with("HTTP/1.1 200"), "{header} {body:?}");
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let accessories = document["accessories"].as_array().unwrap();
        assert_eq!(accessories.len(), 2); // bridge + one native covering
        let services = accessories.iter().find(|a| a["aid"] != 1).unwrap()["services"]
            .as_array()
            .unwrap();
        let covering = services.iter().find(|s| s["type"] == "8C").unwrap();
        assert_eq!(
            covering["characteristics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["type"] == "6D")
                .unwrap()["value"],
            stopped
        );
        assert!(!services.iter().any(|s| s["type"] == "49")); // no switch service
        let body = serde_json::json!({"characteristics":[{"aid":covering_aid,"iid":target_iid,"value":100}]}).to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (header, response) = session.request(&mut stream, request.as_bytes());
        assert!(header.starts_with("HTTP/1.1 204"), "{header} {response:?}");
        sent.blocking_send(()).unwrap();
    });
    incoming.recv().await.unwrap();
    let next = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(next.command.command, Command::Motor(MotorMotion::Down));
    admission.finish_write(next.command.id, true).await.unwrap();
    client.await.unwrap();
    listener.abort();
    let _ = listener.await;
    drop(server);
    admission.shutdown().await.unwrap();
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
