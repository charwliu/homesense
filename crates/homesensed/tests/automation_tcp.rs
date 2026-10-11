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
    let device_path = root.join("devices.json");
    let rules_path = root.join("rules.json");
    std::fs::write(&device_path, &devices).unwrap();
    std::fs::write(&rules_path, &rules).unwrap();
    for _ in 0..2 {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_homesensed"))
            .arg("serve-automation-tcp")
            .arg(&device_path)
            .arg(&rules_path)
            .arg(&root)
            .arg("1")
            .arg("--coexistence-confirmed")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("TCP automation stopped"));
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

#[test]
fn cli_rejects_missing_confirmation_and_invalid_duration_without_state() {
    let root = std::env::temp_dir().join(format!("automation-tcp-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let device = root.join("devices.json");
    let rules = root.join("rules.json");
    let state = root.join("state");
    std::fs::write(
        &device,
        include_bytes!("../../../config/simulation-home.json"),
    )
    .unwrap();
    std::fs::write(&rules, b"{}").unwrap();
    for (duration, confirmation) in [
        ("1", None),
        ("1", Some("--unknown")),
        ("invalid", Some("--coexistence-confirmed")),
        ("0", Some("--coexistence-confirmed")),
        ("86401", Some("--coexistence-confirmed")),
    ] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_homesensed"));
        cmd.arg("serve-automation-tcp")
            .arg(&device)
            .arg(&rules)
            .arg(&state)
            .arg(duration);
        if let Some(flag) = confirmation {
            cmd.arg(flag);
        }
        assert!(!cmd.output().unwrap().status.success());
        assert!(!state.exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn disconnect_after_write_cancels_delayed_scene_without_replay() {
    let root = std::env::temp_dir().join(format!(
        "automation-tcp-disconnect-{}",
        uuid::Uuid::new_v4()
    ));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}},{"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"loopback","max_lateness_ms":1000,"steps":[{"source":"later","offset_ms":1000,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]}"#;
    let server = std::thread::spawn(move || {
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
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let result = homesensed::automation_tcp::serve(
        &hap::serde_json::to_vec(&devices).unwrap(),
        rules,
        &root,
        Duration::from_secs(3),
        true,
    )
    .await;
    assert!(result.is_err(), "disconnect must end this unsupervised run");
    server.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    let count: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "delayed output must never be admitted");
    let pending: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    let scenes: i64 = sql
        .query_row(
            "SELECT count(*) FROM scene_run WHERE status IN ('pending','running')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scenes, 0, "disconnect must leave no live scene receipt");
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn shutdown_audit_failure_is_reported_instead_of_success() {
    let root =
        std::env::temp_dir().join(format!("automation-tcp-cleanup-{}", uuid::Uuid::new_v4()));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}},{"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"loopback","max_lateness_ms":1000,"steps":[{"source":"later","offset_ms":1000,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]}"#;
    let database = root.join("automation-tcp.sqlite");
    let server = std::thread::spawn(move || {
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
        let sql = rusqlite::Connection::open(database).unwrap();
        sql.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE ON scene_run WHEN NEW.status IN ('cancelled','failed','partial') BEGIN SELECT RAISE(ABORT,'injected cleanup audit failure'); END;").unwrap();
        drop(sql);
        assert_eq!(socket.read(&mut frame).unwrap(), 0);
    });
    let result = homesensed::automation_tcp::serve(
        &hap::serde_json::to_vec(&devices).unwrap(),
        rules,
        &root,
        Duration::from_millis(300),
        true,
    )
    .await;
    let error = result.unwrap_err();
    assert!(error.contains("shutdown"), "{error}");
    server.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    let count: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "delayed output must never be admitted");
    let pending: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn disconnect_without_pending_work_has_explicit_transport_outcome() {
    let root = std::env::temp_dir().join(format!(
        "automation-tcp-classified-{}",
        uuid::Uuid::new_v4()
    ));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}},{"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"loopback","max_lateness_ms":1000,"steps":[{"source":"later","offset_ms":1000,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]}"#;
    let mut document: hap::serde_json::Value = hap::serde_json::from_slice(rules).unwrap();
    document["rules"][0]["actions"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    document["timed_scenes"] = hap::serde_json::json!([]);
    let rules = hap::serde_json::to_vec(&document).unwrap();
    let server = std::thread::spawn(move || {
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
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let result = homesensed::automation_tcp::serve(
        &hap::serde_json::to_vec(&devices).unwrap(),
        &rules,
        &root,
        Duration::from_secs(3),
        true,
    )
    .await;
    assert!(result.unwrap_err().contains("gateway disconnected"));
    server.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    let count: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "delayed output must never be admitted");
    let pending: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    let scenes: i64 = sql
        .query_row(
            "SELECT count(*) FROM scene_run WHERE status IN ('pending','running')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scenes, 0, "disconnect must leave no live scene receipt");
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn supervised_reconnect_queries_again_without_repeating_startup() {
    for partial in [false, true] {
        for recovery_feedback in [true, false] {
            let root = std::env::temp_dir().join(format!(
                "automation-tcp-supervised-{}",
                uuid::Uuid::new_v4()
            ));
            let bus = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut devices: hap::serde_json::Value =
                hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
                    .unwrap();
            devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
            devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
            devices["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"verified","evidence":"loopback keypad","verified_at_unix_ms":1},"led_mode":"manual"}));
            devices["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"panel:1","name":"keypad fixture","gateway":"simulation","address":"41","channel":1,"enabled":true}));
            let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"test","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}}]}]}"#;
            let mut rules: hap::serde_json::Value = hap::serde_json::from_slice(rules).unwrap();
            rules["rules"][0]["actions"].as_array_mut().unwrap().push(hap::serde_json::json!({"source":"delayed","action":{"kind":"scene","id":"later","activate":true}}));
            rules["timed_scenes"] = hap::serde_json::json!([{"id":"later","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"later","offset_ms":500,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]);
            if partial {
                rules["rules"][0]["actions"]
                    .as_array_mut()
                    .unwrap()
                    .remove(0);
                rules["timed_scenes"][0]["steps"].as_array_mut().unwrap().insert(0,hap::serde_json::json!({"source":"first","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}));
            }
            let rules = hap::serde_json::to_vec(&rules).unwrap();
            let server = std::thread::spawn(move || {
                for round in 0..2 {
                    let (mut socket, _) = bus.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut frame = [0; 5];
                    socket.read_exact(&mut frame).unwrap();
                    assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]);
                    if recovery_feedback || round == 0 {
                        socket.write_all(&[0xae, 0x15, 0xd1, 42, 0xee]).unwrap();
                    }
                    if round == 0 {
                        socket.read_exact(&mut frame).unwrap();
                        assert_eq!(frame, [0xae, 0x15, 1, 30, 0xee]);
                        socket.shutdown(std::net::Shutdown::Both).unwrap();
                    } else {
                        assert_eq!(
                            socket.read(&mut frame).unwrap(),
                            0,
                            "recovery must not repeat startup output"
                        );
                    }
                }
            });
            if recovery_feedback {
                std::fs::create_dir_all(&root).unwrap();
                let device_file = root.join("devices.json");
                let rule_file = root.join("rules.json");
                std::fs::write(&device_file, hap::serde_json::to_vec(&devices).unwrap()).unwrap();
                std::fs::write(&rule_file, &rules).unwrap();
                let output = std::process::Command::new(env!("CARGO_BIN_EXE_homesensed"))
                    .arg("serve-automation-tcp")
                    .arg(device_file)
                    .arg(rule_file)
                    .arg(&root)
                    .args(["1", "--coexistence-confirmed", "--recoveries", "1"])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            } else {
                homesensed::automation_tcp::serve_supervised(
                    &hap::serde_json::to_vec(&devices).unwrap(),
                    &rules,
                    &root,
                    Duration::from_millis(700),
                    true,
                    1,
                )
                .await
                .unwrap();
            }
            server.join().unwrap();
            let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
            assert_eq!(
                sql.query_row("SELECT count(*) FROM rule_admission", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                sql.query_row(
                    "SELECT count(*) FROM scene_run WHERE definition='later' AND status=?1",
                    [if partial { "partial" } else { "cancelled" }],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            assert_eq!(
            sql.query_row(
                "SELECT count(*) FROM rule_chain_outcome WHERE error='causal session cancelled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
            drop(sql);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[tokio::test]
async fn supervision_stops_at_limit_or_failed_recovery_sync() {
    for synchronized in [true, false] {
        let root = std::env::temp_dir().join(format!(
            "automation-recovery-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let bus = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut devices: hap::serde_json::Value =
            hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
                .unwrap();
        devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
        devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
        let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
        let server = std::thread::spawn(move || {
            for round in 0..2 {
                let (mut socket, _) = bus.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut frame = [0; 5];
                socket.read_exact(&mut frame).unwrap();
                assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]);
                if round == 0 || synchronized {
                    socket.write_all(&[0xae, 0x15, 0xd1, 42, 0xee]).unwrap();
                }
                socket.shutdown(std::net::Shutdown::Both).unwrap();
            }
            bus
        });
        let error = homesensed::automation_tcp::serve_supervised(
            &hap::serde_json::to_vec(&devices).unwrap(),
            rules,
            &root,
            Duration::from_secs(3),
            true,
            1,
        )
        .await
        .unwrap_err();
        if synchronized {
            assert!(error.contains("recovery limit reached"), "{error}");
        }
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "no third connection after exhausted budget or failed synchronization"
        );
        let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
        assert_eq!(
            sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(sql);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn tcp_long_press_disconnect_does_not_resume_without_new_press() {
    let root = std::env::temp_dir().join(format!("automation-tcp-hold-{}", uuid::Uuid::new_v4()));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json"))
            .unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = hap::serde_json::json!({"status":"verified","evidence":"loopback only","verified_at_unix_ms":1});
    devices["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"verified","evidence":"loopback only","verified_at_unix_ms":1},"led_mode":"manual"}));
    devices["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"panel:1","name":"key","gateway":"simulation","address":"41","channel":1,"enabled":true}));
    let rules=hap::serde_json::to_vec(&hap::serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"gestures":[{"device":"panel:1","debounce_ms":30,"long_press_ms":600,"double_click_ms":300,"max_hold_ms":10000,"double_click_enabled":false}],"holds":{"interval_ms":100,"increment":5,"bindings":[{"key":"panel:1","device":"simulation:light","up":true,"release":"cancel_pending_only","enabled":true,"review_evidence":"loopback only"}]}})).unwrap();
    let server = std::thread::spawn(move || {
        for round in 0..2 {
            let (mut socket, _) = bus.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut frame = [0; 5];
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]);
            socket.write_all(&[0xae, 0x15, 0xd1, 30, 0xee]).unwrap();
            if round == 0 {
                std::thread::sleep(Duration::from_millis(100));
                socket.write_all(&[0xae, 0x41, 0xd1, 0xf1, 0xee]).unwrap();
                socket.read_exact(&mut frame).unwrap();
                assert_eq!(frame, [0xae, 0x15, 1, 35, 0xee]);
                socket.shutdown(std::net::Shutdown::Both).unwrap();
            } else {
                socket
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                let error = socket.read(&mut frame).unwrap_err();
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ));
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                socket.write_all(&[0xae, 0x41, 0xd1, 0xf1, 0xee]).unwrap();
                socket.read_exact(&mut frame).unwrap();
                assert_eq!(frame, [0xae, 0x15, 1, 35, 0xee]);
                socket.write_all(&[0xae, 0x41, 0xd1, 0xf2, 0xee]).unwrap();
                assert_eq!(
                    socket.read(&mut frame).unwrap(),
                    0,
                    "released new hold must stop"
                );
            }
        }
    });
    homesensed::automation_tcp::serve_supervised(
        &hap::serde_json::to_vec(&devices).unwrap(),
        &rules,
        &root,
        Duration::from_millis(2500),
        true,
        1,
    )
    .await
    .unwrap();
    server.join().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn online_publication_queries_new_mapping_before_acknowledgement() {
    use hap::serde_json::{json, to_vec, Value};
    for migrate in [false,true] {
    for reply in [true, false] {
    let root=std::env::temp_dir().join(format!("tcp-publication-{}",uuid::Uuid::new_v4()));
    let bus=TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","evidence":"loopback","verified_at_unix_ms":1});
    let rules=|revision| {
        let mut doc=json!({"schema_version":1,"configuration_revision":revision,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],"rules":[]});
        if migrate {
            doc["variables"][0]["kind"]="bool".into();
            doc["variable_migrations"]=json!([{"variable":"remember","source_revision":1,"source_type":"integer","conversion":"reset_unknown","review_evidence":"loopback explicit reset"}]);
        }
        to_vec(&doc).unwrap()
    };
    let initial=to_vec(&devices).unwrap();
    devices["revision"]=2.into();
    devices["devices"][0]["channel"]=2.into();
    let candidate=to_vec(&devices).unwrap();
    let (synced,ready)=tokio::sync::oneshot::channel();
    let server=std::thread::spawn(move || {
        let (mut old,_)=bus.accept().unwrap();
        old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut frame=[0;5];
        old.read_exact(&mut frame).unwrap();
        assert_eq!(frame,[0xAE,0x15,0xA1,0xF2,0xEE]);
        old.write_all(&[0xAE,0x15,0xD1,30,0xEE]).unwrap();
        synced.send(()).unwrap();
        let (mut fresh,_)=bus.accept().unwrap();
        fresh.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        fresh.read_exact(&mut frame).unwrap();
        assert_eq!(frame,[0xAE,0x15,0xA2,0xF2,0xEE]);
        if reply { fresh.write_all(&[0xAE,0x15,0xD2,40,0xEE]).unwrap(); }
        assert_eq!(fresh.read(&mut frame).unwrap(),0);
        assert_eq!(old.read(&mut frame).unwrap(),0);
    });
    let (publisher,receiver)=homesensed::automation_publication::channel();
    let old_rules=to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"remember","kind":"integer","lifetime":"persistent"}],
        "rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"remember","action":{"kind":"set","variable":"remember","value":{"kind":"integer","value":17}}},{"source":"test","action":{"kind":"scene","id":"later","activate":true}}]}],
        "timed_scenes":[{"id":"later","enabled":true,"review_evidence":"loopback","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":300,"device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}]}]
    })).unwrap();
    let service=homesensed::automation_tcp::serve_with_publications(&initial,&old_rules,&root,Duration::from_millis(700),true,0,Some(receiver));
    let client=async {
        ready.await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(publisher.publish(candidate.clone(),rules(2),99).await.is_err());
        let mut incompatible:Value=hap::serde_json::from_slice(&rules(2)).unwrap();
        incompatible["variables"][0]["kind"]="bool".into();
        incompatible.as_object_mut().unwrap().remove("variable_migrations");
        assert!(publisher.publish(candidate.clone(),to_vec(&incompatible).unwrap(),1).await.is_err());
        let outcome=publisher.publish(candidate,rules(2),1).await;
        if reply { assert_eq!(outcome.unwrap(),2); } else { assert!(outcome.is_err()); }
    };
    let (result,())=tokio::join!(service,client);
    assert_eq!(result.is_ok(),reply,"{result:?}");
    server.join().unwrap();
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_run WHERE definition='later' AND status='cancelled'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    for revision in [1,2] {
        let stored:(String,Option<i64>)=sql.query_row("SELECT kind,value FROM logical_variable WHERE revision=?1 AND name='remember'",[revision],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(stored,if migrate && revision==2 {("bool".into(),None)} else {("integer".into(),Some(17))});
    }
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
    }
    }
}

#[tokio::test]
async fn repeated_publication_then_recovery_uses_latest_endpoint_and_suppresses_startup() {
    use hap::serde_json::{json, to_vec, Value};
    let root=std::env::temp_dir().join(format!("tcp-republish-{}",uuid::Uuid::new_v4()));
    let old_bus=TcpListener::bind("127.0.0.1:0").unwrap();
    let new_bus=TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["endpoint"]=old_bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","evidence":"loopback","verified_at_unix_ms":1});
    let initial=to_vec(&devices).unwrap();
    let rules=|revision|to_vec(&json!({"schema_version":1,"configuration_revision":revision,"variables":[],
        "rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":revision*11,"ramp":false}}}]}]
    })).unwrap();
    devices["gateways"][0]["endpoint"]=new_bus.local_addr().unwrap().to_string().into();
    let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
    let server=std::thread::spawn(move || {
        let mut retained=Vec::new();
        for (index,channel) in [1u8,2,3,3].into_iter().enumerate() {
            let bus=if index==0 {&old_bus} else {&new_bus};
            let (mut socket,_)=bus.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut frame=[0;5];
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame,[0xAE,0x15,0xA0+channel,0xF2,0xEE]);
            socket.write_all(&[0xAE,0x15,0xD0+channel,30,0xEE]).unwrap();
            if index<3 {
                socket.read_exact(&mut frame).unwrap();
                assert_eq!(frame,[0xAE,0x15,channel,channel*11,0xEE]);
                started.send(channel).unwrap();
            }
            if index==2 {drop(socket);} else if index==3 {
                assert_eq!(socket.read(&mut frame).unwrap(),0,"recovery must not repeat startup");
            } else {retained.push(socket);}
        }
        for mut socket in retained {assert_eq!(socket.read(&mut [0;5]).unwrap(),0);}
    });
    let (publisher,receiver)=homesensed::automation_publication::channel();
    let old_rules=rules(1);
    let service=homesensed::automation_tcp::serve_with_publications(&initial,&old_rules,&root,Duration::from_millis(1500),true,1,Some(receiver));
    let client=async {
        assert_eq!(events.recv().await,Some(1));
        for revision in [2,3] {
            devices["revision"]=revision.into();
            devices["devices"][0]["channel"]=revision.into();
            assert_eq!(publisher.publish(to_vec(&devices).unwrap(),rules(revision),revision-1).await.unwrap(),revision);
            assert_eq!(events.recv().await,Some(revision as u8));
        }
        drop(publisher);
    };
    let (result,())=tokio::join!(service,client);
    result.unwrap();
    server.join().unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    drop(sql);
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,3);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn publication_partial_connection_set_is_dropped_without_commands() {
    use hap::serde_json::{json, to_vec, Value};
    let root=std::env::temp_dir().join(format!("tcp-partial-publish-{}",uuid::Uuid::new_v4()));
    let bus=TcpListener::bind("127.0.0.1:0").unwrap();
    let refused=TcpListener::bind("127.0.0.1:0").unwrap();
    let refused_address=refused.local_addr().unwrap();
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","evidence":"loopback","verified_at_unix_ms":1});
    let initial=to_vec(&devices).unwrap();
    let rules=|revision|to_vec(&json!({"schema_version":1,"configuration_revision":revision,"variables":[],"rules":[]})).unwrap();
    let mut second_gateway=devices["gateways"][0].clone();
    second_gateway["id"]="zz-failed".into();
    second_gateway["endpoint"]=refused_address.to_string().into();
    devices["gateways"].as_array_mut().unwrap().push(second_gateway);
    let mut second_device=devices["devices"][0].clone();
    second_device["id"]="second:light".into();
    second_device["gateway"]="zz-failed".into();
    devices["devices"].as_array_mut().unwrap().push(second_device);
    devices["revision"]=2.into();
    let (synced,ready)=tokio::sync::oneshot::channel();
    let server=std::thread::spawn(move || {
        let (mut old,_)=bus.accept().unwrap();
        old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut frame=[0;5];
        old.read_exact(&mut frame).unwrap();
        old.write_all(&[0xAE,0x15,0xD1,30,0xEE]).unwrap();
        drop(refused);
        synced.send(()).unwrap();
        let (mut partial,_)=bus.accept().unwrap();
        partial.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(partial.read(&mut frame).unwrap(),0,"incomplete connection set must not query or write");
        assert_eq!(old.read(&mut frame).unwrap(),0);
    });
    let (publisher,receiver)=homesensed::automation_publication::channel();
    let old_rules=rules(1);
    let service=homesensed::automation_tcp::serve_with_publications(&initial,&old_rules,&root,Duration::from_secs(2),true,0,Some(receiver));
    let client=async {
        ready.await.unwrap();
        let error=publisher.publish(to_vec(&devices).unwrap(),rules(2),1).await.unwrap_err();
        assert!(error.contains("zz-failed"),"{error}");
        assert!(error.contains("inspect active revision"),"{error}");
    };
    let (result,())=tokio::join!(service,client);
    assert!(result.unwrap_err().contains("zz-failed"));
    server.join().unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(sql);
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn publication_partial_feedback_never_enables_startup() {
    use hap::serde_json::{json, to_vec, Value};
    let root=std::env::temp_dir().join(format!("tcp-partial-feedback-{}",uuid::Uuid::new_v4()));
    let bus=TcpListener::bind("127.0.0.1:0").unwrap();
    let refused=TcpListener::bind("127.0.0.1:0").unwrap();
    let refused_address=refused.local_addr().unwrap();
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","evidence":"loopback","verified_at_unix_ms":1});
    let initial=to_vec(&devices).unwrap();
    let rules=|revision|to_vec(&json!({"schema_version":1,"configuration_revision":revision,"variables":[],"rules":[]})).unwrap();
    let mut second_gateway=devices["gateways"][0].clone();
    second_gateway["id"]="zz-failed".into();
    second_gateway["endpoint"]=refused_address.to_string().into();
    devices["gateways"].as_array_mut().unwrap().push(second_gateway);
    let mut second_device=devices["devices"][0].clone();
    second_device["id"]="zz:light".into();
    second_device["gateway"]="zz-failed".into();
    devices["devices"].as_array_mut().unwrap().push(second_device);
    devices["revision"]=2.into();
    let (synced,ready)=tokio::sync::oneshot::channel();
    let server=std::thread::spawn(move || {
        let (mut old,_)=bus.accept().unwrap();
        old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut frame=[0;5];
        old.read_exact(&mut frame).unwrap();
        old.write_all(&[0xAE,0x15,0xD1,30,0xEE]).unwrap();
        synced.send(()).unwrap();
        let (mut partial,_)=bus.accept().unwrap();
        partial.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (mut silent,_)=refused.accept().unwrap();
        silent.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        partial.read_exact(&mut frame).unwrap();
        assert_eq!(frame,[0xAE,0x15,0xA1,0xF2,0xEE]);
        partial.write_all(&[0xAE,0x15,0xD1,40,0xEE]).unwrap();
        silent.read_exact(&mut frame).unwrap();
        assert_eq!(frame,[0xAE,0x15,0xA1,0xF2,0xEE]);
        assert_eq!(silent.read(&mut frame).unwrap(),0,"missing feedback must close new connections");
        assert_eq!(partial.read(&mut frame).unwrap(),0,"partial feedback must not enable startup");
        assert_eq!(old.read(&mut frame).unwrap(),0);
    });
    let (publisher,receiver)=homesensed::automation_publication::channel();
    let old_rules=rules(1);
    let service=homesensed::automation_tcp::serve_with_publications(&initial,&old_rules,&root,Duration::from_secs(2),true,0,Some(receiver));
    let candidate_rules=to_vec(&json!({"schema_version":1,"configuration_revision":2,"variables":[],
        "rules":[{"id":"startup","enabled":true,"review_evidence":"loopback","source":"test","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"test","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}}]}]
    })).unwrap();
    let client=async {
        ready.await.unwrap();
        let error=publisher.publish(to_vec(&devices).unwrap(),candidate_rules,1).await.unwrap_err();

        assert!(error.contains("inspect active revision"),"{error}");
    };
    let (result,())=tokio::join!(service,client);
    assert!(result.unwrap_err().contains("inspect active revision"));
    server.join().unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(sql);
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    std::fs::remove_dir_all(root).unwrap();
}
