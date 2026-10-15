#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use homesense_hap::storage::IdentityStore;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

async fn actual_devices(address: std::net::SocketAddr) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("GET /api/v1/devices HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer synthetic-observation-only-token-abcdefghijklmnopqrstuvwxyz\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(header.starts_with("HTTP/1.1 200"), "{text}");
    serde_json::from_str(body).unwrap()
}
fn free_address() -> std::net::SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn request(
    session: &mut support::Session,
    stream: &mut std::net::TcpStream,
    values: Value,
) -> String {
    let body = json!({"characteristics":values}).to_string();
    let request = format!(
        "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    session.request(stream, request.as_bytes()).0
}
#[tokio::test]
async fn paired_hap_away_uses_its_own_identity_and_native_module_frames() {
    let root = std::env::temp_dir().join(format!("away-hap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free_address();
    let web = free_address();
    assert_ne!(address, web);
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let controller = uuid::Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
    let mut modules = vec![];
    let mut devices = vec![];
    let mut reviews = vec![];
    for (id, address, profile) in [
        ("relay", "27", "r8i8_din_pro_v2_7_7"),
        ("dimmer", "38", "ad8_din_rs485_v2_2_1"),
    ] {
        modules.push(json!({"id":id,"address":address,"profile":profile,"verification":{"status":"verified","evidence":"Synthetic localhost only","verified_at_unix_ms":1}}));
        let mut names = vec![];
        for channel in 1..=8 {
            let name = format!("{id}:{channel}");
            names.push(name.clone());
            devices.push(json!({"id":name,"name":format!("Synthetic {id} {channel}"),"module_id":id,"channel":channel,"enabled":true}));
        }
        reviews.push(json!({"module_id":id,"devices":names,"review_evidence":"All eight synthetic outputs are lighting"}));
    }
    let config = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":modules}],"devices":devices});
    let rules = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"hap_expose":true,"review_evidence":"Synthetic operation","active_execution_policy":"cancel_affected_lighting","modules":reviews}});
    let controls = Arc::new(Mutex::new(Vec::<[u8; 5]>::new()));
    let captured = controls.clone();
    let expected_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_armed = expected_shutdown.clone();
    let gateway = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut relay = true;
        let mut dimmer = 75;
        loop {
            let mut frame = [0; 5];
            match socket.read_exact(&mut frame) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset
                    && shutdown_armed.load(std::sync::atomic::Ordering::Acquire) => break,
                Err(e) => panic!("synthetic gateway: {e}"),
            }
            let module = frame[1];
            assert!([0x27, 0x38].contains(&module));
            if frame[2] == 0 {
                assert_eq!(
                    frame,
                    if module == 0x27 {
                        [0xae, 0x27, 0, 0xf2, 0xee]
                    } else {
                        [0xae, 0x38, 0, 0, 0xee]
                    }
                );
                captured.lock().unwrap().push(frame);
                if module == 0x27 {
                    relay = false;
                } else {
                    dimmer = 0;
                }
            } else {
                assert!(frame[2] == 0xa0 || (0xa1..=0xa8).contains(&frame[2]));
            }
            let value = if module == 0x27 {
                if relay { 0xf1 } else { 0xf2 }
            } else {
                dimmer
            };
            let channels: Vec<u8> = if frame[2] == 0xa0 || frame[2] == 0 {
                (1..=8).collect()
            } else {
                vec![frame[2] - 0xa0]
            };
            let reply: Vec<_> = channels
                .into_iter()
                .flat_map(|channel| [0xae, module, 0xd0 + channel, value, 0xee])
                .collect();
            socket.write_all(&reply).unwrap();
        }
    });
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_combined(
            &serde_json::to_vec(&config).unwrap(),
            &serde_json::to_vec(&rules).unwrap(),
            &directory,
            Duration::from_secs(5),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: "synthetic-observation-only-token-abcdefghijklmnopqrstuvwxyz".into(),
                administrator: true,
                tls: None,
            },
            homesensed::hap_endpoint::Settings {
                address,
                scenes: vec![],
                motors: vec![],
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if service.is_finished() || tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if service.is_finished() {
        panic!("service exited before HAP readiness: {:?}", service.await);
    }
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::User,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let client = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
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
        assert!(header.starts_with("HTTP/1.1 200"));
        let graph: Value = serde_json::from_slice(&body).unwrap();
        let accessories = graph["accessories"].as_array().unwrap();
        let away = accessories
            .iter()
            .find(|a| {
                a["services"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|s| s["characteristics"].as_array().unwrap())
                    .any(|c| c["value"] == "离家全部关灯")
            })
            .expect("published Away operation");
        let power = |a: &Value| {
            a["services"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|s| s["characteristics"].as_array().unwrap())
                .find(|c| {
                    c["type"] == "25"
                        || c["type"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("00000025"))
                })
                .unwrap()["iid"]
                .as_u64()
                .unwrap()
        };
        let aid = away["aid"].as_u64().unwrap();
        let iid = power(away);
        assert!(
            request(
                &mut session,
                &mut stream,
                json!([{"aid":aid,"iid":iid,"value":false}])
            )
            .starts_with("HTTP/1.1 204")
        );
        let light = accessories
            .iter()
            .find(|a| a["aid"].as_u64() != Some(1) && a["aid"] != away["aid"])
            .unwrap();
        let mixed = request(
            &mut session,
            &mut stream,
            json!([{"aid":aid,"iid":iid,"value":true},{"aid":light["aid"],"iid":power(light),"value":true}]),
        );
        assert!(
            !mixed.starts_with("HTTP/1.1 204"),
            "mixed groups must reject atomically"
        );
        assert!(controls.lock().unwrap().is_empty());
        assert!(
            request(
                &mut session,
                &mut stream,
                json!([{"aid":aid,"iid":iid,"value":true}])
            )
            .starts_with("HTTP/1.1 204")
        );
        let (_, body) = session.request(
            &mut stream,
            format!("GET /characteristics?id={aid}.{iid} HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .as_bytes(),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["characteristics"][0]["value"],
            false
        );
    });
    client.await.unwrap();
    // Read-only observation credential is never used to submit the HAP action.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let state = actual_devices(web).await;
            let lights = state["devices"].as_array().unwrap();
            if lights.len() == 16
                && lights.iter().all(|d| {
                    d["quality"] == "Confirmed"
                        && (d["observed"]["on"] == false || d["observed"]["level"] == 0)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // All control/feedback checks are complete; only the intentional service
    // shutdown may close a socket with unread final periodic feedback via RST.
    expected_shutdown.store(true, std::sync::atomic::Ordering::Release);
    service.await.unwrap().unwrap();
    gateway.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE intent=x'01080000' AND origin='hap' AND status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    drop(sql);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
