#[path="support/runtime_outage.rs"]
mod outage;
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

// Retain the historical test identifier for inventory continuity. A missing
// periodic response now gates controls until the original service deadline.
#[tokio::test]
async fn missing_periodic_response_stops_at_recovery_limit_without_control_writes() {
    let root = std::env::temp_dir().join(format!("idle-feedback-timeout-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["refresh_policy"]=serde_json::json!({"idle_interval_ms":500,"active_interval_ms":100,"backoff_ms":[200,300],"quiet_rounds_per_step":2,"round_budget_ms":3500,"observation_grace_ms":1000});
    raw["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    raw["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback only","verified_at_unix_ms":1});
    let (missing,wait_missing)=tokio::sync::oneshot::channel();
    let gateway = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(7))).unwrap();
        let mut frame = [0; 5];let mut queries=vec![];
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);queries.push(frame);
        socket.write_all(&module_levels(0x15, 42)).unwrap();
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);queries.push(frame);
        // The fresh query receives no answer: the old observation is not renewed.
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);queries.push(frame);
        // One bounded read-only retry is also unanswered. Stop accepting new
        // streams so subsequent supervised connection attempts fail explicitly.
        drop(bus);let _=missing.send(());
        assert_eq!(socket.read(&mut frame).unwrap(), 0);queries
    });
    let web=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let web_address=web.local_addr().unwrap();drop(web);
    let directory=root.clone();let input=serde_json::to_vec(&raw).unwrap();
    let started = std::time::Instant::now();
    let service=tokio::spawn(async move {homesensed::automation_tcp::serve_managed(
        &input,br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
        &directory,Duration::from_secs(6),true,0,Duration::from_millis(20),
        homesensed::service::WebSettings{address:web_address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None},
    ).await});
    tokio::time::timeout(Duration::from_secs(5),wait_missing).await.unwrap().unwrap();
    let token="abcdefghijklmnopqrstuvwxyz0123456789";
    outage::wait_unavailable(web_address,token,"1",&service).await;
    outage::assert_gated(web_address,token,"1","simulation:light").await;
    outage::assert_empty_journal(&root);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!service.is_finished(),"missing periodic feedback must not consume the six-second service window");
    outage::assert_gated(web_address,token,"1","simulation:light").await;
    tokio::time::timeout(Duration::from_secs(8),service).await.unwrap().unwrap().unwrap();
    assert!(started.elapsed()>=Duration::from_secs(6),"service ended before its original deadline");
    assert!(tokio::net::TcpStream::connect(web_address).await.is_err());
    assert_eq!(gateway.join().unwrap(),vec![[0xae,0x15,0xa0,0xf2,0xee];3]);
    let db = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

