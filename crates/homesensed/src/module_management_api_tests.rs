//! Actual API/actor tests with synthetic persisted registry; no sockets or hardware.
use super::*;
use crate::web_auth::{Grant, Role, Tokens};
use http_body_util::BodyExt;
#[tokio::test]
async fn administrator_module_controls_use_stable_ids_and_cannot_select_physical_addresses() {
    let draft = crate::web_drafts::Draft {
        id: uuid::Uuid::new_v4(),
        expected_revision: 1,
        revision: 1,
        devices: serde_json::from_slice(&devices()).unwrap(),
        automation: automation(),
    };
    let (draft, _) = crate::module_migration::prepare(
        draft,
        crate::module_migration::Edit {
            id: uuid::Uuid::new_v4(),
            expected_revision: "1".into(),
            revision: "2".into(),
        },
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut h = harness(
        serde_json::to_vec(&draft.devices).unwrap(),
        draft.automation,
    )
    .await;
    let (control, mut receiver) =
        crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let tokens = std::sync::Arc::new(Tokens::default());
    let admin = "abcdefghijklmnopqrstuvwxyz0123456789";
    let viewer = "abcdefghijklmnopqrstuvwxyz9876543210";
    for (secret, role) in [(admin, Role::Administrator), (viewer, Role::Viewer)] {
        tokens
            .insert(
                uuid::Uuid::new_v4(),
                secret,
                Grant {
                    controller: h.controller,
                    role,
                    scenes: Default::default(),
                    devices: ["light".into()].into(),
                },
            )
            .unwrap();
    }
    let api = crate::web_api::Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://127.0.0.1:8088".into(),
        tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control,
            revision: 2,
            boot: h.actor.boot_id().into(),
            scenes: Default::default(),
        })),
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap(),
        ),
        drafts: None,
    };
    fn req(path: &str, secret: &str, body: Option<Json>) -> hyper::Request<hyper::body::Bytes> {
        let mut request = hyper::Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:8088")
            .header("authorization", format!("Bearer {secret}"));
        if body.is_some() {
            request = request
                .method("POST")
                .header("origin", "http://127.0.0.1:8088")
                .header("content-type", "application/json");
        }
        request
            .body(body.map(|b| b.to_string()).unwrap_or_default().into())
            .unwrap()
    }
    async fn json(response: hyper::Response<http_body_util::Full<hyper::body::Bytes>>) -> Json {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    assert_eq!(
        api.handle(req("/api/v1/modules", viewer, None))
            .await
            .status(),
        hyper::StatusCode::FORBIDDEN
    );
    for query in [
        "?limit=65",
        "?offset=1",
        "?offset=1&revision=1",
        "?revision=1",
    ] {
        let response = api
            .handle(req(&format!("/api/v1/modules{query}"), admin, None))
            .await;
        assert!(response.status().is_client_error());
    }
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(req("/api/v1/modules", admin, None)),
    )
    .await;
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let page = json(response).await;
    assert_eq!(page["revision"], "2");
    assert_eq!(page["truth_source"], "canonical_module_channels");
    let module = page["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["slots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|slot| slot["binding"]["id"] == "light")
        })
        .unwrap();
    let id = module["module_id"].as_str().unwrap().to_owned();
    let channel = module["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["binding"]["id"] == "light")
        .unwrap()["channel"]
        .as_u64()
        .unwrap();
    assert!(
        module["slots"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["binding"].as_object())
            .filter(|b| b["id"] != "light")
            .all(|b| b["actions"].as_array().unwrap().is_empty())
    );
    h.no_output();
    let body = json!({"module_id":id,"channel":channel,"revision":"2","request_id":"module-once","command":{"action":"brightness","level":35}});
    let mut invalid = body.clone();
    invalid["address"] = "15".into();
    assert_eq!(
        api.handle(req("/api/v1/module-commands", admin, Some(invalid)))
            .await
            .status(),
        hyper::StatusCode::BAD_REQUEST
    );
    let mut stale = body.clone();
    stale["revision"] = "1".into();
    assert_eq!(
        api.handle(req("/api/v1/module-commands", admin, Some(stale)))
            .await
            .status(),
        hyper::StatusCode::CONFLICT
    );
    assert_eq!(
        api.handle(req("/api/v1/module-commands", viewer, Some(body.clone())))
            .await
            .status(),
        hyper::StatusCode::FORBIDDEN
    );
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(req("/api/v1/module-commands", admin, Some(body.clone()))),
    )
    .await;
    assert_eq!(response.status(), hyper::StatusCode::ACCEPTED);
    let first = json(response).await;
    assert_eq!(first["outcome"], "accepted");
    let duplicate = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(req("/api/v1/module-commands", admin, Some(body))),
    )
    .await;
    assert_eq!(json(duplicate).await, first);
    let count: i64 = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let page = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/modules", admin, None)),
        )
        .await,
    )
    .await;
    assert_eq!(page["commands"][0]["status"], "accepted");
    assert_eq!(page["commands"][0]["requested"]["level"], 35);
    let lamp = page["modules"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["slots"].as_array().unwrap())
        .find(|s| s["binding"]["id"] == "light")
        .unwrap();
    assert_eq!(lamp["binding"]["actual"], Json::Null);
    assert_eq!(lamp["binding"]["quality"], "unknown");
    let query = json!({"module_id":id,"scope":{"kind":"channel","channel":3},"revision":"2","request_id":"query-once"});
    assert_eq!(
        api.handle(req("/api/v1/module-queries", viewer, Some(query.clone())))
            .await
            .status(),
        hyper::StatusCode::FORBIDDEN
    );
    for value in [None, Some("http://untrusted.invalid")] {
        let mut request = req("/api/v1/module-queries", admin, Some(query.clone()));
        request.headers_mut().remove("origin");
        if let Some(value) = value {
            request
                .headers_mut()
                .insert("origin", value.parse().unwrap());
        }
        assert_eq!(
            api.handle(request).await.status(),
            hyper::StatusCode::FORBIDDEN
        );
    }
    let mut invalid_key = query.clone();
    invalid_key["request_id"] = "../unsafe".into();
    assert_eq!(
        api.handle(req("/api/v1/module-queries", admin, Some(invalid_key)))
            .await
            .status(),
        hyper::StatusCode::BAD_REQUEST
    );
    for extra in ["address", "gateway", "hex"] {
        let mut bad = query.clone();
        bad[extra] = "untrusted".into();
        assert_eq!(
            api.handle(req("/api/v1/module-queries", admin, Some(bad)))
                .await
                .status(),
            hyper::StatusCode::BAD_REQUEST
        );
    }
    let mut stale = query.clone();
    stale["revision"] = "1".into();
    assert_eq!(
        api.handle(req("/api/v1/module-queries", admin, Some(stale)))
            .await
            .status(),
        hyper::StatusCode::CONFLICT
    );
    let mut zero = query.clone();
    zero["scope"]["channel"] = 0.into();
    assert_eq!(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(zero)))
        )
        .await
        .status(),
        hyper::StatusCode::BAD_REQUEST
    );
    let panel = page["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "Keypad")
        .unwrap()["module_id"]
        .clone();
    let bad = json!({"module_id":panel,"scope":{"kind":"whole_module"},"revision":"2","request_id":"unsupported-panel"});
    assert_eq!(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(bad)))
        )
        .await
        .status(),
        hyper::StatusCode::BAD_REQUEST
    );
    let accepted = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(query.clone()))),
        )
        .await,
    )
    .await;
    assert_eq!(accepted["outcome"], "accepted");
    assert!(accepted["commands"][0].is_string());
    let duplicate = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(query.clone()))),
        )
        .await,
    )
    .await;
    assert_eq!(duplicate, accepted);
    let mut conflict = query.clone();
    conflict["scope"]["channel"] = 4.into();
    assert_eq!(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(conflict)))
        )
        .await
        .status(),
        hyper::StatusCode::CONFLICT
    );
    let endpoint = "/api/v1/module-queries/query-once";
    assert_eq!(
        api.handle(req(endpoint, viewer, None)).await.status(),
        hyper::StatusCode::FORBIDDEN
    );
    let before = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req(endpoint, admin, None)),
        )
        .await,
    )
    .await;
    assert_eq!(before["status"]["writes"][0]["status"], "accepted");
    assert_eq!(before["status"]["fresh_channels"], 0);
    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);
    impl std::io::Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    }
    impl std::io::Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let writes = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let mut owner = crate::transport_owner::TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                Wire(writes.clone()),
                h.generation.epochs.transport,
            ),
        )]
        .into(),
    );
    for _ in 0..2 {
        owner.step().await.unwrap().unwrap().result.unwrap();
    }
    assert_eq!(
        writes.lock().unwrap()[1],
        vec![0xae, 0x15, 0xa3, 0xf2, 0xee]
    );
    let written = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req(endpoint, admin, None)),
        )
        .await,
    )
    .await;
    assert_eq!(written["status"]["writes"][0]["status"], "written");
    assert_eq!(written["status"]["fresh_channels"], 0);
    h.feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x15,
                    command: 0xd3,
                    value: 37,
                }],
            },
        })
        .unwrap();
    let received = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req(endpoint, admin, None)),
        )
        .await,
    )
    .await;
    assert_eq!(received["status"]["fresh_channels"], 1);
    assert_eq!(received["status"]["requested_channels"], 1);
    assert_eq!(received["status"]["channels"][0]["binding"], Json::Null);
    assert_eq!(received["status"]["channels"][0]["actual"]["level"], 37);
    assert_eq!(received["status"]["atomic_snapshot"], false);
    assert_eq!(received["status"]["feedback_causality"], "not_established");
    let denied = json!({"module_id":id,"channel":3,"revision":"2","request_id":"unbound-control","command":{"action":"brightness","level":30}});
    assert_eq!(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-commands", admin, Some(denied)))
        )
        .await
        .status(),
        hyper::StatusCode::BAD_REQUEST
    );
    let whole = json!({"module_id":id,"scope":{"kind":"whole_module"},"revision":"2","request_id":"whole-query"});
    let whole = json(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req("/api/v1/module-queries", admin, Some(whole))),
        )
        .await,
    )
    .await;
    assert_eq!(whole["commands"].as_array().unwrap().len(), 1);
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(
        writes.lock().unwrap()[2],
        vec![0xae, 0x15, 0xa0, 0xf2, 0xee]
    );
    assert_eq!(h.feedback.snapshot().devices.len(), 6);
    h.actor.transport_failed().await.unwrap();
    assert_eq!(
        poll_request(
            &h,
            &mut receiver,
            &mut scenes,
            api.handle(req(endpoint, admin, None))
        )
        .await
        .status(),
        hyper::StatusCode::CONFLICT
    );
    drop(owner);
    h.close().await;
}
