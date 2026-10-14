//! Actual encrypted HTTP -> RuntimeHap -> durable Admission. No physical bus.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    server::{IpServer, Server},
    storage::Storage,
};
use homesense_core::ExecutionContext;
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    runtime_hap::RuntimeHap,
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[tokio::test]
async fn encrypted_write_is_durable_without_fabricating_observed_state() {
    exercise(false).await;
}
#[tokio::test]
async fn encrypted_combined_light_queues_both_outputs_and_projects_feedback() {
    exercise(true).await;
}
async fn exercise(combined: bool) {
    let directory = std::env::temp_dir().join(format!("homesense-http-runtime-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let database = directory.join("runtime.sqlite");
    let mut value: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    if combined {
        value["gateways"][0]["modules"][0]["profile"] = "ad8_din_rs485_v2_2_1".into();
        value["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
        value["devices"][0]["supply_relay"] = "relay".into();
        value["devices"][0]["default_on_level"] = 100.into();
        value["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":"relay","name":"Supply","gateway":"simulation","address":"03","channel":4,"enabled":true}));
    }
    let bytes = hap::serde_json::to_vec(&value).unwrap();
    let document = bytes.as_slice();
    let mut runtime_store = Store::open(&database).unwrap();
    runtime_store
        .publish_configuration(None, document, ExecutionContext::Simulation, &[])
        .unwrap();
    runtime_store.remember_light_levels(1, &[(homesense_core::DeviceId::new("simulation:light").unwrap(), homesense_core::Percent::new(40).unwrap())]).unwrap();
    drop(runtime_store);
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::generate(&mut rand::rng());
    let actor = Admission::open(
        database.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    let config =
        homesense_config::Configuration::parse(document, ExecutionContext::Simulation).unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut identity = homesense_hap::storage::IdentityStore::open(directory.join("identity"))
        .await
        .unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let mut graph = RuntimeHap::new(&config, feedback.snapshot(), &identity, actor.clone(), 5000)
        .await
        .unwrap();
    assert_eq!(graph.lights.len(), 1);
    let binding = graph.lights[0].0.clone();
    let server = IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    graph.install(&server).await.unwrap();
    identity
        .save_pairing(&Pairing::new(
            controller,
            Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let boot = actor.boot_id().to_owned();
    let transport = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "simulation".into(),
            hb_transport::Connection::for_simulation(
                std::io::Cursor::new(if combined {
                    vec![0xAE, 0x15, 0xD1, 27, 0xEE, 0xAE, 0x03, 0xD4, 0xF1, 0xEE]
                } else {
                    vec![0xAE, 0x15, 0xD1, 27, 0xEE]
                }),
                1,
            ),
        )]
        .into(),
    );
    let mut owner = homesensed::observed_transport::ObservedTransport::new(transport, feedback);
    let (advance, mut steps) = tokio::sync::mpsc::channel(1);
    let (published, ready) = std::sync::mpsc::sync_channel(1);
    let client = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
                Ok(stream) => break stream,
                Err(error) => {
                    assert!(Instant::now() < deadline, "{error}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut session = support::verify(
            &mut stream,
            controller,
            &signing,
            &accessory_id,
            &accessory_key,
        );
        let iid = binding.brightness_iid.unwrap();
        let read = format!(
            "GET /characteristics?id={}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            binding.aid, iid
        );
        let (headers, body) = session.request(&mut stream, read.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 207"), "{headers}");
        let before: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(before["characteristics"][0]["status"], -70402);
        let body =
            hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":iid,"value":30}]})
                .to_string();
        let write = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (headers, body) = session.request(&mut stream, write.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 204"), "{headers} {body:?}");
        assert_eq!(
            Store::open(&database)
                .unwrap()
                .command_status(&boot, 1)
                .unwrap(),
            Some(Status::Accepted)
        );
        if combined {
            assert_eq!(
                Store::open(&database)
                    .unwrap()
                    .command_status(&boot, 2)
                    .unwrap(),
                Some(Status::Accepted)
            );
        }
        let (headers, body) = session.request(&mut stream, read.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 207"), "{headers}");
        let after: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(after["characteristics"][0]["status"], -70402);
        assert!(after["characteristics"][0].get("value").is_none());
        let subscription =
            hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":iid,"ev":true}]})
                .to_string();
        let subscribe = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
            subscription.len(),
            subscription
        );
        let (headers, _) = session.request(&mut stream, subscribe.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 204"));
        advance.blocking_send(()).unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        // Freshness comes from actual 27%; the live admitted target is shown
        // consistently by encrypted reads and notifications during transition.
        let (headers, body) = session.request(&mut stream, &[]);
        assert!(headers.starts_with("EVENT/1.0 200"), "{headers}");
        let target: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(target["characteristics"][0]["value"], 30);
        let (headers, body) = session.request(&mut stream, read.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        let observed: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(observed["characteristics"][0]["value"], 30);
        assert_eq!(
            Store::open(&database)
                .unwrap()
                .command_status(&boot, 1)
                .unwrap(),
            Some(Status::Accepted)
        );
        assert_eq!(
            Store::open(&database)
                .unwrap()
                .light_restore_levels(1)
                .unwrap()[&homesense_core::DeviceId::new("simulation:light").unwrap()]
                .get(),
            40
        );
        advance.blocking_send(()).unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let (headers, body) = session.request(&mut stream, &[]);
        assert!(headers.starts_with("EVENT/1.0 200"));
        let notification: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(notification["characteristics"][0]["aid"], binding.aid);
        assert_eq!(notification["characteristics"][0]["iid"], iid);
        assert_eq!(
            notification["characteristics"][0].get("value"),
            Some(&hap::serde_json::Value::Null)
        );

        let (headers, body) = session.request(&mut stream, read.as_bytes());
        assert!(headers.starts_with("HTTP/1.1 207"), "{headers}");
        let offline: hap::serde_json::Value = hap::serde_json::from_slice(&body).unwrap();
        assert_eq!(offline["characteristics"][0]["status"], -70402);
        assert!(offline["characteristics"][0].get("value").is_none());
    });
    let publication: Result<(), String> = async {
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), steps.recv())
                .await
                .map_err(|_| "client progress timeout")?
                .ok_or("client stopped")?;
            owner.receive("simulation").await?;
            graph.publish(&owner).await?;
            published.send(()).map_err(|_| "client stopped")?;
        }
        Ok(())
    }
    .await;
    drop(published);
    let result = client.await;
    task.abort();
    let _ = task.await;
    actor.shutdown().await.unwrap();
    drop(graph);
    drop(server);
    if combined {
        let restarted = Admission::open(
            directory.join("runtime.sqlite"),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(restarted.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let graph = RuntimeHap::new(
            &config,
            feedback.snapshot(),
            &identity,
            restarted.clone(),
            5000,
        )
        .await
        .unwrap();
        let binding = &graph.lights[0].0;
        let body = hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]}).to_string();
        graph
            .write_handler
            .write_async(controller, body.as_bytes())
            .await
            .unwrap();
        let ticket = restarted.dispatch().await.unwrap().unwrap();
        assert_eq!(
            ticket.command.command,
            homesense_core::Command::SetLevel {
                level: homesense_core::Percent::new(40).unwrap(),
                transition: homesense_core::LevelTransition::Ramp
            }
        );
        restarted
            .finish_write(ticket.command.id, true)
            .await
            .unwrap();
        let relay = restarted.dispatch().await.unwrap().unwrap();
        assert_eq!(relay.command.command, homesense_core::Command::Switch(true));
        restarted
            .finish_write(relay.command.id, true)
            .await
            .unwrap();
        restarted.shutdown().await.unwrap();
        drop(graph);
    }
    drop(identity);
    std::fs::remove_dir_all(directory).unwrap();
    result.unwrap();
    publication.unwrap();
}
