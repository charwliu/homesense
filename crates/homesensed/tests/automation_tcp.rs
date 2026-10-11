use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};
#[tokio::test]
async fn tcp_automation_synchronizes_before_startup_and_preserves_other_state() {
    let root = std::env::temp_dir().join(format!("automation-tcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("runtime.sqlite"), b"untouched").unwrap();
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    let id = devices["devices"][0]["id"].as_str().unwrap();
    let rules=hap::serde_json::to_vec(&hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":id,"command":{"kind":"level","value":30,"ramp":false}}}]}]})).unwrap();
    let devices = hap::serde_json::to_vec(&devices).unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = bus.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut frame = [0; 5];
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xAE, 0x15, 0xA1, 0xF2, 0xEE]);
            socket.write_all(&[0xAE, 0x15, 0xD1, 42, 0xEE]).unwrap();
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xAE, 0x15, 0x01, 30, 0xEE]);
            assert_eq!(
                socket.read(&mut frame).unwrap(),
                0,
                "no repeated startup or replay"
            );
        }
    });
    for _ in 0..2 {
        homesensed::automation_tcp::serve(
            &devices,
            &rules,
            &root,
            Duration::from_millis(250),
            true,
        )
        .await
        .unwrap();
    }
    server.join().unwrap();
    assert_eq!(
        std::fs::read(root.join("runtime.sqlite")).unwrap(),
        b"untouched"
    );
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    let count: i64 = sql
        .query_row(
            "SELECT count(*) FROM rule_admission WHERE rule_id='startup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn rejects_unconfirmed_or_unverified_before_creating_state() {
    let root = std::env::temp_dir().join(format!("automation-tcp-reject-{}", uuid::Uuid::new_v4()));
    for confirmed in [false, true] {
        assert!(
            homesensed::automation_tcp::serve(
                include_bytes!("../../../config/simulation-home.json"),
                b"{}",
                &root,
                Duration::from_secs(1),
                confirmed
            )
            .await
            .is_err()
        );
    }
    assert!(!root.exists());
}

#[tokio::test]
async fn missing_initial_feedback_never_executes_startup() {
    let root = std::env::temp_dir().join(format!(
        "automation-tcp-no-feedback-{}",
        uuid::Uuid::new_v4()
    ));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}}]}]}"#;
    let server = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut frame = [0; 5];
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xAE, 0x15, 0xA1, 0xF2, 0xEE]);
        assert_eq!(
            socket.read(&mut frame).unwrap(),
            0,
            "unsynchronized startup must send no controls"
        );
    });
    let result = homesensed::automation_tcp::serve(
        &hap::serde_json::to_vec(&devices).unwrap(),
        rules,
        &root,
        Duration::from_millis(250),
        true,
    )
    .await;
    assert!(result.is_err());
    server.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    let count: i64 = sql
        .query_row("SELECT count(*) FROM rule_admission", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
