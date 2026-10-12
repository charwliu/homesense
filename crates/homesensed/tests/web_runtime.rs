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
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(bytes.to_vec());
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
    let mut definition:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    definition["rooms"]=serde_json::json!([{"id":"visible","name":"Visible room","floor":"Test floor"},{"id":"hidden","name":"Unassigned room"}]);
    definition["devices"][0]["room"]=serde_json::json!("visible");

    definition["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    for (id,channel) in [("z:motor",2),("z:uncalibrated",3)] {
        let mut device=serde_json::json!({"id":id,"name":"Curtain","gateway":"simulation","address":"22","channel":channel,"enabled":true});
        if channel==2 {device["motor_calibration"]=serde_json::json!({"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1});}
        definition["devices"].as_array_mut().unwrap().push(device);
    }
    let device_bytes=serde_json::to_vec(&definition).unwrap();let devices=device_bytes.as_slice();
    let writes=Arc::new(Mutex::new(vec![]));
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
            homesensed::management_devices::DeviceAction::Power { on: true },
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
            homesensed::management_devices::DeviceAction::Brightness { level: 40 },
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
        homesensed::management_devices::DeviceAction::Brightness { level: 40 },
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
                hb_transport::Connection::for_simulation(Wire(writes.clone()), 1),
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
                devices: ["simulation:light".into(),"z:motor".into(),"z:uncalibrated".into()].into(),
            },
        )
        .unwrap();
    let mut api = Api {
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
        for (path,status) in [("/health/live",StatusCode::OK),("/health/ready",StatusCode::UNAUTHORIZED)] {
            let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=path.parse().unwrap();read.headers_mut().remove("authorization");
            assert_eq!(api.handle(read).await.status(),status);
        }
        let mut rooms=make(&api.origin,"");*rooms.method_mut()=hyper::Method::GET;*rooms.uri_mut()="/api/v1/rooms".parse().unwrap();
        let rooms=api.handle(rooms).await;assert_eq!(rooms.status(),StatusCode::OK);
        let rooms:serde_json::Value=serde_json::from_slice(&rooms.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(rooms["rooms"],serde_json::json!([{"id":"visible","name":"Visible room","floor":"Test floor"}]));
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
        let Reply::LastRun(Some(last))=control.request(Action::LastRun("light".into())).await.unwrap() else {panic!("missing run")};
        let run_path=format!("/api/v1/runs/{}/{}",last.boot,last.run);
        let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=run_path.parse().unwrap();
        let read=api.handle(read).await;assert_eq!(read.status(),StatusCode::OK);
        let summary:serde_json::Value=serde_json::from_slice(&read.into_body().collect().await.unwrap().to_bytes()).unwrap();assert_eq!(summary["scene"],"light");assert_eq!(summary["revision"],"1");assert!(summary["started_ms"].as_str().unwrap().parse::<u64>().is_ok());assert!(summary.get("cause").is_none());
        let allowed=tokens.authenticate(&format!("Bearer {secret}")).unwrap();let mut denied=allowed.clone();denied.scenes.clear();tokens.insert(token_id,secret,denied).unwrap();
        let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=run_path.parse().unwrap();assert_eq!(api.handle(read).await.status(),StatusCode::NOT_FOUND);
        tokens.insert(token_id,secret,allowed).unwrap();
        for (path,body,status) in [("/api/v1/runs/not-a-boot/run","",StatusCode::BAD_REQUEST),(run_path.as_str(),"{}",StatusCode::BAD_REQUEST)] {
            let mut read=make(&api.origin,body);*read.method_mut()=hyper::Method::GET;*read.uri_mut()=path.parse().unwrap();assert_eq!(api.handle(read).await.status(),status);
        }
        assert!(control.request(Action::RunSummary{controller:caller,revision:2,boot:last.boot.clone(),run:last.run.clone()}).await.is_err());
        assert!(control.request(Action::RunSummary{controller:Uuid::new_v4(),revision:1,boot:last.boot,run:last.run}).await.is_err());

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
        assert_eq!(snapshot["devices"].as_array().unwrap().len(), 3);
        assert_eq!(snapshot["devices"][1]["can_control"],true);
        assert_eq!(snapshot["devices"][2]["can_control"],false);
        assert!(snapshot["devices"][1]["observed"].is_null());
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
        let member=tokens.authenticate(&format!("Bearer {secret}")).unwrap();
        for viewer in [false,true] {
            let mut denied=member.clone();
            if viewer {denied.role=Role::Viewer;} else {denied.devices.remove("z:motor");}
            tokens.insert(token_id,secret,denied).unwrap();
            let body=r#"{"device":"z:motor","request_id":"denied-motor","command":{"action":"open"}}"#;
            let mut request=make(&api.origin,body);*request.uri_mut()="/api/v1/device-commands".parse().unwrap();
            assert_eq!(api.handle(request).await.status(),StatusCode::FORBIDDEN);
        }
        tokens.insert(token_id,secret,member).unwrap();
        assert!(control.request(Action::DeviceCommand{controller:Uuid::new_v4(),revision:1,device:homesense_core::DeviceId::new("z:motor").unwrap(),action:homesensed::management_devices::DeviceAction::Open}).await.is_err());
        for (index,action) in ["open","close","stop"].into_iter().enumerate() {
            let body=serde_json::json!({"device":"z:motor","request_id":format!("motor-{action}"),"command":{"action":action}}).to_string();
            let mut original=None;
            for expected in [StatusCode::ACCEPTED,StatusCode::OK] {
                let mut request=make(&api.origin,&body);*request.uri_mut()="/api/v1/device-commands".parse().unwrap();
                let response=api.handle(request).await;assert_eq!(response.status(),expected);
                let value:serde_json::Value=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
                assert_eq!(value["outcome"],"accepted");
                if let Some(previous)=original.as_ref() {assert_eq!(previous,&value);} else {original=Some(value);}
            }
            tokio::time::timeout(Duration::from_secs(1),async {
                while writes.lock().unwrap().iter().filter(|f|f[1]==0x22).count()<=index {tokio::time::sleep(Duration::from_millis(2)).await;}
            }).await.unwrap();
            let mut conflict=make(&api.origin,&body.replace(&format!("\"action\":\"{action}\""),"\"action\":\"power\",\"on\":true"));
            *conflict.uri_mut()="/api/v1/device-commands".parse().unwrap();assert_eq!(api.handle(conflict).await.status(),StatusCode::CONFLICT);
        }
        for (device,action) in [("z:uncalibrated","open"),("simulation:light","stop"),("z:motor","power")] {
            let command=if action=="power" {serde_json::json!({"action":action,"on":true})}else{serde_json::json!({"action":action})};
            let body=serde_json::json!({"device":device,"request_id":format!("reject-{device}"),"command":command}).to_string();
            let mut request=make(&api.origin,&body);*request.uri_mut()="/api/v1/device-commands".parse().unwrap();
            let response=api.handle(request).await;let value:serde_json::Value=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
            assert_ne!(value["outcome"],"accepted");
        }
        assert!(control.request(Action::DeviceCommand{controller:caller,revision:2,device:homesense_core::DeviceId::new("z:motor").unwrap(),action:homesensed::management_devices::DeviceAction::Open}).await.is_err());
        assert_eq!(writes.lock().unwrap().iter().filter(|f|f[1]==0x22).cloned().collect::<Vec<_>>(),vec![vec![0xae,0x22,2,0xf2,0xee],vec![0xae,0x22,2,0xf1,0xee],vec![0xae,0x22,2,0xf0,0xee]]);
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
        for path in ["/api/v1/config/versions/1/restore-draft","/health/ready","/api/v1/diagnostics","/api/v1/diagnostics/calendar","/api/v1/config/drafts","/api/v1/config/versions","/api/v1/config/versions/1/export"] {
            let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=path.parse().unwrap();
            assert_eq!(api.handle(read).await.status(),StatusCode::FORBIDDEN);
        }
        let mut capability=make(&api.origin,"");*capability.method_mut()=hyper::Method::GET;*capability.uri_mut()="/api/v1/session".parse().unwrap();
        let capability:serde_json::Value=serde_json::from_slice(&api.handle(capability).await.into_body().collect().await.unwrap().to_bytes()).unwrap();assert_eq!(capability["can_view_diagnostics"],false);
        let mut administrator = tokens.authenticate(&format!("Bearer {secret}")).unwrap();
        administrator.role = Role::Administrator;
        tokens.insert(token_id, secret, administrator).unwrap();
        let saved_drafts=api.drafts.take();
        let mut capability=make(&api.origin,"");*capability.method_mut()=hyper::Method::GET;*capability.uri_mut()="/api/v1/session".parse().unwrap();
        let capability:serde_json::Value=serde_json::from_slice(&api.handle(capability).await.into_body().collect().await.unwrap().to_bytes()).unwrap();assert_eq!(capability["can_view_diagnostics"],true);assert_eq!(capability["can_manage_configuration"],false);
        let mut diagnostic=make(&api.origin,"");*diagnostic.method_mut()=hyper::Method::GET;*diagnostic.uri_mut()="/api/v1/diagnostics".parse().unwrap();assert_eq!(api.handle(diagnostic).await.status(),StatusCode::OK);
        api.drafts=saved_drafts;

        for (path,method,body,status) in [("/health/ready",hyper::Method::GET,"",StatusCode::OK),("/health/ready",hyper::Method::GET,"{}",StatusCode::BAD_REQUEST),("/health/ready",hyper::Method::POST,"{}",StatusCode::METHOD_NOT_ALLOWED),("/health/live",hyper::Method::POST,"",StatusCode::METHOD_NOT_ALLOWED),("/health/live",hyper::Method::GET,"{}",StatusCode::BAD_REQUEST),("/health/ready?token=x",hyper::Method::GET,"",StatusCode::BAD_REQUEST)] {
            let mut read=make(&api.origin,body);*read.method_mut()=method;*read.uri_mut()=path.parse().unwrap();assert_eq!(api.handle(read).await.status(),status);
        }
        for (method,body,status) in [(hyper::Method::GET,"",StatusCode::OK),(hyper::Method::GET,"{}",StatusCode::BAD_REQUEST),(hyper::Method::POST,"{}",StatusCode::METHOD_NOT_ALLOWED)] {
            let mut read=make(&api.origin,body);*read.method_mut()=method;*read.uri_mut()="/api/v1/diagnostics".parse().unwrap();
            let result=api.handle(read).await;assert_eq!(result.status(),status);
            if status==StatusCode::OK {let value:serde_json::Value=serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes()).unwrap();assert_eq!(value["revision"],"1");assert_eq!(value["failed"],false);assert_eq!(value["gateway_ready"],true);assert_eq!(value["storage_healthy"],true);}
        }
        assert!(control.request(Action::Diagnostics{controller:Uuid::new_v4(),revision:1}).await.is_err());
        assert!(control.request(Action::Diagnostics{controller:caller,revision:2}).await.is_err());
        for (method,body,status) in [(hyper::Method::GET,"",StatusCode::OK),(hyper::Method::GET,"{}",StatusCode::BAD_REQUEST),(hyper::Method::POST,"{}",StatusCode::METHOD_NOT_ALLOWED)] {
            let mut read=make(&api.origin,body);*read.method_mut()=method;*read.uri_mut()="/api/v1/diagnostics/calendar".parse().unwrap();
            let result=api.handle(read).await;assert_eq!(result.status(),status);
            if status==StatusCode::OK {let value:serde_json::Value=serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes()).unwrap();assert_eq!(value["revision"],"1");assert_eq!(value["calendar"]["schedule_count"],0);assert_eq!(value["calendar"]["stopped"],false);assert!(value["calendar"]["watermark_unix_ms"].as_str().unwrap().parse::<i64>().is_ok());}
        }
        assert!(control.request(Action::ManagedCalendarStatus{controller:Uuid::new_v4(),revision:1}).await.is_err());
        assert!(control.request(Action::ManagedCalendarStatus{controller:caller,revision:2}).await.is_err());
        for (path,status) in [
            ("/api/v1/config/versions?limit=1",StatusCode::OK),
            ("/api/v1/config/versions/1/export",StatusCode::OK),
            ("/api/v1/config/versions/99/export",StatusCode::NOT_FOUND),
            ("/api/v1/config/versions?limit=0",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions?limit=101",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions?limit=1&limit=2",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions?before=0",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions?before=18446744073709551615",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions?unknown=1",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions/1/export?x=1",StatusCode::BAD_REQUEST),
            ("/api/v1/config/versions/0/export",StatusCode::BAD_REQUEST),
        ] {
            let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=path.parse().unwrap();
            let response=api.handle(read).await;assert_eq!(response.status(),status,"{path}");
            assert_eq!(response.headers()["cache-control"],"no-store");
            let body:serde_json::Value=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
            if status==StatusCode::OK {
                assert_eq!(body["active_revision"],"1");
                if path.ends_with("export") {
                    assert_eq!(body["devices"],serde_json::from_slice::<serde_json::Value>(devices).unwrap());
                    assert_eq!(body["automation"],serde_json::from_slice::<serde_json::Value>(rules).unwrap());
                } else {assert_eq!(body["versions"][0]["revision"],"1");assert!(body["next_before"].is_null());}
            }
        }
        let mut write=make(&api.origin,"{}");*write.uri_mut()="/api/v1/config/versions".parse().unwrap();assert_eq!(api.handle(write).await.status(),StatusCode::METHOD_NOT_ALLOWED);
        assert!(control.request(Action::ConfigurationHistory{controller:Uuid::new_v4(),revision:1,before:None,limit:20}).await.is_err());
        assert!(control.request(Action::ConfigurationExport{controller:caller,revision:2,selected:1}).await.is_err());
        assert_eq!(api.handle(make_draft()).await.status(), StatusCode::OK);
        assert_eq!(api.handle(make_draft()).await.status(), StatusCode::OK);
        let mut catalog=make(&api.origin,"");*catalog.method_mut()=hyper::Method::GET;*catalog.uri_mut()="/api/v1/config/drafts".parse().unwrap();
        let catalog=api.handle(catalog).await;assert_eq!(catalog.status(),StatusCode::OK);
        let catalog:serde_json::Value=serde_json::from_slice(&catalog.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(catalog["drafts"].as_array().unwrap().len(),1);assert_eq!(catalog["drafts"][0]["id"],draft_id.to_string());assert_eq!(catalog["drafts"][0]["revision"],"2");assert!(catalog["drafts"][0].get("devices").is_none());
        let precise_id=Uuid::new_v4();let mut precise:serde_json::Value=serde_json::from_slice(devices).unwrap();precise["revision"]=serde_json::json!(9223372036854775807u64);
        let precise_body=serde_json::json!({"expected_revision":1,"revision":9223372036854775807u64,"devices":precise,"automation":{"schema_version":1,"configuration_revision":9223372036854775807u64,"variables":[],"rules":[]}}).to_string();
        let mut save=make(&api.origin,&precise_body);*save.uri_mut()=format!("/api/v1/config/drafts/{precise_id}").parse().unwrap();assert_eq!(api.handle(save).await.status(),StatusCode::OK);
        let mut source=make(&api.origin,"");*source.method_mut()=hyper::Method::GET;*source.uri_mut()=format!("/api/v1/config/drafts/{precise_id}/source").parse().unwrap();
        let source=api.handle(source).await;assert_eq!(source.status(),StatusCode::OK);
        let source:serde_json::Value=serde_json::from_slice(&source.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(source["revision"],"9223372036854775807");assert_eq!(source["expected_revision"],"1");
        assert!(source["devices"].as_str().unwrap().contains("9223372036854775807"));assert!(source["automation"].as_str().unwrap().contains("9223372036854775807"));
        let mut source_write=make(&api.origin,"{}");*source_write.uri_mut()=format!("/api/v1/config/drafts/{precise_id}/source").parse().unwrap();assert_eq!(api.handle(source_write).await.status(),StatusCode::METHOD_NOT_ALLOWED);
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
        let restore_id=Uuid::new_v4();
        let restore_body=serde_json::json!({"id":restore_id,"expected_revision":"1","revision":"9223372036854775807"}).to_string();
        for _ in 0..2 {
            let mut request=make(&api.origin,&restore_body);*request.uri_mut()="/api/v1/config/versions/1/restore-draft".parse().unwrap();
            assert_eq!(api.handle(request).await.status(),StatusCode::OK);
        }
        let restored=api.drafts.as_ref().unwrap().1.lock().unwrap().get(caller,restore_id).unwrap().unwrap();
        assert_eq!(restored.revision,i64::MAX as u64);assert_eq!(restored.devices["revision"],serde_json::json!(i64::MAX));assert_eq!(restored.automation["configuration_revision"],serde_json::json!(i64::MAX));
        let mut original:serde_json::Value=serde_json::from_slice(devices).unwrap();original["revision"]=serde_json::json!(i64::MAX);assert_eq!(original,restored.devices);
        let mut original:serde_json::Value=serde_json::from_slice(rules).unwrap();original["configuration_revision"]=serde_json::json!(i64::MAX);assert_eq!(original,restored.automation);
        for (expected,revision,status) in [("2","3",StatusCode::CONFLICT),("1","1",StatusCode::BAD_REQUEST),("1","9223372036854775808",StatusCode::BAD_REQUEST),("1","2",StatusCode::CONFLICT)] {
            let mut request=make(&api.origin,&serde_json::json!({"id":restore_id,"expected_revision":expected,"revision":revision}).to_string());*request.uri_mut()="/api/v1/config/versions/1/restore-draft".parse().unwrap();assert_eq!(api.handle(request).await.status(),status);
        }
        let saved_runtime = api.runtime.write().unwrap().take().unwrap();
        for (path,status) in [("/health/live",StatusCode::OK),("/health/ready",StatusCode::SERVICE_UNAVAILABLE)] {
            let mut read=make(&api.origin,"");*read.method_mut()=hyper::Method::GET;*read.uri_mut()=path.parse().unwrap();assert_eq!(api.handle(read).await.status(),status);
        }

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
        4
    );
    drop(sql);
    drop(api);
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
