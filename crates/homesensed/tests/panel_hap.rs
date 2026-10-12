//! Encrypted panel subscribers through the shared reconnecting TCP runtime.
#[path = "support/pair_setup.rs"]
mod pair_setup;
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn free_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
enum Action {
    Clicks,
    HoldDisconnect,
}
fn request(
    session: &mut support::Session,
    socket: &mut std::net::TcpStream,
    method: &str,
    path: &str,
    body: &str,
) -> (String, Vec<u8>) {
    session.request(socket,format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes())
}
#[tokio::test]
async fn selected_panel_reads_subscribes_and_recovers_without_old_gestures() {
    let root = std::env::temp_dir().join(format!(
        "homesense-panel-encrypted-{}",
        uuid::Uuid::new_v4()
    ));
    let bus = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    raw["gateways"][0]["modules"][0]["verification"] =
        serde_json::json!({"status":"verified","evidence":"synthetic","verified_at_unix_ms":1});
    raw["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"verified","evidence":"synthetic","verified_at_unix_ms":1}}));
    for key in [2, 6] {
        raw["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":format!("key-{key}"),"name":format!("Panel key {key}"),"gateway":"simulation","address":"41","channel":key,"enabled":true}));
    }
    let devices = serde_json::to_vec(&raw).unwrap();
    let config =
        homesense_config::Configuration::parse(&devices, homesense_core::ExecutionContext::Live)
            .unwrap();
    let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let document = homesense_config::rule_document::Document::parse(rules, &config).unwrap();
    let address = free_address();
    let web = free_address();
    let plan=serde_json::to_vec(&serde_json::json!({"schema_version":1,"bridges":[{"id":"panel","name":"Selected panel","address":address,"lights":[],"scenes":[],"motors":[],"panels":["key-2"]}]})).unwrap();
    let mut prepared = homesensed::hap_bridges::prepare(&plan, &config, &document, &root)
        .await
        .unwrap();
    let identity = prepared.remove(0).identity;
    let saved = identity.load_config().await.unwrap();
    let (actions, mut commands) = tokio::sync::mpsc::channel(4);
    let (connected, mut connections) = tokio::sync::mpsc::channel(4);
    let controls = Arc::new(Mutex::new(Vec::new()));
    let writes = controls.clone();
    let gateway = tokio::spawn(async move {
        for epoch in 0..2 {
            let (mut socket, _) = bus.accept().await.unwrap();
            loop {
                let mut frame = [0; 5];
                tokio::select! {
                    result=socket.read_exact(&mut frame)=>{
                        if result.is_err(){break;}
                        assert_eq!(frame,[0xae,0x15,0xa1,0xf2,0xee],"unexpected control/query {frame:?}");
                        writes.lock().unwrap().push(frame);
                        socket.write_all(&[0xae,0x15,0xd1,30,0xee]).await.unwrap();
                        connected.send(epoch).await.unwrap();
                    }
                    action=commands.recv()=>match action {
                        Some(Action::Clicks)=>for _ in 0..2 {
                            socket.write_all(&[0xae,0x41,0xd2,0xf1,0xee]).await.unwrap();
                            tokio::time::sleep(Duration::from_millis(60)).await;
                            socket.write_all(&[0xae,0x41,0xd2,0xf2,0xee]).await.unwrap();
                            tokio::time::sleep(Duration::from_millis(60)).await;
                        },
                        Some(Action::HoldDisconnect)=>{
                            socket.write_all(&[0xae,0x41,0xd2,0xf1,0xee]).await.unwrap();
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            break;
                        },
                        None=>break,
                    }
                }
            }
        }
    });
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_bridges(
            &devices,
            rules,
            &directory,
            Duration::from_secs(7),
            true,
            1,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                administrator: true,
                tls: None,
            },
            plan,
        )
        .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), connections.recv())
            .await
            .unwrap(),
        Some(0)
    );
    let keep_actions = actions.clone();
    let client = tokio::task::spawn_blocking(move || {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[47; 32]);
        let controller = uuid::Uuid::new_v4();
        let connect = || {
            for _ in 0..100 {
                if let Ok(socket) = std::net::TcpStream::connect(address) {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    return socket;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("panel endpoint not ready");
        };
        let mut socket = connect();
        pair_setup::setup(
            &mut socket,
            &saved.pin.to_string(),
            controller,
            &signing,
            &saved.device_id.to_string(),
            &saved.device_ed25519_keypair.verifying_key(),
        );
        let mut session = support::verify(
            &mut socket,
            controller,
            &signing,
            &saved.device_id.to_string(),
            &saved.device_ed25519_keypair.verifying_key(),
        );
        let (headers, body) = request(&mut session, &mut socket, "GET", "/accessories", "");
        assert!(headers.starts_with("HTTP/1.1 200"));
        let graph: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(graph["accessories"].as_array().unwrap().len(), 2);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("Panel key 2"));
        assert!(!text.contains("Panel key 6"));
        assert!(!text.contains("模拟调光灯"));
        let panel = &graph["accessories"][1];
        let aid = panel["aid"].as_u64().unwrap();
        let event = panel["services"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["characteristics"].as_array().unwrap())
            .find(|c| c["perms"].as_array().unwrap().iter().any(|p| p == "ev"))
            .unwrap();
        let iid = event["iid"].as_u64().unwrap();
        assert!(event["value"].is_null());
        let subscribe =
            serde_json::json!({"characteristics":[{"aid":aid,"iid":iid,"ev":true,"remote":true}]})
                .to_string();
        let put =
            serde_json::json!({"characteristics":[{"aid":aid,"iid":iid,"value":0}]}).to_string();
        let (headers, body) = request(&mut session, &mut socket, "PUT", "/characteristics", &put);
        assert!(headers.starts_with("HTTP/1.1 207"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["characteristics"][0]["status"],
            -70404
        );
        let (headers, _) = request(
            &mut session,
            &mut socket,
            "PUT",
            "/characteristics",
            &subscribe,
        );
        assert!(headers.starts_with("HTTP/1.1 204"));
        actions.blocking_send(Action::Clicks).unwrap();
        for _ in 0..2 {
            let (headers, body) = session.request(&mut socket, b"");
            assert!(headers.starts_with("EVENT/1.0"));
            let notification: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(notification["characteristics"][0]["value"], 0);
        }
        let path = format!("/characteristics?id={aid}.{iid}");
        let (headers, body) = request(&mut session, &mut socket, "GET", &path, "");
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["characteristics"][0]["value"].is_null());
        actions.blocking_send(Action::HoldDisconnect).unwrap();
        assert_eq!(connections.blocking_recv(), Some(1));
        drop(socket);
        let mut socket = connect();
        let mut session = support::verify(
            &mut socket,
            controller,
            &signing,
            &saved.device_id.to_string(),
            &saved.device_ed25519_keypair.verifying_key(),
        );
        let (headers, _) = request(
            &mut session,
            &mut socket,
            "PUT",
            "/characteristics",
            &subscribe,
        );
        assert!(headers.starts_with("HTTP/1.1 204"));
        // A retained down from the old generation must not become a long press.
        std::thread::sleep(Duration::from_millis(700));
        let (headers, body) = request(&mut session, &mut socket, "GET", &path, "");
        assert!(
            headers.starts_with("HTTP/1.1 200"),
            "unexpected replay {headers}"
        );
        assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["characteristics"][0]["value"].is_null());
        actions.blocking_send(Action::Clicks).unwrap();
        for _ in 0..2 {
            let (headers, body) = session.request(&mut socket, b"");
            assert!(headers.starts_with("EVENT/1.0"));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["characteristics"][0]["value"],
                0
            );
        }
    });
    client.await.unwrap();
    service.await.unwrap().unwrap();
    drop(keep_actions);
    gateway.await.unwrap();
    assert_eq!(controls.lock().unwrap().len(), 2);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
