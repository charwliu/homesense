#[path = "../../test-support/cli.rs"]
mod test_cli;
use homesense_core::{DeviceId, MotorAction};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};
#[tokio::test]
async fn directions_stop_and_durable_audit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut doc: hap::serde_json::Value =
        hap::serde_json::from_str(include_str!("../../../config/test/legacy-study/motor.json")).unwrap();
    doc["gateways"][0]["endpoint"] = listener.local_addr().unwrap().to_string().into();
    let bytes = hap::serde_json::to_vec(&doc).unwrap();
    let peer = std::thread::spawn(move || {
        for direction in [0xf1, 0xf2] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut frame = [0; 5];
            stream.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x22, 2, direction, 0xee]);
            stream.write_all(&[0xae, 0x12, 0xd4, 30, 0xee]).unwrap();
            let start = Instant::now();
            stream.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xae, 0x22, 2, 0xf0, 0xee]);
            assert!(start.elapsed() >= Duration::from_millis(2900));
            assert_eq!(stream.read(&mut frame).unwrap(), 0);
        }
    });
    let dir = std::env::temp_dir().join(format!("hs-motor-{}", uuid::Uuid::new_v4()));
    let device = DeviceId::new("control4:290").unwrap();
    assert!(
        homesensed::motor_commission::run(&bytes, &dir, &device, MotorAction::Open, false)
            .await
            .is_err()
    );
    for action in [MotorAction::Open, MotorAction::Close] {
        let report = homesensed::motor_commission::run(&bytes, &dir, &device, action, true)
            .await
            .unwrap();
        assert_eq!(report["teardown_drained_bytes"], 5);
        let store = homesense_storage::Store::open(dir.join("runtime.sqlite")).unwrap();
        for item in report["commands"].as_array().unwrap() {
            assert_eq!(
                store
                    .command_status(
                        report["boot_id"].as_str().unwrap(),
                        item["command_id"].as_u64().unwrap()
                    )
                    .unwrap(),
                Some(homesense_storage::journal::Status::Written)
            );
        }
    }
    peer.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn broken_stdout_does_not_prevent_scheduled_stop() {
    use std::process::{Command, Stdio};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut doc: hap::serde_json::Value =
        hap::serde_json::from_str(include_str!("../../../config/test/legacy-study/motor.json")).unwrap();
    doc["gateways"][0]["endpoint"] = listener.local_addr().unwrap().to_string().into();
    let dir = std::env::temp_dir().join(format!("hs-motor-pipe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.json");
    std::fs::write(&config, hap::serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut child = Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed")))
        .arg("motor-test")
        .arg(&config)
        .arg(dir.join("state"))
        .args(["control4:290", "open", "--coexistence-confirmed"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut frame = [0; 5];
    stream.read_exact(&mut frame).unwrap();
    assert_eq!(frame, [0xae, 0x22, 2, 0xf1, 0xee]);
    stream.read_exact(&mut frame).unwrap();
    assert_eq!(frame, [0xae, 0x22, 2, 0xf0, 0xee]);
    assert!(!child.wait().unwrap().success()); // Only final reporting fails.
    std::fs::remove_dir_all(dir).unwrap();
}
