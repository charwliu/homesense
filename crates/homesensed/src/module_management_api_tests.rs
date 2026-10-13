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
    let h = harness(
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
    assert_eq!(
        page["truth_source"],
        "single_feedback_owner_physical_bindings"
    );
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
    h.close().await;
}
