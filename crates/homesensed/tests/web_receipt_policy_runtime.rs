//! Actual authenticated Web handler, runtime, Admission, persistent SQLite and
//! memory transport. No real credential, socket, gateway or field configuration.
use homesense_core::ExecutionContext;
use homesensed::{
    admission::{Admission, Options},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
    web_api::{Api, Runtime},
    web_auth::{Grant, Role, Tokens},
    web_receipt_policy::{Expiry, Policy},
    web_receipts::Receipts,
};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
struct Memory(Arc<Mutex<Vec<u8>>>);
impl std::io::Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Memory {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Lab {
    dir: std::path::PathBuf,
    api: Arc<Api>,
    actor: Admission,
    clock: Arc<AtomicU64>,
    bytes: Arc<Mutex<Vec<u8>>>,
    stop: tokio::sync::watch::Sender<bool>,
    receiver: Option<tokio::sync::watch::Receiver<bool>>,
    runtime: Option<AutomationRuntime>,
    owner: Option<ObservedTransport<Memory>>,
    driver: Option<tokio::task::JoinHandle<Result<(), String>>>,
    secret: String,
    token: Uuid,
}
impl Lab {
    async fn new(policy: Policy, start: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("web-policy-runtime-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let data=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"d","address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},{"id":"m","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"d","channel":1,"enabled":true},{"id":"motor","name":"Synthetic motor","module_id":"m","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic calibration","verified_at_unix_ms":1}}]})).unwrap();
        let rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic delayed vector","max_lateness_ms":1000,"steps":[{"source":"member","offset_ms":60000,"device":"light","command":{"kind":"level","value":30,"ramp":false}}]}]})).unwrap();
        let bundle = homesense_storage::Store::open(dir.join("runtime.sqlite"))
            .unwrap()
            .publish_automation(None, &data, &rules, ExecutionContext::Simulation, &[])
            .unwrap();
        let caller = Uuid::new_v4();
        let actor = Admission::open(
            dir.join("runtime.sqlite"),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == caller,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let runtime = AutomationRuntime::load(
            actor.clone(),
            caller,
            1_800_000_000_000,
            1000,
            16,
            Default::default(),
        )
        .await
        .unwrap();
        let control = runtime.control();
        let feedback = Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let bytes = Arc::new(Mutex::new(vec![]));
        let owner = ObservedTransport::new(
            TransportOwner::new(
                actor.clone(),
                [(
                    "bus".into(),
                    hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
                )]
                .into(),
            ),
            feedback,
        );
        let secret = format!("synthetic-local-{}", Uuid::new_v4().simple());
        let tokens = Arc::new(
            Tokens::open_bootstrapped(
                &dir.join("web-credentials.sqlite"),
                &secret,
                Grant {
                    controller: caller,
                    role: Role::Administrator,
                    scenes: ["later".into()].into(),
                    devices: ["light".into(), "motor".into()].into(),
                },
            )
            .unwrap(),
        );
        let token = tokens.summaries().unwrap()[0].id;
        let clock = Arc::new(AtomicU64::new(1000));
        let n = clock.clone();
        let receipts = Receipts::open_with_clock(
            &dir.join("web-receipts.sqlite"),
            policy,
            Arc::new(move || Ok(n.load(Ordering::Acquire))),
        )
        .unwrap();
        let api = Arc::new(Api {
            daily_backup: Default::default(),
            hap_bridges: Default::default(),
            hap_status: Default::default(),
            backup_source: None,
            publisher: None,
            origin: "http://127.0.0.1:8088".into(),
            tokens,
            runtime: RwLock::new(Some(Runtime {
                control,
                revision: 1,
                boot: actor.boot_id().into(),
                scenes: [("later".into(), "Synthetic later".into())].into(),
            })),
            drafts: None,
            receipts: Mutex::new(receipts),
        });
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let mut l = Self {
            dir,
            api,
            actor,
            clock,
            bytes,
            stop,
            receiver: Some(receiver),
            runtime: Some(runtime),
            owner: Some(owner),
            driver: None,
            secret,
            token,
        };
        if start {
            l.start();
        }
        l
    }
    fn start(&mut self) {
        let mut runtime = self.runtime.take().unwrap();
        let mut owner = self.owner.take().unwrap();
        let receiver = self.receiver.take().unwrap();
        self.driver = Some(tokio::spawn(async move {
            Box::pin(
                runtime.drive(&mut owner, Duration::from_millis(1), 1000, receiver, || {
                    Ok(1_800_000_000_000)
                }),
            )
            .await
        }));
    }
    fn make(&self, path: &str, body: Value) -> Request<Bytes> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "127.0.0.1:8088")
            .header("origin", &self.api.origin)
            .header("authorization", format!("Bearer {}", self.secret))
            .header("idempotency-key", Uuid::new_v4().to_string())
            .header("content-type", "application/json")
            .body(Bytes::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }
    async fn request(&self, path: &str, body: Value) -> (StatusCode, Value, bool) {
        let r = self.api.handle(self.make(path, body)).await;
        let expired = r.headers().get("x-homesense-receipt-state").is_some();
        let status = r.status();
        let body =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        (status, body, expired)
    }
    fn count(&self, db: &str, table: &str) -> i64 {
        rusqlite::Connection::open(self.dir.join(db))
            .unwrap()
            .query_row(&if table=="web_receipt"{"SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)".into()}else{format!("select count(*) from {table}")}, [], |r| r.get(0))
            .unwrap()
    }
    async fn wait_written(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.bytes.lock().unwrap().len() >= n {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn finish(self) {
        let _ = self.stop.send(true);
        if let Some(driver) = self.driver {
            driver.await.unwrap().unwrap();
        }
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}
fn policy(n: u32) -> Policy {
    Policy {
        max_records: n,
        expiry: Expiry::MarkExpiredPreserveFence { window_ms: 100 },
    }
}
fn device(key: &str, level: u8) -> Value {
    json!({"device":"light","revision":"1","request_id":key,"command":{"action":"brightness","level":level}})
}
#[tokio::test]
async fn real_http_expiry_capacity_and_canonical_route_aliases_never_repeat_output() {
    let l = Lab::new(policy(1), true).await;
    let first = l
        .request("/api/v1/device-commands", device("once", 40))
        .await;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(first.1["outcome"], "accepted");
    l.wait_written(5).await;
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x15, 1, 40, 0xee]);
    l.clock.store(1200, Ordering::Release);
    let again = l
        .request("/api/v1/device-commands", device("once", 40))
        .await;
    assert_eq!(again.0, StatusCode::OK);
    assert_eq!(first.1, again.1);
    assert!(again.2);
    assert_eq!(
        l.request("/api/v1/device-commands", device("once", 30))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        l.request("/api/v1/device-commands", device("new", 30))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    for path in [
        "/api/v1/%64evice-commands",
        "/api/v1/device-commands/",
        "/api/v1/./device-commands",
    ] {
        assert_eq!(
            l.request(path, device("new", 30)).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 1);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 1);
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x15, 1, 40, 0xee]);
    l.api.tokens.revoke(l.token).unwrap();
    assert_eq!(
        l.request("/api/v1/device-commands", device("once", 40))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    l.finish().await;
}
#[tokio::test]
async fn actual_cancel_and_query_lookups_use_their_owned_endpoint_not_same_named_device_receipt() {
    let l = Lab::new(policy(20), true).await;
    assert_eq!(
        l.request("/api/v1/device-commands", device("shared", 40))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        l.request(
            "/api/v1/scene-cancellations",
            json!({"start_request_id":"shared","request_id":"cancel-before"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let mut read = l.make("/api/v1/module-queries/shared", json!({}));
    *read.method_mut() = hyper::Method::GET;
    *read.body_mut() = Bytes::new();
    assert_eq!(l.api.handle(read).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        l.request(
            "/api/v1/scene-actions",
            json!({"scene":"later","request_id":"shared"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(l.request("/api/v1/module-queries",json!({"module_id":"d","scope":{"kind":"whole_module"},"revision":"1","request_id":"shared"})).await.0,StatusCode::ACCEPTED);
    let mut read = l.make("/api/v1/module-queries/shared", json!({}));
    *read.method_mut() = hyper::Method::GET;
    *read.body_mut() = Bytes::new();
    l.clock.store(1200, Ordering::Release);
    let response = l.api.handle(read).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-homesense-receipt-state"));
    let cancellation = l
        .request(
            "/api/v1/scene-cancellations",
            json!({"start_request_id":"shared","request_id":"shared"}),
        )
        .await;
    assert_eq!(cancellation.0, StatusCode::ACCEPTED);
    assert_eq!(l.count("runtime.sqlite", "scene_run"), 1);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 4);
    l.finish().await;
}
#[tokio::test]
async fn aborted_http_future_leaves_real_unknown_fence_across_expiry_without_runtime_replay() {
    let mut l = Lab::new(policy(3), false).await;
    let api = l.api.clone();
    let request = l.make("/api/v1/device-commands", device("aborted", 40));
    let task = tokio::spawn(async move { api.handle(request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while l.count("web-receipts.sqlite", "web_receipt") == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    l.clock.store(1200, Ordering::Release);
    l.start();
    let again = l
        .request("/api/v1/device-commands", device("aborted", 40))
        .await;
    assert_eq!(again.0, StatusCode::ACCEPTED);
    assert_eq!(again.1["outcome"], "unknown");
    assert_eq!(again.1["retry"], false);
    assert!(again.2);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 0);
    assert!(l.bytes.lock().unwrap().is_empty());
    assert_eq!(
        l.request("/api/v1/device-commands", device("fresh", 30))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 1);
    l.finish().await;
}
#[tokio::test]
async fn actual_http_concurrency_reserves_one_fence_and_one_admission() {
    let l = Lab::new(policy(2), true).await;
    let mut tasks = vec![];
    for _ in 0..8 {
        let api = l.api.clone();
        let req = l.make("/api/v1/device-commands", device("concurrent", 40));
        tasks.push(tokio::spawn(async move { api.handle(req).await.status() }));
    }
    let mut new = 0;
    for t in tasks {
        let s = t.await.unwrap();
        assert!(s == StatusCode::ACCEPTED || s == StatusCode::OK);
        if s == StatusCode::ACCEPTED {
            new += 1;
        }
    }
    assert!(new >= 1);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 1);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 1);
    l.finish().await;
}
#[tokio::test]
async fn configuration_early_lookup_canonical_uuid_preserves_response_and_expiry_metadata() {
    let mut l = Lab::new(policy(10), true).await;
    let principal = l
        .api
        .tokens
        .authenticate(&format!("Bearer {}", l.secret))
        .unwrap()
        .controller;
    let id = Uuid::new_v4();
    let (publisher, _receive) = homesensed::automation_publication::channel();
    let mut devices: Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["revision"] = 2.into();
    let automation =
        json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]});
    let draft = homesensed::web_drafts::Draft {
        id,
        expected_revision: 1,
        revision: 2,
        devices,
        automation,
    };
    let mut drafts = homesensed::web_drafts::Drafts::open(&l.dir.join("drafts.sqlite")).unwrap();
    drafts
        .save(principal, &draft, ExecutionContext::Simulation)
        .unwrap();
    let api = Arc::get_mut(&mut l.api).unwrap();
    api.publisher = Some(publisher);
    api.drafts = Some((ExecutionContext::Simulation, Mutex::new(drafts)));
    let endpoint = homesensed::web_receipts::Endpoint::ConfigurationActivate(id);
    let fingerprint=json!({"action":"activate","draft":id,"revision":2,"expected_revision":1,"boot":l.actor.boot_id()}).to_string();
    let original = json!({"request_id":"activation","outcome":"activated","revision":2});
    {
        let mut r = l.api.receipts.lock().unwrap();
        r.claim_scoped(
            &principal.to_string(),
            &endpoint,
            "activation",
            &fingerprint,
        )
        .unwrap();
        r.finish_scoped(
            &principal.to_string(),
            &endpoint,
            "activation",
            &original.to_string(),
        )
        .unwrap();
    }
    l.clock.store(1200, Ordering::Release);
    let path = format!(
        "/api/v1/config/activations/{}",
        id.to_string().to_uppercase()
    );
    let first = l
        .request(
            &path,
            json!({"request_id":"activation","boot":l.actor.boot_id()}),
        )
        .await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1, original);
    assert!(first.2);
    l.clock.store(0, Ordering::Release);
    let again = l
        .request(
            &format!("/api/v1/config/activations/{id}"),
            json!({"request_id":"activation","boot":l.actor.boot_id()}),
        )
        .await;
    assert_eq!(again.0, StatusCode::OK);
    assert_eq!(again.1, original);
    assert!(again.2);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 0);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 1);
    l.finish().await;
}
#[tokio::test]
async fn actual_http_finish_failure_preserves_unknown_and_prevents_expired_replay() {
    let l = Lab::new(policy(2), true).await;
    let sql = rusqlite::Connection::open(l.dir.join("web-receipts.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_receipt_finish BEFORE UPDATE OF response ON web_receipt_scoped BEGIN SELECT RAISE(ABORT,'synthetic finish failure'); END;").unwrap();
    let first = l
        .request("/api/v1/device-commands", device("unknown", 40))
        .await;
    assert_eq!(first.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first.1["error"], "receipt_unavailable_outcome_unknown");
    l.wait_written(5).await;
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x15, 1, 40, 0xee]);
    sql.execute_batch("DROP TRIGGER fail_receipt_finish;")
        .unwrap();
    drop(sql);
    l.clock.store(1200, Ordering::Release);
    let again = l
        .request("/api/v1/device-commands", device("unknown", 40))
        .await;
    assert_eq!(again.0, StatusCode::ACCEPTED);
    assert_eq!(again.1["outcome"], "unknown");
    assert_eq!(again.1["retry"], false);
    assert!(again.2);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 1);
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x15, 1, 40, 0xee]);
    l.finish().await;
}
#[tokio::test]
async fn motor_and_relative_unknown_receipts_are_preserved_after_expiry() {
    let l = Lab::new(policy(4), true).await;
    let body = json!({"device":"motor","revision":"1","request_id":"motor-once","command":{"action":"close"}});
    let first = l.request("/api/v1/device-commands", body.clone()).await;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    l.wait_written(5).await;
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x22, 1, 0xf2, 0xee]);
    l.clock.store(1200, Ordering::Release);
    let again = l.request("/api/v1/device-commands", body).await;
    assert_eq!(again.0, StatusCode::OK);
    assert_eq!(again.1, first.1);
    assert!(again.2);
    assert_eq!(*l.bytes.lock().unwrap(), [0xae, 0x22, 1, 0xf2, 0xee]);
    let relative = json!({"scene":"later","request_id":"relative-unknown"});
    let first = l
        .request("/api/v1/scene-relative-actions", relative.clone())
        .await;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(first.1["outcome"], "unknown");
    assert_eq!(first.1["retry"], false);
    l.clock.store(1400, Ordering::Release);
    let again = l.request("/api/v1/scene-relative-actions", relative).await;
    assert_eq!(again.0, StatusCode::OK);
    assert_eq!(again.1, first.1);
    assert!(again.2);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 1);
    l.finish().await;
}
#[tokio::test]
async fn credential_issue_retry_never_discloses_or_persists_secret_twice_and_mutations_are_scoped()
{
    let l = Lab::new(policy(20), true).await;
    let mut first = l.make(
        "/api/v1/credentials",
        json!({"role":"Member","devices":["light"],"scenes":[]}),
    );
    first
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let response = l.api.handle(first).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let secret = issued["secret"].as_str().unwrap();
    assert!(secret.len() >= 32);
    let mut again = l.make(
        "/api/v1/credentials",
        json!({"role":"Member","devices":["light"],"scenes":[]}),
    );
    again
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let response = l.api.handle(again).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let cached: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(cached["id"], issued["id"]);
    assert_eq!(cached["grant"], issued["grant"]);
    assert!(cached.get("secret").is_none());
    assert_eq!(cached["secret_disclosed_once"], true);
    assert_eq!(l.count("web-credentials.sqlite", "web_credential"), 2);
    let sql = rusqlite::Connection::open(l.dir.join("web-receipts.sqlite")).unwrap();
    let text: String = sql
        .query_row(
            "select fingerprint||coalesce(response,'') from web_receipt_scoped",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!text.contains(secret));
    drop(sql);
    let id = issued["id"].as_str().unwrap();
    let patch = json!({"expected":issued["grant"],"role":"Viewer","devices":["light"],"scenes":[]});
    let path = format!("/api/v1/credentials/{id}");
    let mut request = l.make(&path, patch.clone());
    *request.method_mut() = hyper::Method::PATCH;
    request
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let first = l.api.handle(request).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let mut request = l.make(&path, patch);
    *request.method_mut() = hyper::Method::PATCH;
    request
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let second = l.api.handle(request).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.into_body().collect().await.unwrap().to_bytes(),
        first
    );
    let path = format!("/api/v1/credentials/{id}/revoke");
    let mut request = l.make(&path, json!({}));
    *request.body_mut() = Bytes::new();
    request
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let first = l.api.handle(request).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let mut request = l.make(&path, json!({}));
    *request.body_mut() = Bytes::new();
    request
        .headers_mut()
        .insert("idempotency-key", "issue-once".parse().unwrap());
    let second = l.api.handle(request).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.into_body().collect().await.unwrap().to_bytes(),
        first
    );
    assert_eq!(l.count("web-credentials.sqlite", "web_credential"), 1);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 3);
    l.finish().await;
}
#[tokio::test]
async fn draft_save_edit_archive_and_history_restore_have_actual_request_records() {
    let mut l = Lab::new(policy(20), true).await;
    let api = Arc::get_mut(&mut l.api).unwrap();
    api.drafts = Some((
        ExecutionContext::Simulation,
        Mutex::new(homesensed::web_drafts::Drafts::open(&l.dir.join("drafts.sqlite")).unwrap()),
    ));
    let id = Uuid::new_v4();
    let mut devices: Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["revision"] = 2.into();
    let draft = json!({"expected_revision":1,"revision":2,"devices":devices,"automation":{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}});
    let path = format!("/api/v1/config/drafts/{id}");
    let send = |path: &str, body: Value| {
        let mut req = l.make(path, body);
        req.headers_mut()
            .insert("idempotency-key", "same-management-key".parse().unwrap());
        req
    };
    let first = l.api.handle(send(&path, draft.clone())).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let second = l.api.handle(send(&path, draft)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.into_body().collect().await.unwrap().to_bytes(),
        first
    );
    let derived = Uuid::new_v4();
    let path = format!("/api/v1/config/drafts/{id}/device-metadata");
    let body = json!({"id":derived,"expected_revision":"1","revision":"2","device":"simulation:light","name":"Renamed synthetic","room":null});
    assert_eq!(
        l.api.handle(send(&path, body.clone())).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        l.api.handle(send(&path, body)).await.status(),
        StatusCode::OK
    );
    assert_eq!(l.count("drafts.sqlite", "configuration_draft"), 2);
    let path = format!("/api/v1/config/drafts/{derived}/archive");
    let mut request = send(&path, json!({}));
    *request.body_mut() = Bytes::new();
    assert_eq!(l.api.handle(request).await.status(), StatusCode::OK);
    let mut request = send(&path, json!({}));
    *request.body_mut() = Bytes::new();
    assert_eq!(l.api.handle(request).await.status(), StatusCode::OK);
    assert_eq!(l.count("drafts.sqlite", "archived_draft"), 1);
    let path = "/api/v1/config/versions/1/restore-draft";
    let restored = Uuid::new_v4();
    let body = json!({"id":restored,"expected_revision":"1","revision":"3"});
    let first = l.api.handle(send(path, body.clone())).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let second = l.api.handle(send(path, body)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.into_body().collect().await.unwrap().to_bytes(),
        first
    );
    assert_eq!(l.count("drafts.sqlite", "configuration_draft"), 3);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 4);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 0);
    l.finish().await;
}
#[tokio::test]
async fn actual_actor_handler_restart_reuses_draft_record_without_boot_in_fingerprint() {
    let mut l = Lab::new(policy(10), true).await;
    let principal = l
        .api
        .tokens
        .authenticate(&format!("Bearer {}", l.secret))
        .unwrap()
        .controller;
    Arc::get_mut(&mut l.api).unwrap().drafts = Some((
        ExecutionContext::Simulation,
        Mutex::new(homesensed::web_drafts::Drafts::open(&l.dir.join("drafts.sqlite")).unwrap()),
    ));
    let id = Uuid::new_v4();
    let mut devices: Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["revision"] = 2.into();
    let body = json!({"expected_revision":1,"revision":2,"devices":devices,"automation":{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}});
    let path = format!("/api/v1/config/drafts/{id}");
    let mut request = l.make(&path, body.clone());
    request
        .headers_mut()
        .insert("idempotency-key", "draft-restart".parse().unwrap());
    let first = l.api.handle(request).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.into_body().collect().await.unwrap().to_bytes();
    let old_boot = l.actor.boot_id().to_owned();
    l.stop.send(true).unwrap();
    l.driver.take().unwrap().await.unwrap().unwrap();
    l.actor.shutdown().await.unwrap();
    let actor = Admission::open(
        l.dir.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == principal,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    assert_ne!(actor.boot_id(), old_boot);
    let runtime = AutomationRuntime::load(
        actor.clone(),
        principal,
        1_800_000_000_000,
        1000,
        16,
        Default::default(),
    )
    .await
    .unwrap();
    let config = homesense_storage::Store::open(l.dir.join("runtime.sqlite"))
        .unwrap()
        .active_bundle(ExecutionContext::Simulation)
        .unwrap()
        .unwrap()
        .configuration;
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let owner = ObservedTransport::new(
        TransportOwner::new(
            actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Memory(l.bytes.clone()), 1),
            )]
            .into(),
        ),
        feedback,
    );
    let api = Arc::get_mut(&mut l.api).unwrap();
    api.tokens = Arc::new(Tokens::open(&l.dir.join("web-credentials.sqlite")).unwrap());
    *api.runtime.write().unwrap() = Some(Runtime {
        control: runtime.control(),
        revision: 1,
        boot: actor.boot_id().into(),
        scenes: [("later".into(), "Synthetic later".into())].into(),
    });
    l.actor = actor;
    let (stop, receive) = tokio::sync::watch::channel(false);
    l.stop = stop;
    l.receiver = Some(receive);
    l.runtime = Some(runtime);
    l.owner = Some(owner);
    l.start();
    l.clock.store(1200, Ordering::Release);
    let mut request = l.make(&path, body);
    request
        .headers_mut()
        .insert("idempotency-key", "draft-restart".parse().unwrap());
    let again = l.api.handle(request).await;
    assert_eq!(again.status(), StatusCode::OK);
    assert!(again.headers().contains_key("x-homesense-receipt-state"));
    assert_eq!(again.into_body().collect().await.unwrap().to_bytes(), first);
    assert_eq!(l.count("drafts.sqlite", "configuration_draft"), 1);
    assert_eq!(l.count("web-receipts.sqlite", "web_receipt"), 1);
    assert_eq!(l.count("runtime.sqlite", "command_journal"), 0);
    l.finish().await;
}
