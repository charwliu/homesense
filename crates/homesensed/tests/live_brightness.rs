//! Fresh steady feedback, retained Off brightness and real graph read callbacks.
use homesense_core::{DeviceId, ExecutionContext, ModuleKind, Observation, Percent};
use homesense_state::{Cursor, DeviceState, Quality, Snapshot};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};
fn p(n: u8) -> Percent {
    Percent::new(n).unwrap()
}
const CONFIG: &[u8] = br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"dimmer-role","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"down","name":"Synthetic light","module_id":"dimmer-role","channel":5,"enabled":true}]}"#;
fn config() -> homesense_config::Configuration {
    homesense_config::Configuration::parse(br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"dimmer-role","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"down","name":"Synthetic light","module_id":"dimmer-role","channel":5,"enabled":true}]}"#,ExecutionContext::Simulation).unwrap()
}
fn snapshot(level: u8) -> Snapshot {
    Snapshot {
        cursor: Cursor {
            boot_id: 1,
            sequence: 1,
        },
        state_version: 1,
        devices: [(
            DeviceId::new("down").unwrap(),
            DeviceState {
                kind: ModuleKind::Dimmer,
                desired: None,
                observed: Some(Observation::Level(p(level))),
                quality: Quality::Confirmed,
                observed_at_ms: Some(10),
                last_command_id: None,
                pending_command: None,
                version: 1,
            },
        )]
        .into(),
    }
}
#[test]
fn fresh_steady_on_projection_and_hap_read_use_external_actual_instead_of_old_setting() {
    let id = DeviceId::new("down").unwrap();
    let history = [(id.clone(), p(100))].into();
    let projected = homesensed::light_state::project(&config(), &snapshot(17), &history).unwrap();
    assert_eq!(
        projected.devices[&id].observed,
        Some(Observation::Level(p(17)))
    );
    let view =
        homesense_hap::observed::ObservedView::new(Arc::new(RwLock::new(projected)), 100, || 11)
            .unwrap()
            .with_restore_levels(Arc::new(RwLock::new(history)));
    assert!(view.power(&id, true).unwrap());
    assert_eq!(view.brightness(&id).unwrap(), p(17));
}
#[test]
fn off_retains_restore_only_and_stale_feedback_stays_unavailable() {
    let id = DeviceId::new("down").unwrap();
    let history: BTreeMap<_, _> = [(id.clone(), p(100))].into();
    let projected = homesensed::light_state::project(&config(), &snapshot(0), &history).unwrap();
    assert_eq!(
        projected.devices[&id].observed,
        Some(Observation::Level(p(0)))
    );
    let shared = Arc::new(RwLock::new(projected));
    let view = homesense_hap::observed::ObservedView::new(shared.clone(), 100, || 11)
        .unwrap()
        .with_restore_levels(Arc::new(RwLock::new(history)));
    assert!(!view.power(&id, true).unwrap());
    assert_eq!(view.brightness(&id).unwrap(), p(100));
    shared
        .write()
        .unwrap()
        .devices
        .get_mut(&id)
        .unwrap()
        .quality = Quality::Stale;
    assert!(view.power(&id, true).is_err());
    assert!(view.brightness(&id).is_err());
}

#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    server::{IpServer, Server},
    storage::Storage,
};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    runtime_hap::RuntimeHap,
};
use serde_json::{Value, json};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
async fn input(feedback: &mut Feedback, actor: &Admission, n: u8) {
    // Receive through the actual owner, which stamps the actor's own clock.
    // This read-only fixture never calls step and emits no control bytes.
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                std::io::Cursor::new(vec![0xae, 0x15, 0xd5, n, 0xee]),
                1,
            ),
        )]
        .into(),
    );
    feedback
        .apply(&owner.receive("bus").await.unwrap())
        .unwrap();
}
fn request(
    stream: &mut TcpStream,
    session: &mut support::Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    events: &mut Vec<Value>,
) -> Value {
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut response = session.request(stream, request.as_bytes());
    for _ in 0..32 {
        if response.0.starts_with("EVENT/") {
            events.push(serde_json::from_slice(&response.1).unwrap());
            response = session.request(stream, &[]);
            continue;
        }
        assert!(
            response.0.starts_with("HTTP/1.1 200") || response.0.starts_with("HTTP/1.1 204"),
            "{}",
            response.0
        );
        return if response.1.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.1).unwrap()
        };
    }
    panic!("unbounded notifications");
}
fn notification(
    stream: &mut TcpStream,
    session: &mut support::Session,
    events: &mut Vec<Value>,
    iid: u64,
    value: Value,
) {
    for _ in 0..8 {
        let response = session.request(stream, &[]);
        assert!(response.0.starts_with("EVENT/"));
        let event: Value = serde_json::from_slice(&response.1).unwrap();
        let found = event["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["iid"] == iid && c["value"] == value);
        events.push(event);
        if found {
            return;
        }
    }
    panic!("missing expected notification");
}
#[tokio::test]
async fn encrypted_hap_external17_off_restore_on_and_owned_ramp_share_reported_read_events() {
    let root = std::env::temp_dir().join(format!("homesense-live-brightness-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let configuration = config();
    let actor_db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&actor_db)
        .unwrap()
        .publish_configuration(None, CONFIG, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let actor = Admission::open(
        actor_db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let id = DeviceId::new("down").unwrap();
    actor
        .submit(homesense_hap::requests::Batch {
            controller,
            intents: vec![homesense_hap::requests::Intent {
                device: id.clone(),
                command: homesense_core::Command::SetLevel {
                    level: p(100),
                    transition: homesense_core::LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    actor.finish_write(ticket.command.id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    let mut feedback = Feedback::new(
        &configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    input(&mut feedback, &actor, 100).await;
    let mut identity = homesense_hap::storage::IdentityStore::open(root.join("identity"))
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
    let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let mut graph = RuntimeHap::new(
        &configuration,
        feedback.snapshot(),
        &identity,
        actor.clone(),
        5000,
    )
    .await
    .unwrap();
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
    let service = tokio::spawn(async move { running.run_handle().await });
    let (advance, mut steps) = tokio::sync::mpsc::channel(1);
    let (done, ready) = std::sync::mpsc::sync_channel(1);
    let client = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match TcpStream::connect(address) {
                Ok(s) => break s,
                Err(e) => {
                    assert!(Instant::now() < deadline, "{e}");
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
        let bright = binding.brightness_iid.unwrap();
        let mut events = vec![];
        let path = format!(
            "/characteristics?id={}.{},{}.{}",
            binding.aid, binding.on_iid, binding.aid, bright
        );
        let value = request(&mut stream, &mut session, "GET", &path, None, &mut events);
        assert_eq!(value["characteristics"][1]["value"], 100);
        request(
            &mut stream,
            &mut session,
            "PUT",
            "/characteristics",
            Some(
                json!({"characteristics":[{"aid":binding.aid,"iid":bright,"ev":true},{"aid":binding.aid,"iid":binding.on_iid,"ev":true}]}),
            ),
            &mut events,
        );
        for stage in 1..=6 {
            if stage == 3 {
                request(
                    &mut stream,
                    &mut session,
                    "PUT",
                    "/characteristics",
                    Some(
                        json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]}),
                    ),
                    &mut events,
                );
            }
            if stage == 4 {
                request(
                    &mut stream,
                    &mut session,
                    "PUT",
                    "/characteristics",
                    Some(json!({"characteristics":[{"aid":binding.aid,"iid":bright,"value":40}]})),
                    &mut events,
                );
            }
            advance.blocking_send(stage).unwrap();
            ready.recv_timeout(Duration::from_secs(3)).unwrap();
            if [1, 3, 4, 6].contains(&stage) {
                notification(
                    &mut stream,
                    &mut session,
                    &mut events,
                    bright,
                    json!(if stage == 4 { 40 } else { 17 }),
                );
            }
            if stage == 2 {
                notification(
                    &mut stream,
                    &mut session,
                    &mut events,
                    binding.on_iid,
                    json!(false),
                );
            }
            let result = request(&mut stream, &mut session, "GET", &path, None, &mut events);
            assert_eq!(result["characteristics"][0]["value"], stage != 2);
            assert_eq!(
                result["characteristics"][1]["value"],
                if [4, 5].contains(&stage) { 40 } else { 17 },
                "stage {stage}"
            );
        }
        assert!(
            events
                .iter()
                .flat_map(|e| e["characteristics"].as_array().unwrap())
                .filter(|c| c["iid"] == bright)
                .all(|c| c["value"] == 17 || c["value"] == 40),
            "no old100 or intermediate36 notification"
        );
    });
    let result: Result<(), String> = async {
        for _ in 1..=6 {
            let stage = tokio::time::timeout(Duration::from_secs(4), steps.recv())
                .await
                .map_err(|e| e.to_string())?
                .ok_or("client stopped")?;
            if [3, 4].contains(&stage) {
                let ticket = actor
                    .dispatch()
                    .await
                    .map_err(|e| format!("{e:?}"))?
                    .ok_or("expected local command")?;
                assert_eq!(
                    ticket.command.command,
                    homesense_core::Command::SetLevel {
                        level: p(if stage == 3 { 17 } else { 40 }),
                        transition: homesense_core::LevelTransition::Immediate
                    }
                );
                actor
                    .finish_write(ticket.command.id, true)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
            input(
                &mut feedback,
                &actor,
                match stage {
                    2 => 0,
                    4 => 36,
                    5 => 40,
                    _ => 17,
                },
            )
            .await;
            graph.publish_snapshot(feedback.snapshot()).await?;
            let generation = actor
                .task_generation()
                .await
                .map_err(|e| format!("{e:?}"))?;
            let state = homesensed::management_devices::snapshot(
                &actor,
                controller,
                generation,
                vec![id.clone()],
                &feedback,
            )
            .await?;
            assert_eq!(
                state.devices[0].reported,
                Some(Observation::Level(p(match stage {
                    2 => 0,
                    4 | 5 => 40,
                    _ => 17,
                })))
            );
            assert_eq!(
                state.devices[0].recorded,
                stage == 4,
                "only genuine owned ramp is a display target"
            );
            let saved = homesense_storage::Store::open(&actor_db)
                .unwrap()
                .light_restore_levels(1)
                .unwrap();
            assert_eq!(
                saved[&id],
                p(if stage == 5 { 40 } else { 17 }),
                "stage {stage} restore memory"
            );
            done.send(()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    .await;
    drop(done);
    let client_result = client.await;
    service.abort();
    let _ = service.await;
    actor.shutdown().await.unwrap();
    drop(graph);
    drop(server);
    drop(identity);
    drop(feedback);
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
    client_result.unwrap();
}
