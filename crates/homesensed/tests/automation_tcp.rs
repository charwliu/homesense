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
