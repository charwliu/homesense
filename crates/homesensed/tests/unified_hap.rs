//! Encrypted controller against the actual unified simulation service.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[tokio::test]
async fn unified_service_publishes_both_graphs_and_does_not_deadlock_writes() {
    let root = std::env::temp_dir().join(format!("unified-hap-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let lights = LightGraph::build(
        &identity,
        "HomeSense",
        vec![LightSpec {
            device: homesense_core::DeviceId::new("simulation:light").unwrap(),
            name: "Light".into(),
            dimmable: true,
            default_on_level: Some(homesense_core::Percent::new(100).unwrap()),
        }],
    )
    .await
    .unwrap();
    let light = lights.bindings[0].clone();
    let scenes = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "demo".into(),
            name: "demo".into(),
        }],
    )
    .await
    .unwrap();
    let scene = scenes.bindings[0].clone();
    struct Reject;
    impl hap::server::WriteBatchHandler for Reject {
        fn write(&self, _: Uuid, _: &[u8]) -> Result<(), hap::server::WriteStatus> {
            Err(hap::server::WriteStatus::ServiceCommunicationFailure)
        }
    }
    let seed = hap::server::IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    let mut handles: Vec<hap::server::AccessoryHandle> = vec![std::sync::Arc::new(
        hap::futures::lock::Mutex::new(Box::new(lights.bridge)),
    )];
    for (_, light) in lights.lights {
        handles.push(std::sync::Arc::new(hap::futures::lock::Mutex::new(
            Box::new(light),
        )));
    }
    for (_, scene) in scenes.scenes {
        handles.push(std::sync::Arc::new(hap::futures::lock::Mutex::new(
            Box::new(scene),
        )));
    }
    seed.install_startup_graph(handles, std::sync::Arc::new(Reject))
        .await
        .unwrap();
    drop(seed);
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    identity
        .save_pairing(&Pairing::new(
            controller,
            Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let devices = include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    drop(reservation);
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::service::serve_unified_simulation(
            devices,
            rules,
            &directory,
            Duration::from_secs(5),
            homesensed::service::UnifiedSimulation {
                address,
                scenes: vec!["demo".into()],
            },
        )
        .await
    });
    let client = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match TcpStream::connect(address) {
                Ok(s) => break s,
                Err(e) => {
                    assert!(Instant::now() < deadline, "{e}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut session = support::verify(
            &mut stream,
            controller,
            &signing,
            &accessory_id,
            &accessory_key,
        );
        let (headers, body) = session.request(
            &mut stream,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(headers.starts_with("HTTP/1.1 200"));
        let graph: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(graph["accessories"].as_array().unwrap().len(), 3);
        let mut write = |items: serde_json::Value| {
            let body = serde_json::json!({"characteristics":items}).to_string();
            let request = format!(
                "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            session.request(&mut stream, request.as_bytes())
        };
        for _ in 0..5 {
            let (h, b) =
                write(serde_json::json!([{"aid":scene.aid,"iid":scene.on_iid,"value":true}]));
            assert!(h.starts_with("HTTP/1.1 204"), "{h} {b:?}");
            let (h, b) = write(
                serde_json::json!([{"aid":light.aid,"iid":light.brightness_iid.unwrap(),"value":30}]),
            );
            assert!(h.starts_with("HTTP/1.1 204"), "{h} {b:?}");
        }
        let (h, _) = write(
            serde_json::json!([{"aid":light.aid,"iid":light.on_iid,"value":true},{"aid":scene.aid,"iid":scene.on_iid,"value":true}]),
        );
        assert!(h.starts_with("HTTP/1.1 207"));
        let request = format!(
            "GET /characteristics?id={}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            scene.aid, scene.on_iid
        );
        let (h, b) = session.request(&mut stream, request.as_bytes());
        assert!(h.starts_with("HTTP/1.1 200"));
        let value: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(value["characteristics"][0]["value"], false);
        let payload=serde_json::json!({"characteristics":[{"aid":scene.aid,"iid":scene.on_iid,"ev":true,"value":true}]}).to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let first = session.request(&mut stream, request.as_bytes());
        let second = session.request(&mut stream, b"");
        assert!(
            [&first.0, &second.0]
                .iter()
                .any(|h| h.starts_with("HTTP/1.1 204"))
        );
        let event = if first.0.starts_with("EVENT/") {
            first
        } else {
            second
        };
        assert!(event.0.starts_with("EVENT/"), "{}", event.0);
        let event: serde_json::Value = serde_json::from_slice(&event.1).unwrap();
        assert_eq!(event["characteristics"][0]["value"], false);
    });
    let client_result = client.await;
    let service_result = service.await;
    assert!(
        service_result.as_ref().is_ok_and(|r| r.is_ok()),
        "service: {service_result:?}"
    );
    client_result.unwrap();
    let number=identity.load_config().await.unwrap().configuration_number;
    homesensed::service::serve_unified_simulation(devices,rules,&root,Duration::from_millis(100),homesensed::service::UnifiedSimulation{address,scenes:vec!["demo".into()]}).await.unwrap();
    assert_eq!(identity.load_config().await.unwrap().configuration_number,number);
    assert!(identity.controller_is_paired(controller).unwrap());
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    drop(sql);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
