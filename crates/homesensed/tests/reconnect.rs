use homesense_core::ExecutionContext;
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    reconnect::{RetryPolicy, reconnect_tcp},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};
use tokio::sync::watch;

async fn fixture() -> (std::path::PathBuf, Admission) {
    let root = std::env::temp_dir().join(format!("homesense-connect-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation-home.json"),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    drop(store);
    let actor = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    (root, actor)
}
#[tokio::test]
async fn connector_receives_on_new_epoch_without_reopening_control_or_sending() {
    let (root, actor) = fixture().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        socket.write_all(&[0xAE, 0x15, 0xD1, 30, 0xEE]).unwrap();
        let mut byte = [0];
        let read = socket.read(&mut byte);
        assert!(
            matches!(read, Ok(0))
                || read.is_err_and(|e| matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )),
            "connector unexpectedly sent application bytes"
        );
    });
    let (_sender, stop) = watch::channel(false);
    let (mut owner, epoch) = reconnect_tcp(
        actor.clone(),
        BTreeMap::from([("simulation".into(), address)]),
        7,
        RetryPolicy::default(),
        true,
        stop,
    )
    .await
    .unwrap();
    assert_eq!(epoch, 8);
    assert!(actor.dispatch().await.is_err());
    let read = owner.receive("simulation").await.unwrap();
    assert_eq!(read.transport_epoch, 8);
    assert!(matches!(read.result, hb_transport::Receive::Frames { .. }));
    drop(owner);
    server.join().unwrap();
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn connector_requires_coexistence_and_honors_stop() {
    let (root, actor) = fixture().await;
    let endpoints = BTreeMap::from([("simulation".into(), "127.0.0.1:9".parse().unwrap())]);
    let (_sender, stop) = watch::channel(true);
    assert!(
        reconnect_tcp(
            actor.clone(),
            endpoints.clone(),
            1,
            RetryPolicy::default(),
            false,
            stop.clone()
        )
        .await
        .is_err()
    );
    // Rejected parameters never touched admission.
    assert!(actor.dispatch().await.unwrap().is_none());
    assert!(
        reconnect_tcp(
            actor.clone(),
            endpoints,
            1,
            RetryPolicy::default(),
            true,
            stop
        )
        .await
        .is_err()
    );
    assert!(actor.recover_synchronized(2, 1).await.is_err());
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_connects_exhaust_bounded_retries_with_backoff() {
    let (root, actor) = fixture().await;
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let (_sender, stop) = watch::channel(false);
    let policy = RetryPolicy {
        attempts: 3,
        initial_delay: Duration::from_millis(10),
        maximum_delay: Duration::from_millis(20),
        connect_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let result = reconnect_tcp(
        actor.clone(),
        BTreeMap::from([("simulation".into(), address)]),
        1,
        policy,
        true,
        stop,
    )
    .await;
    assert!(matches!(result, Err(ref e) if e.contains("attempts exhausted")));
    assert!(start.elapsed() >= Duration::from_millis(30));
    assert!(actor.dispatch().await.is_err());
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