async fn devices(address: std::net::SocketAddr) -> serde_json::Value {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("GET /api/v1/devices HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(header.starts_with("HTTP/1.1 200"), "{text}");
    serde_json::from_str(body).unwrap()
}

#[tokio::test]
async fn idle_combined_light_stays_readable_over_encrypted_hap_and_web() {
    let root = std::env::temp_dir().join(format!("idle-feedback-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web_address = web.local_addr().unwrap();
    drop(web);
    let hap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hap_address = hap.local_addr().unwrap();
    drop(hap);
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["refresh_policy"]=serde_json::json!({"idle_interval_ms":500,"active_interval_ms":100,"backoff_ms":[200,300],"quiet_rounds_per_step":2,"round_budget_ms":3500,"observation_grace_ms":1000});
    raw["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    let verified = serde_json::json!({"status":"verified","evidence":"synthetic loopback only","verified_at_unix_ms":1});
    raw["gateways"][0]["modules"][0]["verification"] = verified.clone();
    for (address, profile) in [
        ("03", "r8i8_din_pro_v2_7_7"),
        ("22", "c4_din_pro_v2_7_7"),
        ("41", "ts6_86_rs485_v2_6_5_6key"),
    ] {
        raw["gateways"][0]["modules"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"address":address,"profile":profile,"verification":verified}));
    }
    raw["devices"][0]["supply_relay"] = "supply".into();
    raw["devices"][0]["default_on_level"] = 50.into();
    for (id, address) in [("supply", "03"), ("motor", "22"), ("panel", "41")] {
        let mut device = serde_json::json!({"id":id,"name":id,"gateway":"simulation","address":address,"channel":1,"enabled":true});
        if id == "motor" {
            device["motor_calibration"] = serde_json::json!({"open_direction":"up","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1});
        }
        raw["devices"].as_array_mut().unwrap().push(device);
    }
    let queries = Arc::new(Mutex::new(vec![]));
    let controls = Arc::new(Mutex::new(vec![]));
    let received_queries = queries.clone();
    let received_controls = controls.clone();
    let gateway = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut level = 42;
        loop {
            let mut frame = [0; 5];
            match socket.read_exact(&mut frame) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("{e}"),
            }
            assert!(
                matches!(frame[1], 0x03 | 0x15),
                "queried event-only panel or motor: {frame:?}"
            );
            if (0xa0..=0xa8).contains(&frame[2]) {
                assert_eq!(frame[3], 0xf2);
                received_queries.lock().unwrap().push(frame[1]);
            } else {
                received_controls.lock().unwrap().push(frame);
                if frame[1] == 0x15 {
                    level = frame[3];
                }
            }
            let value = if frame[1] == 0x03 { 0xf1 } else { level };
            let reply = if frame[2] == 0xa0 {
                assert_eq!(frame[1], 0x15, "PRO relay must use individual queries");
                module_levels(frame[1], value)
            } else {
                let channel = if frame[2] >= 0xa1 { frame[2] - 0xa0 } else { 1 };
                vec![0xae, frame[1], 0xd0 + channel, value, 0xee]
            };
            socket.write_all(&reply).unwrap();
        }
    });
    let mut identity = homesense_hap::storage::IdentityStore::open(root.join("identity"))
        .await
        .unwrap();
    let config = hap::Config {
        host: hap_address.ip(),
        port: hap_address.port(),
        ..Default::default()
    };
    identity.save_config(&config).await.unwrap();
    identity.initialize_aid_cache().await.unwrap();
    homesense_hap::graph::LightGraph::build(
        &identity,
        "HomeSense",
        vec![homesense_hap::graph::LightSpec {
            device: homesense_core::DeviceId::new("simulation:light").unwrap(),
            name: "模拟调光灯".into(),
            dimmable: true,
            default_on_level: Some(homesense_core::Percent::new(50).unwrap()),
        }],
    )
    .await
    .unwrap();
    let caller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            caller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let id = config.device_id.to_string();
    let key = config.device_ed25519_keypair.verifying_key();
    let directory = root.clone();
    let device_bytes = serde_json::to_vec(&raw).unwrap();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_combined(
            &device_bytes,
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory,
            Duration::from_secs(11),
            true,
            0,
            Duration::from_millis(100),
            homesensed::service::WebSettings {
                address: web_address,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
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
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(web_address).await.is_ok() {
            break;
        }
        assert!(!service.is_finished());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // No unsolicited events or controls: exceed the original five-second lease.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let view = devices(web_address).await;
    assert_eq!(view["devices"].as_array().unwrap().len(), 3);
    let light = view["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "simulation:light")
        .unwrap();
    assert_eq!(light["quality"], "Confirmed");
    assert_eq!(light["observed"]["level"], 42);
    for device in view["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["id"] != "simulation:light")
    {
        assert_eq!(device["quality"], "Unknown");
        assert!(device["observed"].is_null());
    }
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(hap_address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut session = support::verify(&mut stream, caller, &signing, &id, &key);
        let (header, body) = session.request(
            &mut stream,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(header.starts_with("HTTP/1.1 200"), "{header}");
        let graph: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(graph["accessories"].as_array().unwrap().len(), 2);
    })
    .await
    .unwrap();
    service.await.unwrap().unwrap();
    gateway.join().unwrap();
    let queries = queries.lock().unwrap();
    assert!(queries.iter().filter(|&&address| address == 0x03).count() > 1);
    assert!(queries.iter().filter(|&&address| address == 0x15).count() > 1);
    assert!(controls.lock().unwrap().is_empty());
    drop(queries);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn lost_periodic_reply_requeries_same_connection_and_requires_new_observation() {
    let root = std::env::temp_dir().join(format!("idle-feedback-retry-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web_address = web.local_addr().unwrap();
    drop(web);
    let mut raw: serde_json::Value = serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["refresh_policy"]=serde_json::json!({"idle_interval_ms":500,"active_interval_ms":100,"backoff_ms":[200,300],"quiet_rounds_per_step":2,"round_budget_ms":3500,"observation_grace_ms":1000});
    raw["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    raw["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback only","verified_at_unix_ms":1});
    let server = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        let mut frame = [0; 5];
        for index in 0..3 {
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee], "only status reads may retry");
            match index {
                0 => socket.write_all(&module_levels(0x15, 42)).unwrap(),
                1 => {}, // lose exactly the first periodic reply
                _ => socket.write_all(&module_levels(0x15, 43)).unwrap(),
            }
        }
        loop {
            let n = socket.read(&mut frame).unwrap();
            if n == 0 {break;}
            let mut filled = n;
            while filled < frame.len() {let n=socket.read(&mut frame[filled..]).unwrap();assert!(n>0,"EOF within query frame");filled+=n;}
            assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee], "later periodic queries are allowed, controls are not");
            socket.write_all(&module_levels(0x15, 43)).unwrap();
        }
    });
    let bytes = serde_json::to_vec(&raw).unwrap();
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_managed(&bytes,
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory, Duration::from_secs(6), true, 0, Duration::from_millis(20),
            homesensed::service::WebSettings {address:web_address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(web_address).await.is_ok() {break;}
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    loop {
        let state = devices(web_address).await;
        if state["devices"][0]["observed"]["level"] == 43 {break;}
        assert!(tokio::time::Instant::now() < deadline, "new response not projected: {state}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(service.await.unwrap().is_ok());
    server.join().unwrap();
    let db = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM command_journal", [], |row|row.get::<_,i64>(0)).unwrap(), 0);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

fn module_levels(address: u8, level: u8) -> Vec<u8> {
    (1..=8).flat_map(|channel| [0xae, address, 0xd0 + channel, level, 0xee]).collect()
}
