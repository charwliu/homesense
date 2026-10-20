//! Independent encrypted HAP reconnect/subscription audit using localhost simulation.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    scene_graph::{SceneActivityGraph, SceneGraph, SceneSpec},
    storage::IdentityStore,
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[tokio::test]
async fn encrypted_reauthentication_reads_web_changes_and_resubscription_delivers_next_change() {
    run_resume().await;
}
async fn run_resume() {
    let combined = true;
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
    let activity=SceneActivityGraph::build(&identity,vec![SceneSpec{id:"demo".into(),name:"Demo activity".into()}]).await.unwrap().bindings[0].clone();
    let scenes = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "demo".into(),
            name: "demo".into(),
        }],
    )
    .await
    .unwrap();
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
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"scene_tracking":[{"scene":"demo","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Demo activity","max_age_ms":2000,"review_evidence":"Synthetic reconnect observation","members":[{"id":"level","device":"simulation:light","predicate":{"kind":"level_range","minimum":17,"maximum":17},"review_evidence":"Synthetic exact level"}]}],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    drop(reservation);
    let web_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let web_address = web_reservation.local_addr().unwrap();
    drop(web_reservation);
    let directory = root.clone();
    let service = tokio::spawn(async move {
        let hap = homesensed::service::UnifiedSimulation {
            address,
            scenes: vec!["demo".into()],
        };
        if combined {
            homesensed::service::serve_combined_simulation(
                devices,
                rules,
                &directory,
                Duration::from_secs(12),
                hap,
                homesensed::service::WebSettings {
                    address: web_address,
                    secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                    administrator: true,
                    tls: None,
                },
            )
            .await
        } else {
            homesensed::service::serve_unified_simulation(
                devices,
                rules,
                &directory,
                Duration::from_secs(12),
                hap,
            )
            .await
        }
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

        let (h,b)=session.request(&mut stream,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(h.starts_with("HTTP/1.1 200"));
        let graph:serde_json::Value=serde_json::from_slice(&b).unwrap();
        assert!(graph["accessories"].as_array().unwrap().iter().flat_map(|a|a["services"].as_array().unwrap()).all(|s|s["type"]!="43"));
        let body=serde_json::json!({"characteristics":[{"aid":activity.aid,"iid":activity.on_iid,"ev":true}]}).to_string();
        let subscribe = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (h, b) = session.request(&mut stream, subscribe.as_bytes());
        assert!(h.starts_with("HTTP/1.1 204"), "{h} {b:?}");

        // A subscribed authenticated client disconnects while Web control remains active.
        drop(stream);
        drop(session);
        let change = |target: u8, id: &str| {
            let (status, value) = web_request(
                web_address,
                "/api/v1/device-commands",
                Some(
                    serde_json::json!({"request_id":id,"device":"simulation:light","command":{"action":"brightness","level":target}}),
                ),
            );
            assert_eq!(status, 202, "{value}");
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let (status, value) = web_request(web_address, "/api/v1/devices", None);
                assert_eq!(status, 200);
                if value["devices"][0]["observed"]["level"] == target {
                    break;
                }
                assert!(Instant::now() < deadline, "{value}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        change(17, "disconnected-change");
        let mut stream = TcpStream::connect(address).unwrap();
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
        let read = format!("GET /characteristics?id={}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",activity.aid,activity.on_iid);
        let (h,b)=session.request(&mut stream,read.as_bytes());
        assert!(h.starts_with("HTTP/1.1 200"));
        let v:serde_json::Value=serde_json::from_slice(&b).unwrap();
        assert_eq!(v["characteristics"][0]["value"],true);
        // New encrypted session explicitly re-establishes subscriptions.
        let (h, b) = session.request(&mut stream, subscribe.as_bytes());
        assert!(h.starts_with("HTTP/1.1 204"), "{h} {b:?}");
        // External controls must be observed without a HAP write or reconnection.
        for (target, id) in [(30, "foreground-30"), (17, "foreground-17")] {
            change(target, id);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let (h, b) = session.request(&mut stream, b"");
                assert!(h.starts_with("EVENT/"), "{h}");
                let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
                let found = v["characteristics"].as_array().unwrap().iter().any(|c| {
                    c["aid"] == activity.aid
                        && c["iid"] == activity.on_iid
                        && c["value"] == (target == 17)
                });
                if found {
                    break;
                }
                assert!(Instant::now() < deadline, "{v}");
            }
        }
        drop(stream);
    });
    let client_result = client.await;
    let service_result = service.await;
    assert!(
        service_result.as_ref().is_ok_and(|r| r.is_ok()),
        "{service_result:?}"
    );
    client_result.unwrap();
    assert!(identity.controller_is_paired(controller).unwrap());
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    drop(sql);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
fn web_request(
    address: std::net::SocketAddr,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    use std::io::{Read, Write};
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let text = body.as_ref().map_or(String::new(), |v| v.to_string());
    write!(stream,"{} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",if body.is_some(){"POST"}else{"GET"},text.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
