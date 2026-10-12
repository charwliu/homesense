//! Scoped Web scene targets use the same durable local selection as HAP.
use homesense_core::ExecutionContext;
use homesensed::{
    admission::{Admission, Options},
    automation_control::{Action, Reply},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
    web_api::{Api, Runtime},
    web_auth::{Grant, Role, Tokens},
    web_receipts::Receipts,
};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

fn rules() -> Vec<u8> {
    let scenes=[("reading",100),("daily",60),("eco",17),("off",0)].map(|(id,level)|json!({"id":id,"enabled":true,"review_evidence":"synthetic reviewed scene","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":level,"ramp":false}}]}));
    serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":scenes,
        "scene_switch_groups":[{"id":"room","variable":"selection","review_evidence":"synthetic reviewed exclusion","members":[{"scene":"reading","value":1,"off_scene":"off"},{"scene":"daily","value":2,"off_scene":"off"},{"scene":"eco","value":3,"off_scene":"off"}],"reset_scenes":["off"]}]})).unwrap()
}
struct Wire;
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        panic!("delayed scenes must not write")
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn request(api: &Api, secret: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("host", api.origin.strip_prefix("http://").unwrap())
        .header("origin", &api.origin)
        .header("authorization", format!("Bearer {secret}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let response = api
        .handle(
            builder
                .body(Bytes::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn owner(
    path: &std::path::Path,
    caller: Uuid,
) -> (
    Admission,
    homesensed::automation_control::Control,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    let a = Admission::open(
        path.to_owned(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == caller,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    let mut runtime = AutomationRuntime::load(
        a.clone(),
        caller,
        1_800_000_000_000,
        1000,
        16,
        Default::default(),
    )
    .await
    .unwrap();
    let control = runtime.control();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut transport = ObservedTransport::new(
        TransportOwner::new(
            a.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(Wire, 1),
            )]
            .into(),
        ),
        feedback,
    );
    let (stop, receive) = tokio::sync::watch::channel(false);
    let drive = tokio::spawn(async move {
        runtime
            .drive(
                &mut transport,
                Duration::from_millis(1),
                5000,
                receive,
                || Ok(1_800_000_000_000),
            )
            .await
    });
    (a, control, stop, drive)
}

#[tokio::test]
async fn scene_stream_invalidates_stopped_selection_and_recovers_on_same_socket() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let root = std::env::temp_dir().join(format!("scene-stream-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(
            None,
            include_bytes!("../../../config/simulation.json"),
            &rules(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let caller = Uuid::new_v4();
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    let tokens = Arc::new(Tokens::default());
    tokens
        .insert(
            Uuid::new_v4(),
            secret,
            Grant {
                controller: caller,
                role: Role::Member,
                scenes: ["reading".into()].into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let (a, control, stop, drive) = owner(&path, caller).await;
    let view = |a: &Admission, control: homesensed::automation_control::Control| Runtime {
        control,
        revision: 1,
        boot: a.boot_id().into(),
        scenes: ["reading", "daily", "eco", "off"]
            .map(|id| (id.into(), id.into()))
            .into(),
    };
    let api = Arc::new(Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: format!("http://{address}"),
        tokens,
        runtime: std::sync::RwLock::new(Some(view(&a, control.clone()))),
        receipts: Mutex::new(Receipts::open(&root.join("web.sqlite")).unwrap()),
        drafts: None,
    });
    let (web_stop, receive) = tokio::sync::watch::channel(false);
    let web = tokio::spawn(homesensed::web_server::serve(api.clone(), address, receive));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let stream = loop {
        match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => break stream,
            Err(_) => {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    let mut handshake = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    handshake
        .headers_mut()
        .insert("origin", api.origin.parse().unwrap());
    handshake
        .headers_mut()
        .insert("authorization", format!("Bearer {secret}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::client_async(handshake, stream)
        .await
        .unwrap();
    socket
        .send(Message::Text(r#"{"cursor":null}"#.into()))
        .await
        .unwrap();
    async fn next(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
        let Some(Ok(Message::Text(v))) =
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
        else {
            panic!("expected state message")
        };
        serde_json::from_str(&v).unwrap()
    }
    let initial = next(&mut socket).await;
    assert_eq!(initial["type"], "state");
    assert_eq!(initial["scenes"].as_array().unwrap().len(), 1);
    assert!(initial["scenes"][0]["selected"].is_null());
    assert_eq!(initial["scenes"][0]["switch_kind"], "selection");
    let (status, v) = request(
        &api,
        secret,
        "/api/v1/scene-actions",
        Some(json!({"scene":"reading","request_id":"stream-on","on":true})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(v["outcome"], "accepted");
    let active = next(&mut socket).await;
    assert_eq!(active["scenes"][0]["selected"], true);
    assert_eq!(active["scenes"].as_array().unwrap().len(), 1);
    stop.send(true).unwrap();
    drive.await.unwrap().unwrap();
    let invalid = next(&mut socket).await;
    assert_eq!(invalid, json!({"type":"unavailable"}));
    a.shutdown().await.unwrap();
    let (a, control, stop, drive) = owner(&path, caller).await;
    *api.runtime.write().unwrap() = Some(view(&a, control.clone()));
    let restored = next(&mut socket).await;
    assert_eq!(restored["type"], "state");
    assert_eq!(restored["reset"], true);
    assert_eq!(restored["scenes"][0]["selected"], true);
    assert_eq!(restored["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(definitions(&path), ["reading"]);
    // Another local control entry changes selection; the event stream updates
    // the allowed member without exposing the selected peer's identity.
    assert!(matches!(
        control.request(Action::Start("eco".into())).await.unwrap(),
        Reply::Started(_)
    ));
    let changed = next(&mut socket).await;
    assert_eq!(changed["scenes"][0]["selected"], false);
    assert_eq!(changed["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(changed["scenes"][0]["id"], "reading");
    socket.close(None).await.unwrap();
    web_stop.send(true).unwrap();
    web.await.unwrap().unwrap();
    stop.send(true).unwrap();
    drive.await.unwrap().unwrap();
    a.shutdown().await.unwrap();
    drop(api);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
async fn selected(api: &Api, secret: &str, expected: Option<&str>) -> Value {
    let (status, v) = request(api, secret, "/api/v1/scenes", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for id in ["reading", "daily", "eco"] {
        let scene = v["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap();
        assert_eq!(scene["switch_kind"], "selection");
        assert_eq!(scene["selected"], json!(expected == Some(id)));
    }
    let off = v["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "off")
        .unwrap();
    assert_eq!(off["switch_kind"], "momentary");
    assert!(off["selected"].is_null());
    v
}
fn definitions(path: &std::path::Path) -> Vec<String> {
    let db = rusqlite::Connection::open(path).unwrap();
    db.prepare("SELECT definition FROM scene_run ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
#[tokio::test]
async fn web_selection_targets_preserve_scope_legacy_receipts_and_off_cancellation() {
    let root = std::env::temp_dir().join(format!("scene-web-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let devices = include_bytes!("../../../config/simulation.json");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(None, devices, &rules(), ExecutionContext::Simulation, &[])
        .unwrap();
    let caller = Uuid::new_v4();
    let limited_controller = Uuid::new_v4();
    let observer_controller = Uuid::new_v4();
    let member = "abcdefghijklmnopqrstuvwxyz0123456789";
    let limited = "bcdefghijklmnopqrstuvwxyz0123456789a";
    let observer = "cdefghijklmnopqrstuvwxyz0123456789ab";
    for restarting in [false, true] {
        let a = Admission::open(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| [caller, limited_controller, observer_controller].contains(&id),
        )
        .await
        .unwrap();
        a.synchronized(1, 1).await.unwrap();
        let mut runtime = AutomationRuntime::load(
            a.clone(),
            caller,
            1_800_000_000_000,
            1000,
            16,
            Default::default(),
        )
        .await
        .unwrap();
        let control = runtime.control();
        let config =
            homesense_config::Configuration::parse(devices, ExecutionContext::Simulation).unwrap();
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut owner = ObservedTransport::new(
            TransportOwner::new(
                a.clone(),
                [(
                    "simulation".into(),
                    hb_transport::Connection::for_simulation(Wire, 1),
                )]
                .into(),
            ),
            feedback,
        );
        let tokens = Arc::new(Tokens::default());
        for (secret, controller, role, scenes) in [
            (
                member,
                caller,
                Role::Member,
                vec!["reading", "daily", "eco", "off"],
            ),
            (limited, limited_controller, Role::Member, vec!["reading"]),
            (observer, observer_controller, Role::Viewer, vec!["daily"]),
        ] {
            tokens
                .insert(
                    Uuid::new_v4(),
                    secret,
                    Grant {
                        controller,
                        role,
                        scenes: scenes.into_iter().map(String::from).collect(),
                        devices: Default::default(),
                    },
                )
                .unwrap();
        }
        let api = Api {
            daily_backup: Default::default(),
            hap_bridges: Default::default(),
            hap_status: Default::default(),
            backup_source: None,
            publisher: None,
            origin: "http://127.0.0.1:8088".into(),
            tokens: tokens.clone(),
            runtime: std::sync::RwLock::new(Some(Runtime {
                control: control.clone(),
                revision: 1,
                boot: a.boot_id().into(),
                scenes: ["reading", "daily", "eco", "off"]
                    .map(|id| (id.into(), id.into()))
                    .into(),
            })),
            receipts: Mutex::new(Receipts::open(&root.join("web.sqlite")).unwrap()),
            drafts: None,
        };
        let (stop, receive) = tokio::sync::watch::channel(false);
        let drive = tokio::spawn(async move {
            runtime
                .drive(&mut owner, Duration::from_millis(1), 5000, receive, || {
                    Ok(1_800_000_000_000)
                })
                .await
        });
        if restarting {
            selected(&api, member, Some("eco")).await;
            assert_eq!(
                definitions(&path),
                ["reading", "off", "daily", "eco", "reading", "off", "eco"]
            );
        } else {
            let (status, v) = request(&api, member, "/api/v1/scenes", None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                v["scenes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|s| s["selected"].is_null())
            );
            let (status, v) = request(&api, observer, "/api/v1/scenes", None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["scenes"].as_array().unwrap().len(), 1);
            assert_eq!(v["scenes"][0]["id"], "daily");
            assert_eq!(v["scenes"][0]["can_control"], false);
            assert!(v.get("variables").is_none());
            assert!(v["scenes"][0].get("group").is_none());
            assert_eq!(
                request(
                    &api,
                    observer,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":"daily","request_id":"denied","on":true}))
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
            for on in [Value::Null, json!(1), json!("false")] {
                assert_eq!(
                    request(
                        &api,
                        member,
                        "/api/v1/scene-actions",
                        Some(json!({"scene":"reading","request_id":"malformed","on":on}))
                    )
                    .await
                    .0,
                    StatusCode::BAD_REQUEST
                );
            }
            assert!(definitions(&path).is_empty());
            let legacy = json!({"scene":"reading","request_id":"legacy-on"});
            let (status, receipt) =
                request(&api, member, "/api/v1/scene-actions", Some(legacy)).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(receipt["outcome"], "accepted");
            let (status, replay) = request(
                &api,
                member,
                "/api/v1/scene-actions",
                Some(json!({"scene":"reading","request_id":"legacy-on","on":true})),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(replay, receipt);
            assert_eq!(
                request(
                    &api,
                    member,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":"reading","request_id":"legacy-on","on":false}))
                )
                .await
                .0,
                StatusCode::CONFLICT
            );
            selected(&api, member, Some("reading")).await;
            let (status, v) = request(
                &api,
                member,
                "/api/v1/scene-actions",
                Some(json!({"scene":"reading","request_id":"off-reading","on":false})),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(v["outcome"], "accepted");
            selected(&api, member, None).await;
            for scene in ["daily", "eco"] {
                let (status, v) = request(
                    &api,
                    member,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":scene,"request_id":scene,"on":true})),
                )
                .await;
                assert_eq!(status, StatusCode::ACCEPTED);
                assert_eq!(v["outcome"], "accepted");
                selected(&api, member, Some(scene)).await;
            }
            for (scene, on, key) in [("daily", false, "inactive-off"), ("eco", true, "same-on")] {
                let (status, v) = request(
                    &api,
                    member,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":scene,"request_id":key,"on":on})),
                )
                .await;
                assert_eq!(status, StatusCode::ACCEPTED);
                assert_eq!(v["outcome"], "unchanged");
                assert_eq!(v["retry"], false);
            }
            assert_eq!(definitions(&path), ["reading", "off", "daily", "eco"]);
            assert_eq!(
                request(
                    &api,
                    limited,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":"off","request_id":"hidden","on":true}))
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
            let (status, v) = request(&api, limited, "/api/v1/scenes", None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["scenes"].as_array().unwrap().len(), 1);
            assert_eq!(v["scenes"][0]["selected"], false);
            for (key, on) in [("limited-on", true), ("limited-off", false)] {
                let (status, v) = request(
                    &api,
                    limited,
                    "/api/v1/scene-actions",
                    Some(json!({"scene":"reading","request_id":key,"on":on})),
                )
                .await;
                assert_eq!(status, StatusCode::ACCEPTED);
                assert_eq!(v["outcome"], "accepted");
            }
            selected(&api, member, None).await;
            let cancel = json!({"start_request_id":"limited-off","request_id":"cancel-off"});
            assert_eq!(
                request(
                    &api,
                    member,
                    "/api/v1/scene-cancellations",
                    Some(cancel.clone())
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
            let (status, v) =
                request(&api, limited, "/api/v1/scene-cancellations", Some(cancel)).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(v["outcome"], "cancelled");
            selected(&api, member, None).await;
            assert!(matches!(
                control.request(Action::Start("eco".into())).await.unwrap(),
                Reply::Started(_)
            ));
            selected(&api, member, Some("eco")).await;
        }
        stop.send(true).unwrap();
        drive.await.unwrap().unwrap();
        assert_eq!(
            request(&api, member, "/api/v1/scenes", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        a.shutdown().await.unwrap();
    }
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
