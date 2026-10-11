use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
async fn request(
    address: std::net::SocketAddr,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let text = body.as_ref().map_or(String::new(), |v| v.to_string());
    socket.write_all(format!("{} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",if body.is_some(){"POST"}else{"GET"},text.len()).as_bytes()).await.unwrap();
    let mut response = vec![];
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(response).unwrap();
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
#[tokio::test]
async fn managed_tcp_uses_one_owner_and_recovers_view_after_publication() {
    let root = std::env::temp_dir().join(format!("homesense-managed-{}", Uuid::new_v4()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    let writes = Arc::new(Mutex::new(vec![]));
    let received = writes.clone();
    let server = std::thread::spawn(move || {
        let level = Arc::new(AtomicU8::new(42));
        let mut workers = vec![];
        for _ in 0..2 {
            let (mut stream, _) = bus.accept().unwrap();
            let level = level.clone();
            let writes = received.clone();
            workers.push(std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                loop {
                    let mut frame = [0; 5];
                    match stream.read_exact(&mut frame) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(e) => panic!("bus: {e}"),
                    };
                    match frame[2] {
                        0xa1 => assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]),
                        0x01 => {
                            writes.lock().unwrap().push(frame[3]);
                            level.store(frame[3], Ordering::SeqCst);
                        }
                        _ => panic!("unexpected bus command"),
                    }
                    stream
                        .write_all(&[0xae, 0x15, 0xd1, level.load(Ordering::SeqCst), 0xee])
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let service_devices = serde_json::to_vec(&devices).unwrap();
    let service_rules = serde_json::to_vec(&rules).unwrap();
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_managed(
            &service_devices,
            &service_rules,
            &directory,
            Duration::from_secs(6),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                administrator: true,
                tls: None,
            },
        )
        .await
    });
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (code, snapshot) = request(address, "/api/v1/devices", None).await;
    assert_eq!(code, 200);
    assert_eq!(snapshot["devices"][0]["observed"]["level"], 42);
    let result=request(address,"/api/v1/device-commands",Some(serde_json::json!({"request_id":"one","device":"simulation:light","command":{"action":"brightness","level":30}}))).await;
    assert_eq!(result.1["outcome"], "accepted");
    for _ in 0..30 {
        if writes.lock().unwrap().as_slice() == [30] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    devices["revision"] = 2.into();
    devices["devices"][0]["name"] = "Renamed".into();
    let mut rules = rules;
    rules["configuration_revision"] = 2.into();
    let id = Uuid::new_v4();
    assert_eq!(request(address,&format!("/api/v1/config/drafts/{id}"),Some(serde_json::json!({"expected_revision":1,"revision":2,"devices":devices,"automation":rules}))).await.0,200);
    let activation = serde_json::json!({"request_id":"publish","boot":snapshot["boot"]});
    let result = request(
        address,
        &format!("/api/v1/config/activations/{id}"),
        Some(activation.clone()),
    )
    .await;
    assert_eq!(result.1["outcome"], "activated", "{result:?}");
    assert_eq!(
        request(
            address,
            &format!("/api/v1/config/activations/{id}"),
            Some(activation)
        )
        .await
        .0,
        200
    );
    let (_, next) = request(address, "/api/v1/devices", None).await;
    assert_eq!(next["revision"], 2);
    assert_eq!(next["devices"][0]["observed"]["level"], 30);
    let result=request(address,"/api/v1/device-commands",Some(serde_json::json!({"request_id":"two","device":"simulation:light","command":{"action":"brightness","level":40}}))).await;
    assert_eq!(result.1["outcome"], "accepted");
    service.await.unwrap().unwrap();
    server.join().unwrap();
    assert_eq!(*writes.lock().unwrap(), vec![30, 40]);
    assert_eq!(
        std::fs::read_dir(root.join("publication-backups"))
            .unwrap()
            .count(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn occupied_management_port_never_runs_startup_actions() {
    let root = std::env::temp_dir().join(format!("homesense-managed-bind-{}", Uuid::new_v4()));
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let server = std::thread::spawn(move || {
        let (mut stream, _) = bus.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut frame = [0; 5];
        stream.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]);
        stream.write_all(&[0xae, 0x15, 0xd1, 42, 0xee]).unwrap();
        assert_eq!(stream.read(&mut frame).unwrap(), 0);
    });
    let rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"synthetic","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}}]}]});
    let result = homesensed::automation_tcp::serve_managed(
        &serde_json::to_vec(&devices).unwrap(),
        &serde_json::to_vec(&rules).unwrap(),
        &root,
        Duration::from_secs(1),
        true,
        0,
        Duration::from_millis(20),
        homesensed::service::WebSettings {
            address: occupied.local_addr().unwrap(),
            secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
            administrator: false,
            tls: None,
        },
    )
    .await;
    assert!(result.is_err());
    server.join().unwrap();
    drop(occupied);
    std::fs::remove_dir_all(root).unwrap();
}
