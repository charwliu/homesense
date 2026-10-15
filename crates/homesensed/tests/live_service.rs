//! Live execution path against a loopback fixture; no field verification is implied.
use hap::{serde_json, storage::Storage};
use homesense_hap::storage::IdentityStore;
use homesensed::live_service::serve_live;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};
#[tokio::test]
async fn live_service_queries_before_serving_and_preserves_identity_across_restart() {
    let root = std::env::temp_dir().join(format!("homesense-live-{}", uuid::Uuid::new_v4()));
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = bus.local_addr().unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut document: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    document["gateways"][0]["endpoint"] = endpoint.to_string().into();
    document["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"loopback fixture, not field evidence","verified_at_unix_ms":1});
    let document = serde_json::to_vec(&document).unwrap();
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = queries.clone();
    let (query_ready, mut queries_ready) = tokio::sync::mpsc::channel(2);
    let (release_feedback, feedback_released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        for round in 0..2 {
            let (mut socket, _) = bus.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut frame = [0; 5];
            socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0xAE, 0x15, 0xA0, 0xF2, 0xEE]);
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            query_ready.blocking_send(round).unwrap();
            feedback_released.recv_timeout(Duration::from_secs(3)).unwrap();
            // Startup requires current feedback for every registered output,
            // including channels with no logical alias. This verified AD8 model
            // reads all eight channels in one A0 F2 request.
            for channel in 1..=8 {
                socket.write_all(&[0xAE, 0x15, 0xD0 + channel, 42, 0xEE]).unwrap();
            }
            assert_eq!(
                socket.read(&mut frame).unwrap(),
                0,
                "unauthenticated service must not send control"
            );
        }
    });
    let mut previous = None;
    for round in 0..2 {
        let bytes = document.clone();
        let directory = root.clone();
        let service = tokio::spawn(async move {
            serve_live(
                &bytes,
                &directory,
                address,
                Duration::from_millis(250),
                true,
            )
            .await
        });
        // Cold database and identity setup is not the listener-readiness phase.
        // Wait for the actual startup query, then prove that no HAP listener is
        // exposed until complete physical feedback is supplied. Keep the original
        // 150 x 2ms readiness loop and 250ms serving duration below.
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), queries_ready.recv())
                .await.unwrap(),
            Some(round)
        );
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(10)).is_err(),
            "HAP listener exposed before startup physical feedback");
        release_feedback.send(()).unwrap();
        let status = tokio::task::spawn_blocking(move || {
            for _ in 0..150 {
                if let Ok(mut socket) = TcpStream::connect_timeout(&address, Duration::from_millis(10)) {
                    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    socket.write_all(b"GET /accessories HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
                    let mut result = String::new();
                    socket.read_to_string(&mut result).unwrap();
                    return result;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("HAP listener never ready");
        }).await.unwrap();
        assert!(status.starts_with("HTTP/1.1 401"));
        assert_eq!(queries.load(std::sync::atomic::Ordering::SeqCst), round + 1);
        service.await.unwrap().unwrap();
        drop(TcpListener::bind(address).unwrap());
        let identity = IdentityStore::open(root.join("identity")).await.unwrap();
        let settings = identity.load_config().await.unwrap();
        let current = (
            settings.device_id,
            settings.device_ed25519_keypair.to_bytes(),
            settings.pin.to_string(),
            settings.configuration_number,
        );
        if let Some(old) = previous {
            assert_eq!(old, current);
        }
        previous = Some(current);
    }
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn live_service_rejects_simulated_config_and_missing_coexistence_before_state_creation() {
    let root = std::env::temp_dir().join(format!("homesense-live-reject-{}", uuid::Uuid::new_v4()));
    let document = include_bytes!("../../../config/simulation-home.json");
    let address = "127.0.0.1:51826".parse().unwrap();
    assert!(
        serve_live(document, &root, address, Duration::from_secs(1), false)
            .await
            .is_err()
    );
    assert!(
        serve_live(document, &root, address, Duration::from_secs(1), true)
            .await
            .is_err()
    );
    assert!(!root.exists());
}
