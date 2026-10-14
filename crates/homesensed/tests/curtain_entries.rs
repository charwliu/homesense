//! Real encrypted localhost HAP and authenticated Web, one owner, memory bus.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use futures_util::{SinkExt, StreamExt};
use hap::storage::Storage;
use homesense_core::{DeviceId, ExecutionContext};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    web_api::Api,
    web_auth::{Grant, Role, Tokens},
};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(b.to_vec());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn document() -> serde_json::Value {
    serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"26","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"panel-module","address":"4A","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"stop_led_idle_ms":200,"evidence":"synthetic review","reviewed_at_unix_ms":1,"leds":{"open":["open-led"],"close":["close-led"],"stop":["stop-led"]}}},{"id":"open-led","name":"Open LED","module_id":"panel-module","channel":2,"enabled":true},{"id":"stop-led","name":"Stop LED","module_id":"panel-module","channel":4,"enabled":true},{"id":"close-led","name":"Close LED","module_id":"panel-module","channel":6,"enabled":true}]})
}

async fn view(admission: &Admission, controller: Uuid) -> homesensed::curtain_owner::View {
    admission
        .curtain_views(
            controller,
            admission.task_generation().await.unwrap(),
            vec![DeviceId::new("curtain").unwrap()],
        )
        .await
        .unwrap()
        .remove(&DeviceId::new("curtain").unwrap())
        .unwrap()
}
async fn wait_written(
    admission: &Admission,
    controller: Uuid,
    action: &str,
    after: Option<u64>,
) -> homesensed::curtain_owner::View {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let v = view(admission, controller).await;
        if v.last_written_command.as_deref() == Some(action) && v.last_written_id != after {
            return v;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "missing Written {action}: {v:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn request(origin: &str, secret: &str, device: &str, action: &str, key: &str) -> Request<Bytes> {
    Request::builder().method("POST").uri("/api/v1/device-commands").header("host",origin.strip_prefix("http://").unwrap()).header("origin",origin).header("authorization",format!("Bearer {secret}")).header("content-type","application/json")
        .body(Bytes::from(serde_json::json!({"device":device,"revision":"1","request_id":key,"command":{"action":action}}).to_string())).unwrap()
}
async fn event_until(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    wanted: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let frame = serde_json::from_str(&text).unwrap();
                    if wanted(&frame) {
                        return frame;
                    }
                }
                Some(Ok(Message::Ping(bytes))) => socket.send(Message::Pong(bytes)).await.unwrap(),
                other => panic!("unexpected event stream {other:?}"),
            }
        }
    })
    .await
    .expect("model event notification")
}
#[tokio::test]
async fn curtain_encrypted_hap_web_share_written_motion_leds_and_original_scope() {
    let root = std::env::temp_dir().join(format!("curtain-entry-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let document = serde_json::to_vec(&document()).unwrap();
    let config =
        homesense_config::Configuration::parse(&document, ExecutionContext::Simulation).unwrap();
    let db = root.join("runtime.sqlite");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[]}"#;
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_automation(None, &document, rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let admission = Admission::open(
        db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == a || id == b,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let writes = Arc::new(Mutex::new(vec![]));
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut transport = homesensed::observed_transport::ObservedTransport::new(
        homesensed::transport_owner::TransportOwner::new(
            admission.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(writes.clone()), 1),
            )]
            .into(),
        ),
        feedback,
    );
    let mut runtime = homesensed::automation_runtime::AutomationRuntime::load(
        admission.clone(),
        a,
        1800000000000,
        1000,
        8,
        Default::default(),
    )
    .await
    .unwrap();
    let tokens = Arc::new(Tokens::default());
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            Uuid::new_v4(),
            secret,
            Grant {
                controller: b,
                role: Role::Member,
                devices: ["curtain".into()].into(),
                scenes: Default::default(),
            },
        )
        .unwrap();
    let web_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web_address = web_reservation.local_addr().unwrap();
    drop(web_reservation);
    let api = Arc::new(Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: format!("http://{web_address}"),
        tokens,
        runtime: std::sync::RwLock::new(Some(homesensed::web_api::Runtime {
            control: runtime.control(),
            revision: 1,
            boot: admission.boot_id().into(),
            scenes: Default::default(),
        })),
        drafts: None,
        receipts: Mutex::new(
            homesensed::web_receipts::Receipts::open(&root.join("web.sqlite")).unwrap(),
        ),
    });
    let mut identity = homesense_hap::storage::IdentityStore::open(root.join("identity"))
        .await
        .unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let graph = homesensed::runtime_motor_hap::RuntimeMotorHap::new(
        &config,
        &identity,
        admission.clone(),
        vec![homesense_hap::motor_graph::MotorSpec {
            device: DeviceId::new("curtain").unwrap(),
            name: "Synthetic curtain".into(),
        }],
        move |id, device| id == a && device.as_str() == "curtain",
    )
    .await
    .unwrap();
    let binding = graph
        .accessories
        .iter()
        .find(|(b, _)| b.operation == homesense_hap::identifiers::MotorOperation::Open)
        .unwrap()
        .0
        .clone();
    let server = hap::server::IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    server
        .install_startup_graph(
            graph.accessories.iter().map(|(_, h)| h.clone()).collect(),
            graph.write_handler.clone(),
        )
        .await
        .unwrap();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[49; 32]);
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            a,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    drop(reservation);
    let (ready, started) = tokio::sync::oneshot::channel();
    let active = server.clone();
    let listener = tokio::spawn(async move { active.run_handle_ready(ready).await });
    started.await.unwrap();
    let (stop, receive) = tokio::sync::watch::channel(false);
    let web_api = api.clone();
    let web_stop = receive.clone();
    let web =
        tokio::spawn(
            async move { homesensed::web_server::serve(web_api, web_address, web_stop).await },
        );
    let drive = tokio::spawn(async move {
        runtime
            .drive(
                &mut transport,
                Duration::from_millis(5),
                1000,
                receive,
                || Ok(1800000000000),
            )
            .await
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while tokio::net::TcpStream::connect(web_address).await.is_err() {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Real Web HTTP session followed by a real authenticated WS transport.
    let mut login = tokio::net::TcpStream::connect(web_address).await.unwrap();
    login.write_all(format!("POST /api/v1/session HTTP/1.1\r\nHost: {web_address}\r\nOrigin: {}\r\nAuthorization: Bearer {secret}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",api.origin).as_bytes()).await.unwrap();
    let mut login_bytes = vec![];
    tokio::time::timeout(Duration::from_secs(3), login.read_to_end(&mut login_bytes))
        .await
        .unwrap()
        .unwrap();
    let login = String::from_utf8(login_bytes).unwrap();
    assert!(login.starts_with("HTTP/1.1 200"));
    let cookie = login
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut ws_request = format!("ws://{web_address}/api/v1/events")
        .into_client_request()
        .unwrap();
    ws_request
        .headers_mut()
        .insert("origin", api.origin.parse().unwrap());
    ws_request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let socket = tokio::net::TcpStream::connect(web_address).await.unwrap();
    let (mut events, _) = tokio_tungstenite::client_async(ws_request, socket)
        .await
        .unwrap();
    events
        .send(Message::Text("{\"cursor\":null}".into()))
        .await
        .unwrap();
    let initial = event_until(&mut events, |v| v["type"] == "state").await;
    assert_eq!(initial["reset"], true);
    assert_eq!(initial["devices"].as_array().unwrap().len(), 1);
    // Encrypted HAP action from A, subsequently read and controlled by Web B.
    tokio::task::spawn_blocking(move || {
        let mut stream=std::net::TcpStream::connect(address).unwrap();stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut session=support::verify(&mut stream,a,&signing,&accessory_id,&accessory_key);
        for value in [false,true] {
            let body=serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":value}]}).to_string();
            let request=format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
            let (head,body)=session.request(&mut stream,request.as_bytes());assert!(head.starts_with("HTTP/1.1 204"),"{head} {body:?}");
        }
    }).await.unwrap();
    let opened = wait_written(&admission, b, "Open", None).await;
    assert_eq!(opened.movement.as_deref(), Some("Opening"));
    assert!(opened.stop_led_active.unwrap());
    let opening_event = event_until(&mut events, |v| {
        v["devices"][0]["motor_estimate"]["movement"] == "Opening"
    })
    .await;
    assert!(opening_event["devices"][0]["observed"].is_null());
    assert_eq!(
        view(&admission, a).await.last_written_id,
        opened.last_written_id
    );
    let mut get = request(&api.origin, secret, "curtain", "open", "unused");
    *get.method_mut() = hyper::Method::GET;
    *get.uri_mut() = "/api/v1/devices".parse().unwrap();
    let response = api.handle(get).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(json["devices"].as_array().unwrap().len(), 1);
    assert_eq!(json["devices"][0]["motor_estimate"]["source"], "inferred");
    assert!(json["devices"][0]["observed"].is_null());
    let before = writes.lock().unwrap().len();
    assert_eq!(
        api.handle(request(
            &api.origin,
            secret,
            "stop-led",
            "stop",
            "forbidden"
        ))
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(writes.lock().unwrap().len(), before);
    assert_eq!(
        api.handle(request(&api.origin, secret, "curtain", "close", "close-1"))
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let closing = wait_written(&admission, a, "Close", opened.last_written_id).await;
    assert_eq!(closing.movement.as_deref(), Some("Closing"));
    assert_eq!(
        view(&admission, b).await.last_written_id,
        closing.last_written_id
    );
    assert_eq!(
        api.handle(request(&api.origin, secret, "curtain", "stop", "stop-1"))
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let stopped = wait_written(&admission, a, "Stop", closing.last_written_id).await;
    assert_eq!(stopped.position.as_deref(), Some("Intermediate"));
    assert_eq!(stopped.percent_hundredths, None);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(view(&admission, b).await.stop_led_active, Some(false));
    let idle_event = event_until(&mut events, |v| {
        v["devices"][0]["motor_estimate"]["last_written_id"]
            == serde_json::json!(stopped.last_written_id.unwrap())
            && v["devices"][0]["motor_estimate"]["stop_led_active"] == false
    })
    .await;
    assert_eq!(
        idle_event["cursor"]["sequence"], initial["cursor"]["sequence"],
        "model timer events must not mutate actual feedback history"
    );
    assert_eq!(idle_event["reset"], false);
    // Each new Stop is physical, even with a green activity hint. Same HTTP
    // request_id still returns its receipt without issuing a duplicate command.
    assert_eq!(
        api.handle(request(&api.origin, secret, "curtain", "stop", "stop-2"))
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let repeated = wait_written(&admission, a, "Stop", stopped.last_written_id).await;
    assert_eq!(
        api.handle(request(&api.origin, secret, "curtain", "stop", "stop-2"))
            .await
            .status(),
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        view(&admission, b).await.last_written_id,
        repeated.last_written_id
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let latest = view(&admission, a).await;
    assert_eq!(latest.movement.as_deref(), Some("Stopped"));
    assert_eq!(latest.position.as_deref(), Some("Intermediate"));
    assert_eq!(latest.last_written_id, repeated.last_written_id);
    let bytes = writes.lock().unwrap().clone();
    let motor: Vec<_> = bytes.iter().filter(|b| b.get(1) == Some(&0x26)).collect();
    assert_eq!(motor.len(), 4);
    assert_eq!(motor[0], &vec![0xAE, 0x26, 1, 0xF2, 0xEE]);
    assert_eq!(motor[1], &vec![0xAE, 0x26, 1, 0xF1, 0xEE]);
    assert_eq!(motor[2], &vec![0xAE, 0x26, 1, 0xF0, 0xEE]);
    assert_eq!(motor[3], motor[2]);
    assert!(bytes.iter().any(|b| b == &vec![0xAE, 0x4A, 4, 0xF1, 0xEE]));
    assert!(bytes.iter().any(|b| b == &vec![0xAE, 0x4A, 4, 0xF2, 0xEE]));
    let sql = rusqlite::Connection::open(&db).unwrap();
    let causes: Vec<i64> = sql
        .prepare(
            "SELECT run_id FROM command_journal WHERE device_id='stop-led' AND status='written'",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(causes.contains(&i64::try_from(opened.last_written_id.unwrap()).unwrap()));
    assert!(causes.contains(&i64::try_from(stopped.last_written_id.unwrap()).unwrap()));
    drop(sql);
    events.close(None).await.unwrap();
    stop.send(true).unwrap();
    web.await.unwrap().unwrap();
    drive.await.unwrap().unwrap();
    listener.abort();
    let _ = listener.await;
    drop(server);
    drop(identity);
    drop(graph);
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn curtain_idle_read_expires_pending_at_receive_boundary_without_motor_replay() {
    let root = std::env::temp_dir().join(format!("curtain-idle-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut d = document();
    d["devices"][0]["curtain_state"]["leds"] = serde_json::json!({});
    let db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_configuration(
            None,
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let controller = Uuid::new_v4();
    let actor = Admission::open(
        db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            request_ttl: Duration::from_millis(50),
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let device = DeviceId::new("curtain").unwrap();
    let ids = actor
        .submit_motor(
            controller,
            device.clone(),
            homesense_core::MotorAction::Open,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let s = view(&actor, controller).await;
    assert_eq!(s.pending, None);
    assert_eq!(s.last_written_id, None);
    assert!(actor.dispatch().await.unwrap().is_none());
    assert_eq!(
        homesense_storage::Store::open(&db)
            .unwrap()
            .command_status(actor.boot_id(), ids[0])
            .unwrap(),
        Some(homesense_storage::journal::Status::Cancelled)
    );
    actor
        .submit_motor(controller, device, homesense_core::MotorAction::Stop)
        .await
        .unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    actor.finish_write(ticket.command.id, true).await.unwrap();
    assert_eq!(
        view(&actor, controller)
            .await
            .last_written_command
            .as_deref(),
        Some("Stop")
    );
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn curtain_idle_first_reader_cannot_retain_revoked_sources_estimate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let root = std::env::temp_dir().join(format!("curtain-revoke-idle-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut d = document();
    d["devices"][0]["curtain_state"]["leds"] = serde_json::json!({});
    let db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_configuration(
            None,
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let allowed = Arc::new(AtomicBool::new(true));
    let auth = allowed.clone();
    let actor = Admission::open(
        db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| (id == a && auth.load(Ordering::SeqCst)) || id == b,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let device = DeviceId::new("curtain").unwrap();
    actor
        .submit_motor(a, device.clone(), homesense_core::MotorAction::Open)
        .await
        .unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    actor.finish_write(ticket.command.id, true).await.unwrap();
    assert_eq!(view(&actor, b).await.movement.as_deref(), Some("Opening"));
    allowed.store(false, Ordering::SeqCst);
    let revoked = view(&actor, b).await;
    assert_eq!(revoked.last_written_id, None);
    assert_eq!(revoked.movement, None);
    actor
        .submit_motor(b, device, homesense_core::MotorAction::Stop)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(21)).await;
    let ticket = actor.dispatch().await.unwrap().unwrap();
    actor.finish_write(ticket.command.id, true).await.unwrap();
    assert_eq!(
        view(&actor, b).await.last_written_command.as_deref(),
        Some("Stop")
    );
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn curtain_configuration_preview_exposes_timing_led_review_and_removal_changes() {
    let original = document();
    let parse = |d: &serde_json::Value| {
        homesense_config::Configuration::parse(
            &serde_json::to_vec(d).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap()
    };
    let current = parse(&original);
    assert!(homesensed::configuration_preview::device_changes(&current, &current).is_empty());
    for (field, value) in [
        ("open_travel_ms", serde_json::json!(1100)),
        ("stop_led_idle_ms", serde_json::json!(250)),
        ("leds", serde_json::json!({})),
        ("evidence", serde_json::json!("new synthetic review")),
    ] {
        let mut next = original.clone();
        next["revision"] = 2.into();
        next["devices"][0]["curtain_state"][field] = value;
        let changes = homesensed::configuration_preview::device_changes(&current, &parse(&next));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].changes, vec!["curtain_state"]);
    }
    let mut next = original.clone();
    next["revision"] = 2.into();
    next["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("curtain_state");
    assert_eq!(
        homesensed::configuration_preview::device_changes(&current, &parse(&next))[0].changes,
        vec!["curtain_state"]
    );
}
