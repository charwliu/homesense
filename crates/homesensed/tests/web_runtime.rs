use homesense_core::ExecutionContext;
use homesensed::{
    admission::{Admission, Options},
    automation_control::{Action, Reply},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
    web_api::Api,
    web_auth::{Grant, Role, Tokens},
    web_receipts::Receipts,
};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
struct Wire;
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len()) // In-memory simulation sink; no gateway connection.
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn web_scope_origin_receipts_and_runtime_admission() {
    let root = std::env::temp_dir().join(format!("web-runtime-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let devices = include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(None, devices, rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let caller = Uuid::new_v4();
    let token_id = Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == caller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut runtime = AutomationRuntime::load(
        admission.clone(),
        caller,
        1_800_000_000_000,
        1000,
        8,
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
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    // An unobserved dimmer without a default must never invent 100% on power-on.
    let device = homesense_core::DeviceId::new("simulation:light").unwrap();
    let generation = admission.task_generation().await.unwrap();
    assert!(
        homesensed::management_devices::prepare(
            &admission,
            caller,
            generation.clone(),
            device.clone(),
            homesensed::management_devices::LightAction::Power { on: true },
            &feedback,
            1000
        )
        .await
        .is_err()
    );
    assert!(
        homesensed::management_devices::prepare(
            &admission,
            Uuid::new_v4(),
            generation.clone(),
            device.clone(),
            homesensed::management_devices::LightAction::Brightness { level: 40 },
            &feedback,
            1000
        )
        .await
        .is_err()
    );
    let batch = homesensed::management_devices::prepare(
        &admission,
        caller,
        generation,
        device,
        homesensed::management_devices::LightAction::Brightness { level: 40 },
        &feedback,
        1000,
    )
    .await
    .unwrap();
    assert_eq!(batch.controller, caller);
    assert_eq!(batch.intents.len(), 1);
    assert!(
        matches!(batch.intents[0].command,homesense_core::Command::SetLevel{level,..} if level.get()==40)
    );
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            admission.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(Wire, 1),
            )]
            .into(),
        ),
        feedback,
    );
    let tokens = Arc::new(Tokens::default());
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            token_id,
            secret,
            Grant {
                controller: caller,
                role: Role::Member,
                scenes: ["light".into()].into(),
                devices: ["simulation:light".into()].into(),
            },
        )
        .unwrap();
    let api = Api {
        publisher:None,
        origin: "http://127.0.0.1:8088".into(),
        tokens: tokens.clone(),
        runtime: std::sync::RwLock::new(Some(homesensed::web_api::Runtime {
            control: control.clone(),
            revision: 1,
            boot: admission.boot_id().to_owned(),
            scenes: [("light".into(), "Test".into())].into(),
        })),
        drafts: Some((
            ExecutionContext::Simulation,
            Mutex::new(homesensed::web_drafts::Drafts::open(&root.join("drafts.sqlite")).unwrap()),
        )),
        receipts: Mutex::new(Receipts::open(&root.join("web.sqlite")).unwrap()),
    };
    let (stop, receive) = tokio::sync::watch::channel(false);
    let client = async {
        let make = |origin: &str, body: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/v1/scene-actions")
                .header("host", "127.0.0.1:8088")
                .header("origin", origin)
                .header("authorization", format!("Bearer {secret}"))
                .header("content-type", "application/json")
                .body(Bytes::copy_from_slice(body.as_bytes()))
                .unwrap()
        };
        let mut login = make(&api.origin, "{}");
        *login.uri_mut() = "/api/v1/session".parse().unwrap();
        let login = api.handle(login).await;
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let session: serde_json::Value =
            serde_json::from_slice(&login.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let mut no_csrf = make(&api.origin, r#"{"scene":"light","request_id":"blocked"}"#);
        no_csrf.headers_mut().remove("authorization");
        no_csrf
            .headers_mut()
            .insert("cookie", cookie.parse().unwrap());
        assert_eq!(api.handle(no_csrf).await.status(), StatusCode::UNAUTHORIZED);
        let logout = Request::builder()
            .method("DELETE")
            .uri("/api/v1/session")
            .header("host", "127.0.0.1:8088")
            .header("origin", &api.origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .body(Bytes::new())
            .unwrap();
        assert_eq!(api.handle(logout).await.status(), StatusCode::OK);
        let body = r#"{"scene":"light","request_id":"one"}"#;
        assert_eq!(
            api.handle(make("http://evil.invalid", body)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            api.handle(make(&api.origin, r#"{"scene":"other","request_id":"one"}"#))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let first = api.handle(make(&api.origin, body)).await;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let result: serde_json::Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(result["outcome"], "accepted");
        let mut next: serde_json::Value = serde_json::from_slice(devices).unwrap();
        next["revision"] = 2.into();
        let candidate = homesense_config::Configuration::parse(
            &serde_json::to_vec(&next).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap();
        let preview = control
            .request(Action::ConfigurationPreview {
                controller: caller,
                revision: 1,
                candidate: candidate.clone(),
            })
            .await
            .unwrap();
        let Reply::ConfigurationPreview(preview) = preview else {
            panic!("wrong preview reply");
        };
        assert_eq!(preview.interrupted_runs.len(), 1);
        assert_eq!(
            preview.interrupted_runs[0].run,
            result["run"].as_u64().unwrap()
        );
        assert_eq!(
            preview.interrupted_runs[0].definition.as_deref(),
            Some("light")
        );
        assert!(!preview.connection_rebuild_required);
        assert!(
            control
                .request(Action::ConfigurationPreview {
                    controller: Uuid::new_v4(),
                    revision: 1,
                    candidate
                })
                .await
                .is_err()
        );
        let duplicate = api.handle(make(&api.origin, body)).await;
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &duplicate.into_body().collect().await.unwrap().to_bytes()
            )
            .unwrap(),
            result
        );
        let mut cancel = make(
            &api.origin,
            r#"{"start_request_id":"one","request_id":"cancel-one"}"#,
        );
        *cancel.uri_mut() = "/api/v1/scene-cancellations".parse().unwrap();
        let cancelled = api.handle(cancel).await;
        assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
        let cancelled: serde_json::Value =
            serde_json::from_slice(&cancelled.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(cancelled["outcome"], "cancelled");
        let run = result["run"].as_u64().unwrap();
        assert_eq!(
            control.request(Action::Status(run)).await.unwrap(),
            Reply::Status(Some(homesense_core::scene::Status::Cancelled))
        );
        let read = Request::builder()
            .uri("/api/v1/scenes")
            .header("host", "127.0.0.1:8088")
            .header("authorization", format!("Bearer {secret}"))
            .body(Bytes::new())
            .unwrap();
        assert_eq!(api.handle(read).await.status(), StatusCode::OK);
        let read = Request::builder()
            .uri("/api/v1/devices")
            .header("host", "127.0.0.1:8088")
            .header("authorization", format!("Bearer {secret}"))
            .body(Bytes::new())
            .unwrap();
        let snapshot = api.handle(read).await;
        assert_eq!(snapshot.status(), StatusCode::OK);
        let snapshot: serde_json::Value =
            serde_json::from_slice(&snapshot.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(snapshot["devices"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["devices"][0]["id"], "simulation:light");
        assert!(snapshot["devices"][0]["observed"].is_null());
        for (body, expected) in [
            (
                r#"{"device":"other","request_id":"device-one","command":{"action":"power","on":true}}"#,
                StatusCode::FORBIDDEN,
            ),
            (
                r#"{"device":"simulation:light","request_id":"device-one","command":{"action":"brightness","level":101}}"#,
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let mut command = make(&api.origin, body);
            *command.uri_mut() = "/api/v1/device-commands".parse().unwrap();
            assert_eq!(api.handle(command).await.status(), expected);
        }
        let device_body = r#"{"device":"simulation:light","request_id":"device-one","command":{"action":"brightness","level":40}}"#;
        let mut accepted = None;
        for expected in [StatusCode::ACCEPTED, StatusCode::OK] {
            let mut command = make(&api.origin, device_body);
            *command.uri_mut() = "/api/v1/device-commands".parse().unwrap();
            let result = api.handle(command).await;
            assert_eq!(result.status(), expected);
            let result: serde_json::Value =
                serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(result["outcome"], "accepted");
            assert_eq!(result["commands"].as_array().unwrap().len(), 1);
            if let Some(previous) = accepted.as_ref() {
                assert_eq!(previous, &result);
            } else {
                accepted = Some(result);
            }
        }
        let mut conflict = make(&api.origin, &device_body.replace("40", "30"));
        *conflict.uri_mut() = "/api/v1/device-commands".parse().unwrap();
        assert_eq!(api.handle(conflict).await.status(), StatusCode::CONFLICT);
        let draft_id = Uuid::new_v4();
        let mut definition: serde_json::Value = serde_json::from_slice(devices).unwrap();
        definition["revision"] = 2.into();
        let draft_body=serde_json::json!({"expected_revision":1,"revision":2,"devices":definition,"automation":{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[],"timed_scenes":[]}}).to_string();
        let make_draft = || {
            let mut r = make(&api.origin, &draft_body);
            *r.uri_mut() = format!("/api/v1/config/drafts/{draft_id}").parse().unwrap();
            r
        };
        assert_eq!(
            api.handle(make_draft()).await.status(),
            StatusCode::FORBIDDEN
        );
        let mut activation=make_draft();*activation.uri_mut()=format!("/api/v1/config/activations/{draft_id}").parse().unwrap();
        assert_eq!(api.handle(activation).await.status(),StatusCode::FORBIDDEN);
        let mut administrator = tokens.authenticate(&format!("Bearer {secret}")).unwrap();
        administrator.role = Role::Administrator;
        tokens.insert(token_id, secret, administrator).unwrap();
        assert_eq!(api.handle(make_draft()).await.status(), StatusCode::OK);
        assert_eq!(api.handle(make_draft()).await.status(), StatusCode::OK);
        let mut read = make_draft();
        *read.method_mut() = hyper::Method::GET;
        *read.body_mut() = Bytes::new();
        let saved = api.handle(read).await;
        assert_eq!(saved.status(), StatusCode::OK);
        let saved: serde_json::Value =
            serde_json::from_slice(&saved.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(saved["revision"], 2);
        let mut preview = make_draft();
        *preview.uri_mut() = format!("/api/v1/config/previews/{draft_id}")
            .parse()
            .unwrap();
        *preview.method_mut() = hyper::Method::GET;
        *preview.body_mut() = Bytes::new();
        let preview = api.handle(preview).await;
        assert_eq!(preview.status(), StatusCode::OK);
        let preview: serde_json::Value =
            serde_json::from_slice(&preview.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(preview["reservation"], false);
        assert_eq!(preview["impact"]["active_revision"], 1);
        assert!(
            preview["impact"]["interrupted_runs"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let mut no_origin = make_draft();
        no_origin.headers_mut().remove("origin");
        assert_eq!(api.handle(no_origin).await.status(), StatusCode::FORBIDDEN);
        let saved_runtime = api.runtime.write().unwrap().take().unwrap();
        let paused = api
            .handle(make(
                &api.origin,
                r#"{"scene":"light","request_id":"during-publication"}"#,
            ))
            .await;
        assert_eq!(paused.status(), StatusCode::SERVICE_UNAVAILABLE);
        let mut during_read = make(&api.origin, "");
        *during_read.method_mut() = hyper::Method::GET;
        *during_read.uri_mut() = "/api/v1/scenes".parse().unwrap();
        assert_eq!(
            api.handle(during_read).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut session_read = make(&api.origin, "");
        *session_read.method_mut() = hyper::Method::GET;
        *session_read.uri_mut() = "/api/v1/session".parse().unwrap();
        assert_eq!(api.handle(session_read).await.status(), StatusCode::OK);
        let mut logout = make(&api.origin, "");
        *logout.method_mut() = hyper::Method::DELETE;
        *logout.uri_mut() = "/api/v1/session".parse().unwrap();
        assert_eq!(api.handle(logout).await.status(), StatusCode::OK);
        assert!(
            api.receipts
                .lock()
                .unwrap()
                .lookup(&caller.to_string(), "during-publication")
                .unwrap()
                .is_none()
        );
        *api.runtime.write().unwrap() = Some(saved_runtime);
        let mut resumed = make(&api.origin, "");
        *resumed.method_mut() = hyper::Method::GET;
        *resumed.uri_mut() = "/api/v1/devices".parse().unwrap();
        assert_eq!(api.handle(resumed).await.status(), StatusCode::OK);
        tokens.revoke(token_id).unwrap();
        assert_eq!(
            api.handle(make(&api.origin, body)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        stop.send(true).unwrap();
    };
    let (result, ()) = tokio::join!(
        runtime.drive(&mut owner, Duration::from_millis(1), 1000, receive, || Ok(
            1_800_000_000_000
        )),
        client
    );
    result.unwrap();
    let sql = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(sql);
    drop(api);
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
