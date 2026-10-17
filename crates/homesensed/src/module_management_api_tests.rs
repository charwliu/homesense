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
    let restricted = "abcdefghijklmnopqrstuvwxyz5555555555";
    for (secret, role) in [(admin, Role::Administrator), (viewer, Role::Viewer), (restricted, Role::Administrator)] {
        tokens
            .insert(
                uuid::Uuid::new_v4(),
                secret,
                Grant {
                    controller: h.controller,
                    role,
                    scenes: Default::default(),
                    devices: if secret == restricted { Default::default() } else { ["light".into()].into() },
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
    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>);
    impl std::io::Read for Wire {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let mut input=self.1.lock().unwrap();
            if input.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into());}
            let n=out.len().min(input.len());for b in &mut out[..n]{*b=input.pop_front().unwrap();}Ok(n)
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
    let input=std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let mut owner = crate::transport_owner::TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                Wire(writes.clone(),input.clone()),
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
    input.lock().unwrap().extend([0xae,0x15,0xd3,37,0xee]);
    let read=owner.receive("bus").await.unwrap();
    h.feedback.apply(&read).unwrap();
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
    assert_eq!(received["status"]["module"]["module_id"], id);
    assert_eq!(received["status"]["module"]["fresh_channels"], 1);
    assert_eq!(received["status"]["module"]["slots"].as_array().unwrap().len(), 8);
    assert_eq!(received["status"]["module"]["slots"][2]["actual"]["level"], 37);
    assert_eq!(received["status"]["transport_epoch"], h.generation.epochs.transport.to_string());
    assert!(received["status"]["captured_ms"].as_str().unwrap().parse::<u64>().is_ok());
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
    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(1);
    loop {
        if let Some(write)=owner.step().await.unwrap(){write.result.unwrap();break;}
        assert!(std::time::Instant::now()<deadline,"complete channel reply must release the next query after turnaround");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(
        writes.lock().unwrap()[2],
        vec![0xae, 0x15, 0xa0, 0xf2, 0xee]
    );
    assert_eq!(h.feedback.snapshot().devices.len(), 6);
    // Read capability does not advertise control capability, including full-module
    // metadata attached to a single-channel receipt. Actual control remains 403.
    let restricted_page = json(poll_request(&h, &mut receiver, &mut scenes,
        api.handle(req("/api/v1/modules", restricted, None))).await).await;
    assert!(restricted_page["modules"].as_array().unwrap().iter()
        .flat_map(|m| m["slots"].as_array().unwrap())
        .filter(|slot| !slot["binding"].is_null())
        .all(|slot| slot["binding"]["actions"].as_array().unwrap().is_empty()));
    let restricted_query = json!({"module_id":id,"scope":{"kind":"channel","channel":1},"revision":"2","request_id":"restricted-query"});
    let accepted = json(poll_request(&h, &mut receiver, &mut scenes,
        api.handle(req("/api/v1/module-queries", restricted, Some(restricted_query)))).await).await;
    assert_eq!(accepted["outcome"], "accepted");
    let restricted_status = json(poll_request(&h, &mut receiver, &mut scenes,
        api.handle(req("/api/v1/module-queries/restricted-query", restricted, None))).await).await;
    assert_eq!(restricted_status["status"]["channels"][0]["binding"]["id"], "light");
    assert!(restricted_status["status"]["channels"][0]["binding"]["actions"].as_array().unwrap().is_empty());
    assert!(restricted_status["status"]["module"]["slots"].as_array().unwrap().iter()
        .filter(|slot| !slot["binding"].is_null())
        .all(|slot| slot["binding"]["actions"].as_array().unwrap().is_empty()));
    assert_eq!(restricted_status["status"]["fresh_channels"], 0, "accepted is not feedback");
    assert_eq!(restricted_status["status"]["feedback_causality"], "not_established");
    let before_denied: i64 = h.sql().query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0)).unwrap();
    let denied = poll_request(&h, &mut receiver, &mut scenes,
        api.handle(req("/api/v1/module-commands", restricted,
        Some(json!({"module_id":id,"channel":channel,"revision":"2","request_id":"restricted-control","command":{"action":"brightness","level":80}}))))).await;
    assert_eq!(denied.status(), hyper::StatusCode::FORBIDDEN);
    assert_eq!(json(denied).await["error"], "device_not_permitted");
    let after_denied: i64 = h.sql().query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0)).unwrap();
    assert_eq!(after_denied, before_denied, "denied control creates no write ticket");
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


