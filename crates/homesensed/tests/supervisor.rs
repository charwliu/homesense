//! Real loopback sockets only; the verified module is a test fixture, not field evidence.
use hap::{serde_json, storage::Storage};
use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent};
use homesense_hap::{
    requests::{Batch, Intent},
    storage::IdentityStore,
};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    reconnect::RetryPolicy,
    runtime_hap::{RuntimeHap, SupervisorOptions},
    transport_owner::TransportOwner,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};
async fn exercise(fail_query: bool, periodic: bool, multiple: bool) {
    let root = std::env::temp_dir().join(format!("homesense-supervisor-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut document: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    document["gateways"][0]["endpoint"] = address.to_string().into();
    document["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"isolated loopback test fixture only","verified_at_unix_ms":1});
    if multiple {
        let mut second = document["devices"][0].clone();
        second["id"] = "simulation:second".into();
        second["channel"] = 2.into();
        document["devices"].as_array_mut().unwrap().push(second);
    }
    let document = serde_json::to_vec(&document).unwrap();
    let config = homesense_config::Configuration::parse(&document, ExecutionContext::Live).unwrap();
    let path = root.join("runtime.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish_configuration(None, &document, ExecutionContext::Live, &[])
        .unwrap();
    let actor = Admission::open(path, Options::default(), |_| true)
        .await
        .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Live,
        uuid::Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let mut graph = RuntimeHap::new(
        &config,
        feedback.snapshot(),
        &identity,
        actor.clone(),
        if periodic { 200 } else { 5000 },
    )
    .await
    .unwrap();
    let first = hb_transport::Connection::connect(
        address,
        1,
        Duration::from_secs(1),
        Duration::from_millis(20),
    )
    .unwrap();
    let mut owner = ObservedTransport::new(
        TransportOwner::new(actor.clone(), [("simulation".into(), first)].into()),
        feedback,
    );
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let (queried, ready) = tokio::sync::oneshot::channel();
    let (accepted, queued) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut queried = Some(queried);
        let (first, _) = listener.accept().unwrap();
        drop(first); // Trigger EOF before any command has been submitted.
        if fail_query {
            let (mut unresponsive, _) = listener.accept().unwrap();
            unresponsive
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 5];
            unresponsive.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, [0xAE, 0x15, 0xA0, 0xF2, 0xEE]);
            // A connection without its queried feedback must never receive control.
            assert_eq!(unresponsive.read(&mut bytes).unwrap(), 0);
        }
        let (mut recovered, _) = listener.accept().unwrap();
        recovered
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = [0; 5];
        let query = 0xA0;
        recovered.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0xAE, 0x15, query, 0xF2, 0xEE]);
        recovered.write_all(&module_levels(42)).unwrap();
        if periodic {
            // A quiet device emits no unsolicited events. It must be queried
            // again before the previous observation expires.
            recovered.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, [0xAE, 0x15, query, 0xF2, 0xEE]);
            if multiple {
                // Queue control during this module read. Dispatch it after the
                // module completes, before issuing another module read.
                queried.take().unwrap().send(()).unwrap();
                queued.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            recovered.write_all(&module_levels(42)).unwrap();
        }
        if let Some(queried) = queried {
            queried.send(()).unwrap();
        }
        recovered.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0xAE, 0x15, 1, 30, 0xEE]);
        recovered
            .set_read_timeout(Some(Duration::from_millis(800)))
            .unwrap();
        // A post-write query must not wait for the normal 2.5-second refresh.
        recovered.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0xAE, 0x15, query, 0xF2, 0xEE]);
        recovered.write_all(&module_levels(35)).unwrap();
        if !periodic {
            // Continue observing the hardware transition during the burst.
            recovered.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, [0xAE, 0x15, 0xA0, 0xF2, 0xEE]);
            recovered.write_all(&module_levels(30)).unwrap();
        }
        stop.send(true).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    });
    let control = async {
        ready.await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result = actor
                .submit(Batch {
                    controller: uuid::Uuid::new_v4(),
                    intents: vec![Intent {
                        device: DeviceId::new("simulation:light").unwrap(),
                        command: Command::SetLevel {
                            level: Percent::new(30).unwrap(),
                            transition: LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                })
                .await;
            if let Ok(ids) = result {
                let _ = accepted.send(());
                break ids[0];
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let drive = graph.drive_supervised(
        &mut owner,
        &config,
        SupervisorOptions {
            initial_epoch: 1,
            retry: RetryPolicy {
                io_timeout: Duration::from_millis(20),
                ..Default::default()
            },
            cadence: Duration::from_millis(5),
            query_timeout_ms: 100,
            recovery_limit: 2,
            coexistence_confirmed: true,
        },
        receiver,
    );
    let (result, command) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(drive, control)
    })
    .await
    .unwrap();
    result.unwrap();
    server.join().unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), command).unwrap(),
        Some(Status::Written)
    );
    assert!(
        actor
            .submit(Batch {
                controller: uuid::Uuid::new_v4(),
                intents: vec![],
                subscriptions: vec![]
            })
            .await
            .is_err()
    );
    drop(graph);
    drop(owner);
    drop(identity);
    drop(store);
    drop(actor);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn tcp_eof_reconnects_queries_then_sends_only_new_command() {
    exercise(false, false, false).await;
}
#[tokio::test]
async fn query_timeout_reconnects_again_without_sending_control_to_unsynchronized_socket() {
    exercise(true, false, false).await;
}

#[tokio::test]
async fn quiet_device_is_refreshed_without_reconnecting() {
    exercise(false, true, false).await;
}

#[tokio::test]
async fn queued_control_is_dispatched_between_periodic_device_queries() {
    exercise(false, true, true).await;
}

fn module_levels(first: u8) -> Vec<u8> {
    (1..=8).flat_map(|channel| [0xae, 0x15, 0xd0 + channel, if channel == 1 { first } else { 42 }, 0xee]).collect()
}
