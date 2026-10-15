use hap::storage::Storage;
use homesense_hap::storage::IdentityStore;
use homesensed::service::serve_simulation;
use std::{
    net::{TcpListener, TcpStream},
    time::Duration,
};
use uuid::Uuid;
#[tokio::test]
async fn simulation_service_restarts_with_stable_identity_and_releases_listener() {
    let root = std::env::temp_dir().join(format!("homesense-service-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let document = include_bytes!("../../../config/simulation.json");
    let mut previous = None;
    for _ in 0..2 {
        let directory = root.clone();
        let service = tokio::spawn(async move {
            serve_simulation(document, &directory, address, Duration::from_millis(150)).await
        });
        // Keep the probe on this runtime so a delayed blocking-pool worker
        // cannot miss the entire 150 ms lifetime of a healthy listener.
        let reachable = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    break true;
                }
                if service.is_finished() {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or(false);
        let result = service.await.unwrap();
        assert!(reachable, "listener never became reachable: {result:?}");
        result.unwrap();
        let reservation = TcpListener::bind(address).unwrap();
        drop(reservation);
        let identity = IdentityStore::open(root.join("identity")).await.unwrap();
        let config = identity.load_config().await.unwrap();
        assert_eq!(config.name, "HomeSense Simulation");
        assert_eq!(config.category as u8, 2);
        let current = (
            config.pin.to_string(),
            config.device_id.to_string(),
            config.device_ed25519_keypair.to_bytes(),
            config.configuration_number,
            identity.load_aid_cache().await.unwrap(),
        );
        if let Some(previous) = &previous {
            assert_eq!(&current, previous);
        }
        previous = Some(current);
    }
    let modified = String::from_utf8(document.to_vec())
        .unwrap()
        .replace("模拟调光灯", "改名");
    assert!(
        serve_simulation(
            modified.as_bytes(),
            &root,
            address,
            Duration::from_millis(10)
        )
        .await
        .unwrap_err()
        .contains("stored configuration differs")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn occupied_port_fails_and_releases_runtime_for_restart() {
    let root = std::env::temp_dir().join(format!("homesense-service-busy-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let document = include_bytes!("../../../config/simulation.json");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        serve_simulation(document, &root, address, Duration::from_secs(30)),
    )
    .await;
    assert!(
        result.unwrap().is_err(),
        "occupied port must fail before duration expires"
    );
    drop(reservation);
    serve_simulation(document, &root, address, Duration::from_millis(50))
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn aborted_service_releases_listener_and_runtime_lease() {
    let root = std::env::temp_dir().join(format!("homesense-service-abort-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let document = include_bytes!("../../../config/simulation.json");
    let directory = root.clone();
    let task = tokio::spawn(async move {
        serve_simulation(document, &directory, address, Duration::from_secs(30)).await
    });
    let reachable = tokio::task::spawn_blocking(move || {
        for _ in 0..100 {
            if TcpStream::connect_timeout(&address, Duration::from_millis(10)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    assert!(reachable);
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("runtime.runtime.lock"))
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if lease.try_lock().is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime lease leaked after abort"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(lease);
    serve_simulation(document, &root, address, Duration::from_millis(50))
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn lan_simulation_rejects_wildcard_public_and_loopback_before_creating_state() {
    let root = std::env::temp_dir().join(format!("homesense-lan-reject-{}", Uuid::new_v4()));
    for address in [
        "0.0.0.0:32000",
        "127.0.0.1:32000",
        "8.8.8.8:32000",
        "[::]:32000",
        "[::1]:32000",
        "[ff02::1]:32000",
    ] {
        assert!(
            homesensed::service::serve_simulation_lan(
                include_bytes!("../../../config/simulation.json"),
                &root,
                address.parse().unwrap(),
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );
        assert!(!root.exists());
    }
}
