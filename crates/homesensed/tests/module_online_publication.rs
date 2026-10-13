//! Real localhost Web/publication/HAP path with synthetic, query-only gateways.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    serde_json::{Value, json},
    storage::Storage,
};
use homesense_core::{DeviceId, ExecutionContext};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    storage::IdentityStore,
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const TOKEN: &str = "module-publication-synthetic-test-token";
fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
async fn http(address: SocketAddr, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map_or(String::new(), |v| v.to_string());
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(4), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
async fn read_hap(
    store: &IdentityStore,
    address: SocketAddr,
    controller: Uuid,
    signing: ed25519_dalek::SigningKey,
) -> (TcpStream, Value) {
    let config = store.load_config().await.unwrap();
    let id = config.device_id.to_string();
    let public = config.device_ed25519_keypair.verifying_key();
    tokio::task::spawn_blocking(move || {
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut session = support::verify(&mut socket, controller, &signing, &id, &public);
        let (header, body) = session.request(
            &mut socket,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(header.starts_with("HTTP/1.1 200"), "{header}");
        (socket, serde_json::from_slice(&body).unwrap())
    })
    .await
    .unwrap()
}
fn light_ids(graph: &Value) -> (u64, Vec<u64>) {
    let light = &graph["accessories"][1];
    (
        light["aid"].as_u64().unwrap(),
        light["services"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["characteristics"].as_array().unwrap())
            .map(|c| c["iid"].as_u64().unwrap())
            .collect(),
    )
}

#[tokio::test]
async fn stable_module_remap_publishes_all_ports_preserves_pairing_and_retires_old_hap() {
    exercise(false).await;
}
#[tokio::test]
async fn missing_unbound_port_during_remap_fails_closed_without_control_or_replay() {
    exercise(true).await;
}
async fn exercise(omit: bool) {
    let directory = std::env::temp_dir().join(format!("module-online-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let web = free_address();
    let hap_address = free_address();
    let verified =
        json!({"status":"verified","evidence":"synthetic localhost only","verified_at_unix_ms":1});
    let devices = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":[
        {"id":"module-light","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":verified},
        {"id":"module-unbound","address":"16","profile":"ad8_din_rs485_v2_2_1","verification":verified}]}],"devices":[
        {"id":"light","name":"Synthetic light","module_id":"module-light","channel":1,"enabled":true},
        {"id":"disabled","name":"Disabled port","module_id":"module-light","channel":2,"enabled":false}]});
    let rules = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"scene","enabled":true,"review_evidence":"synthetic only","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"light","command":{"kind":"level","value":40,"ramp":true}}]}]});
    let frames = Arc::new(Mutex::new(vec![]));
    let tap = frames.clone();
    let gateway = std::thread::spawn(move || {
        let mut workers = vec![];
        for generation in 0..2 {
            let (mut socket, _) = bus.accept().unwrap();
            let tap = tap.clone();
            workers.push(std::thread::spawn(move || {
                socket
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                loop {
                    let mut frame = [0; 5];
                    match socket.read_exact(&mut frame) {
                        Ok(()) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::UnexpectedEof
                                    | std::io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            break;
                        }
                        Err(e) => panic!("synthetic gateway: {e}"),
                    }
                    assert_eq!(
                        (frame[0], frame[3], frame[4]),
                        (0xae, 0xf2, 0xee),
                        "actuator output forbidden"
                    );
                    assert!((0xa0..=0xa8).contains(&frame[2]), "only queries allowed");
                    assert!(
                        if generation == 0 {
                            [0x15, 0x16].contains(&frame[1])
                        } else {
                            [0x16, 0x61].contains(&frame[1])
                        },
                        "query used obsolete mapping"
                    );
                    tap.lock().unwrap().push((generation, frame));
                    if generation == 1 {
                        // Old physical address on the new stream must not seed the remapped module.
                        socket.write_all(&[0xae, 0x15, 0xd1, 99, 0xee]).unwrap();
                    }
                    let channels: Vec<_> = if frame[2] == 0xa0 {
                        (1..=8).collect()
                    } else {
                        vec![frame[2] - 0xa0]
                    };
                    let reply: Vec<_> = channels
                        .into_iter()
                        .filter(|channel| {
                            !(omit && generation == 1 && frame[1] == 0x16 && *channel == 8)
                        })
                        .flat_map(|channel| {
                            [
                                0xae,
                                frame[1],
                                0xd0 + channel,
                                if generation == 0 {
                                    11
                                } else if frame[1] == 0x61 {
                                    44
                                } else {
                                    22
                                },
                                0xee,
                            ]
                        })
                        .collect();
                    socket.write_all(&reply).unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
    let mut identity = IdentityStore::open(directory.join("identity"))
        .await
        .unwrap();
    let identity_config = hap::Config {
        host: hap_address.ip(),
        port: hap_address.port(),
        ..Default::default()
    };
    identity.save_config(&identity_config).await.unwrap();
    identity.initialize_aid_cache().await.unwrap();
    LightGraph::build(
        &identity,
        "HomeSense",
        vec![LightSpec {
            device: DeviceId::new("light").unwrap(),
            name: "Synthetic light".into(),
            dimmable: true,
            default_on_level: None,
        }],
    )
    .await
    .unwrap();
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let input = serde_json::to_vec(&devices).unwrap();
    let automation = serde_json::to_vec(&rules).unwrap();
    let root = directory.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_combined(
            &input,
            &automation,
            &root,
            Duration::from_secs(6),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: TOKEN.into(),
                administrator: true,
                tls: None,
            },
            homesensed::hap_endpoint::Settings {
                address: hap_address,
                scenes: vec![],
                motors: vec![],
            },
        )
        .await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::net::TcpStream::connect(web).await.is_err() {
        if service.is_finished() {
            panic!(
                "service ended before initial readiness: {:?}",
                service.await
            );
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (code, before) = http(web, "/api/v1/modules", None).await;
    assert_eq!(code, 200);
    assert!(
        before["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["fresh_channels"] == 8)
    );
    let (old_session, old_graph) =
        read_hap(&identity, hap_address, controller, signing.clone()).await;
    let source = Uuid::new_v4();
    let candidate = Uuid::new_v4();
    let mut source_devices = devices.clone();
    source_devices["revision"] = 2.into();
    let mut source_rules = rules.clone();
    source_rules["configuration_revision"] = 2.into();
    assert_eq!(http(web, &format!("/api/v1/config/drafts/{source}"), Some(json!({"expected_revision":1,"revision":2,"devices":source_devices,"automation":source_rules}))).await.0, 200);
    let edit = json!({"id":candidate,"expected_revision":"1","revision":"3","module_id":"module-light","new_address":"61","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","evidence":"synthetic localhost only","verified_at_unix_ms":"1"}});
    let (code, replacement) = http(
        web,
        &format!("/api/v1/config/drafts/{source}/module-replacement"),
        Some(edit),
    )
    .await;
    assert_eq!(code, 200, "{replacement}");
    assert_eq!(replacement["active"], false);
    assert_eq!(replacement["impact"]["hardware_address_changed"], false);
    let (_, preview) = http(web, &format!("/api/v1/config/previews/{candidate}"), None).await;
    assert_eq!(
        preview["impact"]["connection_rebuild_required"], true,
        "{preview}"
    );
    let activation = json!({"request_id":"synthetic-remap","boot":before["boot"]});
    let (_, outcome) = http(
        web,
        &format!("/api/v1/config/activations/{candidate}"),
        Some(activation.clone()),
    )
    .await;
    tokio::task::spawn_blocking(move || {
        let mut old_session = old_session;
        old_session
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut byte = [0];
        match old_session.read(&mut byte) {
            Ok(0) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            other => panic!("old HAP connection survived publication: {other:?}"),
        }
    })
    .await
    .unwrap();
    if omit {
        assert_ne!(
            outcome["outcome"], "activated",
            "missing unbound channel cannot open readiness"
        );
        assert!(service.await.unwrap().is_err());
        assert!(tokio::net::TcpStream::connect(web).await.is_err());
        assert!(tokio::net::TcpStream::connect(hap_address).await.is_err());
    } else {
        assert_eq!(outcome["outcome"], "activated", "{outcome}");
        assert_eq!(
            http(
                web,
                &format!("/api/v1/config/activations/{candidate}"),
                Some(activation)
            )
            .await
            .0,
            200
        );
        let (_, after) = http(web, "/api/v1/modules", None).await;
        assert_eq!(after["revision"], "3");
        assert_ne!(after["transport_epoch"], before["transport_epoch"]);
        assert!(
            after["modules"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["fresh_channels"] == 8)
        );
        let remapped = after["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["module_id"] == "module-light")
            .unwrap();
        assert_eq!(remapped["address"], "61");
        assert_eq!(remapped["slots"][0]["actual"]["level"], 44);
        assert_eq!(remapped["slots"][1]["binding"]["actions"], json!([]));
        let (_, view) = http(web, "/api/v1/devices", None).await;
        assert_eq!(view["devices"].as_array().unwrap().len(), 1);
        assert_eq!(view["devices"][0]["id"], "light");
        assert_eq!(view["devices"][0]["observed"]["level"], 44);
        let (fresh_session, fresh_graph) =
            read_hap(&identity, hap_address, controller, signing).await;
        assert_eq!(light_ids(&fresh_graph), light_ids(&old_graph));
        let brightness = |graph: &Value| {
            graph["accessories"][1]["services"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|service| service["characteristics"].as_array().unwrap())
                .find(|c| c["format"] == "int" && c["unit"] == "percentage")
                .unwrap()["value"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(brightness(&old_graph), 11);
        assert_eq!(
            brightness(&fresh_graph),
            44,
            "old-address value 99 must not reach encrypted HAP"
        );
        drop(fresh_session);
        let stale = json!({"module_id":"module-light","scope":{"kind":"whole_module"},"revision":"1","request_id":"obsolete-query"});
        assert_eq!(
            http(web, "/api/v1/module-queries", Some(stale)).await.0,
            409
        );
        assert_eq!(http(web, "/api/v1/module-commands", Some(json!({"module_id":"module-light","channel":2,"revision":"3","request_id":"disabled-control","command":{"action":"brightness","level":80}}))).await.0, 400);
        service.await.unwrap().unwrap();
    }
    gateway.join().unwrap();
    let preserved = identity.load_config().await.unwrap();
    assert_eq!(preserved.device_id, identity_config.device_id);
    assert_eq!(
        preserved.device_ed25519_keypair.verifying_key(),
        identity_config.device_ed25519_keypair.verifying_key()
    );
    assert!(identity.controller_is_paired(controller).unwrap());
    let store = homesense_storage::Store::open(directory.join("automation-tcp.sqlite")).unwrap();
    let active = store
        .active_bundle(ExecutionContext::Live)
        .unwrap()
        .unwrap();
    assert_eq!(active.configuration.revision(), 3);
    let saved: Value = serde_json::from_slice(&store.active().unwrap().unwrap().payload).unwrap();
    assert_eq!(saved["devices"], devices["devices"]);
    assert_eq!(
        active.automation.unwrap().timed_scenes["scene"].steps.len(),
        1
    );
    let saved_rules: Value =
        serde_json::from_slice(&store.automation_payload(3).unwrap().unwrap()).unwrap();
    assert_eq!(saved_rules["timed_scenes"], rules["timed_scenes"]);
    let sql = rusqlite::Connection::open(directory.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let frames = frames.lock().unwrap();
    assert_eq!(
        frames
            .iter()
            .filter(|(generation, _)| *generation == 0)
            .take(2)
            .map(|(_, frame)| *frame)
            .collect::<Vec<_>>(),
        vec![
            [0xae, 0x15, 0xa0, 0xf2, 0xee],
            [0xae, 0x16, 0xa0, 0xf2, 0xee]
        ]
    );
    assert!(
        frames
            .iter()
            .filter(|(generation, _)| *generation == 1)
            .all(|(_, frame)| frame[1] != 0x15)
    );
    drop(frames);
    drop(sql);
    drop(store);
    drop(identity);
    std::fs::remove_dir_all(directory).unwrap();
}