/// Report layouts without constructing or polling the large client/driver
/// futures. The factory type identifies F, but the factory is never invoked.
#[tokio::test]
async fn diagnostic_exact_source_control_future_layout() {
    fn future_bytes<F:std::future::Future>(_:impl FnOnce()->F)->usize {std::mem::size_of::<F>()}
    let (config,rules)=inputs(false);
    let h=Box::pin(harness(config,rules)).await;
    let (control,mut receiver)=crate::automation_control::channel(h.controller,h.generation.clone());
    let mut scenes=crate::scene_scheduler::SceneScheduler::new(h.actor.clone(),h.controller,8,h.actor.monotonic_ms()).unwrap();
    let api=crate::web_api::Api{daily_backup:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),backup_source:None,publisher:None,origin:"http://127.0.0.1:8088".into(),tokens:std::sync::Arc::new(Tokens::default()),runtime:std::sync::RwLock::new(Some(crate::web_api::Runtime{control,revision:1,boot:h.actor.boot_id().into(),scenes:Default::default()})),receipts:std::sync::Mutex::new(crate::web_receipts::Receipts::open(&h.directory.join("layout-receipts.sqlite")).unwrap()),drafts:None};
    let request=||hyper::Request::builder().uri("/api/v1/modules").body(hyper::body::Bytes::new()).unwrap();
    let api_bytes=future_bytes(||api.handle(request()));
    let poll_bytes={let receiver=&mut receiver;let scenes=&mut scenes;let actor=&h.actor;let document=&h.document;let feedback=&h.feedback;future_bytes(move||receiver.poll(actor,document,scenes,feedback,5000,None))};
    let joined_bytes={let receiver=&mut receiver;let scenes=&mut scenes;let h=&h;let api=&api;future_bytes(move||poll_request(h,receiver,scenes,api.handle(request())))};
    let result=serde_json::json!({"source_bytes":std::mem::size_of::<crate::source_auth::Source>(),"optional_source_bytes":std::mem::size_of::<Option<crate::source_auth::Source>>(),"admission_bytes":std::mem::size_of::<crate::admission::Admission>(),"api_handle_future_bytes":api_bytes,"receiver_poll_future_bytes":poll_bytes,"joined_original_helper_future_bytes":joined_bytes,"large_factories_invoked":false,"test_stack_override":false});
    eprintln!("exact_source_future_layout={result}");
    if let Ok(path)=std::env::var("SOURCE_AUTH_LAYOUT_EVIDENCE"){std::fs::write(path,serde_json::to_vec_pretty(&result).unwrap()).unwrap();}
    drop(api);h.close().await;
}

