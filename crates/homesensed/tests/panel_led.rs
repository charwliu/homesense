use homesense_core::DeviceId;
use homesense_storage::{Store, journal::Status};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    time::Duration,
};
fn document(endpoint: std::net::SocketAddr) -> hap::serde_json::Value {
    hap::serde_json::json!({"schema_version":1,"revision":1,
      "gateways":[{"id":"panel","endpoint":endpoint.to_string(),"modules":[{
        "address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual",
        "verification":{"status":"verified","evidence":"loopback fixture only","verified_at_unix_ms":1}}]}],
      "devices":[{"id":"key2","name":"Key 2","gateway":"panel","address":"41","channel":2,"enabled":true}]})
}
fn root() -> PathBuf {
    std::env::temp_dir().join(format!("homesense-led-{}", uuid::Uuid::new_v4()))
}
#[tokio::test]
async fn explicit_colors_use_audit_and_restart_does_not_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let doc = hap::serde_json::to_vec(&document(listener.local_addr().unwrap())).unwrap();
    let peer = std::thread::spawn(move || {
        for color in [0xf2, 0xf1] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut frame = [0; 5];
            stream.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x41, 0xc7, 0, 0xee]);
            stream.write_all(&[2]).unwrap();
            stream.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x41, 2, color, 0xee]);
            assert_eq!(stream.read(&mut frame).unwrap(), 0);
        }
    });
    let directory = root();
    for red in [false, true] {
        let report = homesensed::panel_led::set(
            &doc,
            &directory,
            &DeviceId::new("key2").unwrap(),
            red,
            true,
        )
        .await
        .unwrap();
        assert_eq!(report["red"], red);
        assert_eq!(report["physical_color_confirmed"], false);
        let store = Store::open(directory.join("runtime.sqlite")).unwrap();
        assert_eq!(
            store
                .command_status(
                    report["boot_id"].as_str().unwrap(),
                    report["command_id"].as_u64().unwrap()
                )
                .unwrap(),
            Some(Status::Written)
        );
    }
    peer.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn self_reset_unknown_or_interleaved_mode_never_writes_led() {
    for reply in [
        vec![1],
        vec![255],
        vec![2, 2],
        vec![2, 0xae, 0x12, 0xd4, 50, 0xee],
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let doc = hap::serde_json::to_vec(&document(listener.local_addr().unwrap())).unwrap();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut frame = [0; 5];
            stream.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x41, 0xc7, 0, 0xee]);
            stream.write_all(&reply).unwrap();
            assert_eq!(stream.read(&mut frame).unwrap(), 0);
        });
        let directory = root();
        let error = homesensed::panel_led::set(
            &doc,
            &directory,
            &DeviceId::new("key2").unwrap(),
            true,
            true,
        )
        .await
        .unwrap_err();
        assert!(error.contains("no LED write"), "{error}");
        peer.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
#[tokio::test]
async fn delayed_bus_feedback_after_mode_byte_prevents_led_admission() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let doc = hap::serde_json::to_vec(&document(listener.local_addr().unwrap())).unwrap();
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.set_nodelay(true).unwrap();
        let mut frame = [0; 5];
        stream.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x41, 0xc7, 0, 0xee]);
        stream.write_all(&[2]).unwrap();
        // Cross a socket read timeout: a plausible first byte is insufficient.
        std::thread::sleep(Duration::from_millis(80));
        stream.write_all(&[0xae, 0x12, 0xd4, 50, 0xee]).unwrap();
        assert_eq!(stream.read(&mut frame).unwrap(), 0);
    });
    let directory = root();
    let error = homesensed::panel_led::set(
        &doc,
        &directory,
        &DeviceId::new("key2").unwrap(),
        false,
        true,
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("Ambiguous") && error.contains("no LED write"),
        "{error}"
    );
    peer.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unverified_mode_scope_and_missing_coexistence_fail_before_state_creation() {
    for case in 0..4 {
        let mut doc = document("127.0.0.1:9".parse().unwrap());
        match case {
            0 => {
                doc["gateways"][0]["modules"][0]["verification"] =
                    hap::serde_json::json!({"status":"pending"})
            }
            1 => doc["gateways"][0]["modules"][0]["led_mode"] = "unknown".into(),
            2 => doc["devices"][0]["enabled"] = false.into(),
            _ => {}
        }
        let directory = root();
        assert!(
            homesensed::panel_led::set(
                &hap::serde_json::to_vec(&doc).unwrap(),
                &directory,
                &DeviceId::new("key2").unwrap(),
                true,
                case != 3
            )
            .await
            .is_err()
        );
        assert!(!directory.exists());
    }
}