#[tokio::test]
async fn module_get_pagination_covers_43_modules_and_preserves_read_only_boundaries() {
    // Cross the default page size with synthetic unbound modules. Inventory
    // must not issue hardware queries, controls, scene runs or admissions.
    let mut config: Json = serde_json::from_slice(&devices()).unwrap();
    for address in 0x80..0xa7 {
        config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({
            "address":format!("{address:02X}"), "profile":"r8i8_din_pro_v2_7_7",
            "verification":{"status":"simulated"}
        }));
    }
    let draft = crate::web_drafts::Draft {
        id: uuid::Uuid::new_v4(), expected_revision: 1, revision: 1,
        devices: config, automation: automation(),
    };
    let (draft, impact) = crate::module_migration::prepare(draft,
        crate::module_migration::Edit {
            id: uuid::Uuid::new_v4(), expected_revision: "1".into(), revision: "2".into(),
        }, ExecutionContext::Simulation).unwrap();
    assert_eq!(impact.modules.len(), 43);
    let expected: std::collections::BTreeSet<_> = impact.modules.into_iter().map(|m| m.module_id).collect();
    let h = harness(serde_json::to_vec(&draft.devices).unwrap(), draft.automation).await;
    let (control, mut receiver) = crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
    let tokens = std::sync::Arc::new(Tokens::default());
    let admin = "abcdefghijklmnopqrstuvwxyz0123456789";
    let member = "abcdefghijklmnopqrstuvwxyz9876543210";
    for (secret, role) in [(admin, Role::Administrator), (member, Role::Member)] {
        tokens.insert(uuid::Uuid::new_v4(), secret, Grant {
            controller: h.controller, role, scenes: Default::default(), devices: Default::default(),
        }).unwrap();
    }
    let api = crate::web_api::Api {
        daily_backup: Default::default(), hap_bridges: Default::default(), hap_status: Default::default(),
        backup_source: None, publisher: None, origin: "http://127.0.0.1:8088".into(), tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control, revision: 2, boot: h.actor.boot_id().into(), scenes: Default::default(),
        })),
        receipts: std::sync::Mutex::new(crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap()),
        drafts: None,
    };
    let request = |path: &str, secret: &str, method: &str, body: &str| {
        hyper::Request::builder().uri(path).method(method)
            .header("host", "127.0.0.1:8088").header("origin", "http://127.0.0.1:8088")
            .header("authorization", format!("Bearer {secret}"))
            .body(hyper::body::Bytes::copy_from_slice(body.as_bytes())).unwrap()
    };
    let mut ids = std::collections::BTreeSet::new();
    for (path, count, next) in [
        ("/api/v1/modules", 32, json!(32)),
        ("/api/v1/modules?offset=32&limit=32&revision=2", 11, Json::Null),
        ("/api/v1/modules?limit=64", 43, Json::Null),
        ("/api/v1/modules?offset=43&limit=1&revision=2", 0, Json::Null),
        ("/api/v1/modules?offset=16320&limit=64&revision=2", 0, Json::Null),
    ] {
        let response = poll_request(&h, &mut receiver, &mut scenes,
            api.handle(request(path, admin, "GET", ""))).await;
        assert_eq!(response.status(), hyper::StatusCode::OK, "legal pagination {path}");
        let page: Json = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(page["revision"], "2");
        assert_eq!(page["truth_source"], "canonical_module_channels");
        assert_eq!(page["next_offset"], next, "{path}");
        let modules = page["modules"].as_array().unwrap();
        assert_eq!(modules.len(), count, "{path}");
        assert!(page["commands"].as_array().unwrap().is_empty());
        if path == "/api/v1/modules?limit=64" { assert_eq!(ids, expected); }
        for module in modules {
            let id = module["module_id"].as_str().unwrap().to_owned();
            if path != "/api/v1/modules?limit=64" { assert!(ids.insert(id)); }
        }
    }
    for query in ["limit=0", "limit=65", "offset=16321&revision=2", "limit=01", "offset=00", "revision=02", "limit=+1", "limit=-1", "limit=184467440737095516160", "limit=%31", "limit=1&limit=2", "revision=2&revision=2", "offset=0&offset=0", "unknown=1", "limit", "limit="] {
        let path = format!("/api/v1/modules?{query}");
        assert_eq!(api.handle(request(&path, admin, "GET", "")).await.status(), hyper::StatusCode::BAD_REQUEST, "{path}");
    }
    for query in ["revision=1", "offset=32", "offset=32&revision=1"] {
        let path = format!("/api/v1/modules?{query}");
        assert_eq!(api.handle(request(&path, admin, "GET", "")).await.status(), hyper::StatusCode::CONFLICT, "{path}");
    }
    for path in ["/api/v1/modules", "/api/v1/modules?limit=64"] {
        assert_eq!(api.handle(request(path, member, "GET", "")).await.status(), hyper::StatusCode::FORBIDDEN, "{path}");
        for method in ["POST", "PUT", "DELETE", "PATCH", "HEAD"] {
            assert!(api.handle(request(path, admin, method, "")).await.status().is_client_error(), "{method} {path}");
        }
        assert_eq!(api.handle(request(path, admin, "GET", "{}")).await.status(), hyper::StatusCode::BAD_REQUEST, "{path}");
    }
    for path in ["/api/v1/devices?limit=64", "/api/v1/modules/extra?limit=64", "/api/v1/module-commands?limit=64"] {
        assert_eq!(api.handle(request(path, admin, "GET", "")).await.status(), hyper::StatusCode::BAD_REQUEST, "{path}");
    }
    h.no_output();
    h.close().await;
}
