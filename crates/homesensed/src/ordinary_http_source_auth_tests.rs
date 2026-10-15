//! Nested under automation_control so the harness can observe its mailbox.
//! HTTP -> actual control receiver -> durable admission -> memory wire.
use super::{Control, Receiver};
use crate::{
    admission::{Admission, Options},
    deferred_router::Router,
    feedback::Feedback,
    scene_scheduler::SceneScheduler,
    transport_owner::TransportOwner,
    web_api::{Api, Runtime},
    web_auth::{
        CredentialRequest, CredentialUpdate, IssuedCredential, Role, SessionSecret, Tokens,
    },
    web_receipts::Receipts,
};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use http_body_util::BodyExt;
use hyper::{Request, Response, body::Bytes};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};
use uuid::Uuid;
const ROOT: &str = "ordinary-auth-synthetic-root-0123456789";
#[derive(Clone, Copy, Debug)]
enum Case {
    Switch,
    Dimmer,
    Combined,
    MotorOpen,
    MotorStop,
    ModuleSwitch,
    ModuleMotor,
    Immediate,
    Later,
    Relative,
}
impl Case {
    fn module(self) -> bool {
        matches!(self, Self::ModuleSwitch | Self::ModuleMotor)
    }
    fn scene(self) -> bool {
        matches!(self, Self::Immediate | Self::Later | Self::Relative)
    }
    fn request(self) -> (&'static str, Value) {
        let id = Uuid::new_v4().to_string();
        match self {
            Self::Switch => (
                "/api/v1/device-commands",
                json!({"device":"switch","request_id":id,"command":{"action":"power","on":true}}),
            ),
            Self::Dimmer | Self::Combined => (
                "/api/v1/device-commands",
                json!({"device":if matches!(self,Self::Dimmer){"dimmer"}else{"combined"},"request_id":id,"command":{"action":"brightness","level":30}}),
            ),
            Self::MotorOpen | Self::MotorStop => (
                "/api/v1/device-commands",
                json!({"device":"motor","request_id":id,"command":{"action":if matches!(self,Self::MotorOpen){"open"}else{"stop"}}}),
            ),
            Self::ModuleSwitch | Self::ModuleMotor => (
                "/api/v1/module-commands",
                json!({"module_id":if matches!(self,Self::ModuleSwitch){"relay"}else{"curtain"},"channel":1,"revision":"1","request_id":id,"command":if matches!(self,Self::ModuleSwitch){json!({"action":"power","on":true})}else{json!({"action":"open"})}}),
            ),
            Self::Immediate | Self::Later => (
                "/api/v1/scene-actions",
                json!({"scene":if matches!(self,Self::Immediate){"instant"}else{"later"},"request_id":id,"on":true}),
            ),
            Self::Relative => (
                "/api/v1/scene-relative-actions",
                json!({"scene":"on","request_id":id}),
            ),
        }
    }
}
struct Wire(Arc<Mutex<Vec<u8>>>);
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Harness {
    directory: PathBuf,
    actor: Admission,
    clock: &'static AtomicU64,
    tokens: Arc<Tokens>,
    source: IssuedCredential,
    session: Option<SessionSecret>,
    api: Arc<Api>,
    control: Control,
    receiver: Receiver,
    scheduler: SceneScheduler,
    router: Router,
    document: Document,
    feedback: Feedback,
    bytes: Arc<Mutex<Vec<u8>>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
impl Harness {
    async fn new(case: Case, cookie: bool) -> Self {
        Self::configured(case, cookie, |_, _| {}).await
    }
    async fn configured(
        case: Case,
        cookie: bool,
        configure: impl FnOnce(&mut Value, &mut Value),
    ) -> Self {
        let directory = std::env::temp_dir().join(format!("ordinary-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut config = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"dim","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
            {"id":"curtain","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[
            {"id":"switch","name":"Switch","module_id":"relay","channel":1,"enabled":true},
            {"id":"supply","name":"Hidden supply","module_id":"relay","channel":2,"enabled":true},
            {"id":"dimmer","name":"Dimmer","module_id":"dim","channel":1,"enabled":true,"default_on_level":40},
            {"id":"combined","name":"Combined","module_id":"dim","channel":2,"enabled":true,"default_on_level":40,"supply":{"module_id":"relay","channel":2}},
            {"id":"motor","name":"Motor","module_id":"curtain","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"Synthetic","verified_at_unix_ms":1}}]});
        let step = |id: &str, on: bool, offset: u64| json!({"id":id,"enabled":true,"review_evidence":"Synthetic authorized scene","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":offset,"device":"switch","command":{"kind":"switch","on":on}}]});
        let mut rules = json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],
            "immediate_scenes":[{"id":"instant","enabled":true,"review_evidence":"Synthetic","activate":[{"source":"synthetic","action":{"kind":"command","device":"switch","command":{"kind":"switch","on":true}}}]}],
            "timed_scenes":[step("later",true,50),step("on",true,0),step("off",false,0),
                {"id":"low","enabled":true,"review_evidence":"Synthetic second group member","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"switch","command":{"kind":"switch","on":false}},{"source":"synthetic","offset_ms":0,"device":"dimmer","command":{"kind":"level","value":20,"ramp":true}}]}],
            "scene_switch_groups":[{"id":"selection","variable":"mode","review_evidence":"Synthetic selection","members":[{"scene":"on","value":1,"off_scene":"off"},{"scene":"low","value":2,"off_scene":"off"}],"reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"Synthetic"}}]});
        configure(&mut config, &mut rules);
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let rule_bytes = serde_json::to_vec(&rules).unwrap();
        let bundle = homesense_storage::Store::open(directory.join("runtime.sqlite"))
            .unwrap()
            .publish_automation(
                None,
                &config_bytes,
                &rule_bytes,
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let configuration =
            Configuration::parse(&config_bytes, ExecutionContext::Simulation).unwrap();
        let controller = Uuid::new_v4();
        let clock = Box::leak(Box::new(AtomicU64::new(100)));
        // This is the real production authorization shape. Source revocation
        // cannot be simulated by flipping this controller-wide predicate.
        let actor = Admission::open_with_test_time(
            directory.join("runtime.sqlite"),
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            move |id| id == controller,
            clock,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let generation = actor.task_generation().await.unwrap();
        actor
            .initialize_variables(
                controller,
                generation.clone(),
                bundle
                    .automation
                    .as_ref()
                    .unwrap()
                    .variables
                    .iter()
                    .map(|(id, v)| (id.clone(), v.clone()))
                    .collect(),
            )
            .await
            .unwrap();
        let (control, receiver) = super::channel(controller, generation.clone());
        let scheduler =
            SceneScheduler::new(actor.clone(), controller, 16, actor.monotonic_ms()).unwrap();
        let router = Router::new(actor.clone(), controller, &scheduler, 16).unwrap();
        let feedback = Feedback::new(
            &configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let tokens = Arc::new(Tokens::default());
        let devices = ["switch", "dimmer", "combined", "motor"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let scenes = ["instant", "later", "on"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        tokens
            .insert(
                Uuid::new_v4(),
                ROOT,
                crate::web_auth::Grant {
                    controller,
                    role: Role::Administrator,
                    devices: devices.clone(),
                    scenes: scenes.clone(),
                },
            )
            .unwrap();
        // Scene-only scope intentionally contains no individual device grants.
        let source = tokens
            .issue(
                CredentialRequest {
                    role: if case.module() {
                        Role::Administrator
                    } else {
                        Role::Member
                    },
                    devices: if case.scene() {
                        BTreeSet::new()
                    } else {
                        devices
                    },
                    scenes: if case.scene() {
                        scenes
                    } else {
                        BTreeSet::new()
                    },
                },
                Some(&format!("Bearer {ROOT}")),
                None,
                None,
            )
            .unwrap();
        let session = cookie.then(|| tokens.login(&format!("Bearer {}", source.secret)).unwrap());
        let document = bundle.automation.unwrap();
        let api = Arc::new(Api {
            daily_backup: Default::default(),
            hap_bridges: Default::default(),
            hap_status: Default::default(),
            backup_source: None,
            publisher: None,
            origin: "http://127.0.0.1:8088".into(),
            tokens: tokens.clone(),
            runtime: RwLock::new(Some(Runtime {
                control: control.clone(),
                revision: 1,
                boot: actor.boot_id().to_owned(),
                scenes: document.scene_catalog(),
            })),
            drafts: None,
            receipts: Mutex::new(Receipts::open(&directory.join("receipts.sqlite")).unwrap()),
        });
        Self {
            directory,
            actor,
            clock,
            tokens,
            source,
            session,
            api,
            control,
            receiver,
            scheduler,
            router,
            document,
            feedback,
            bytes: Arc::new(Mutex::new(vec![])),
        }
    }
    fn call(&self, case: Case) -> tokio::task::JoinHandle<Response<http_body_util::Full<Bytes>>> {
        let (path, body) = case.request();
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "127.0.0.1:8088")
            .header("origin", &self.api.origin)
            .header("content-type", "application/json");
        if let Some(session) = &self.session {
            request = request
                .header("cookie", format!("homesense_session={}", session.cookie))
                .header("x-csrf-token", &session.csrf);
        } else {
            request = request.header("authorization", format!("Bearer {}", self.source.secret));
        }
        let request = request.body(Bytes::from(body.to_string())).unwrap();
        let api = self.api.clone();
        tokio::spawn(async move { api.handle(request).await })
    }
    fn call_root(&self) -> tokio::task::JoinHandle<Response<http_body_util::Full<Bytes>>> {
        let (path, body) = Case::Switch.request();
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "127.0.0.1:8088")
            .header("origin", &self.api.origin)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {ROOT}"))
            .body(Bytes::from(body.to_string()))
            .unwrap();
        let api = self.api.clone();
        tokio::spawn(async move { api.handle(request).await })
    }
    async fn wait_queued(&self) {
        let end = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while self.receiver.receiver.is_empty() {
            assert!(
                tokio::time::Instant::now() < end,
                "HTTP did not reach control mailbox"
            );
            tokio::task::yield_now().await;
        }
        assert!(self.control.sender.capacity() < 32);
    }
    async fn finish_http(
        &mut self,
        task: tokio::task::JoinHandle<Response<http_body_util::Full<Bytes>>>,
    ) -> (u16, Value) {
        let end = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !task.is_finished() {
            assert!(tokio::time::Instant::now() < end, "control stalled");
            self.receiver
                .poll_causal(
                    &self.actor,
                    &self.document,
                    &mut self.scheduler,
                    &self.feedback,
                    5000,
                    None,
                    Some(&mut self.router),
                )
                .await;
            tokio::task::yield_now().await;
        }
        let response = task.await.unwrap();
        let code = response.status().as_u16();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (code, serde_json::from_slice(&body).unwrap())
    }
    fn change(&self, kind: &str) {
        let root = format!("Bearer {ROOT}");
        match kind {
            "revoke" => self
                .tokens
                .revoke_managed(self.source.id, Some(&root), None, None)
                .unwrap(),
            "logout" => self.tokens.logout(&self.session.as_ref().unwrap().cookie),
            _ => {
                let role = if kind == "role" {
                    if self.source.grant.role == Role::Administrator {
                        Role::Member
                    } else {
                        Role::Viewer
                    }
                } else {
                    self.source.grant.role.clone()
                };
                self.tokens
                    .update_grant(
                        self.source.id,
                        CredentialUpdate {
                            expected: self.source.grant.clone(),
                            role,
                            devices: if kind == "scope" {
                                BTreeSet::new()
                            } else {
                                self.source.grant.devices.clone()
                            },
                            scenes: if kind == "scope" {
                                BTreeSet::new()
                            } else {
                                self.source.grant.scenes.clone()
                            },
                        },
                        Some(&root),
                        None,
                        None,
                    )
                    .unwrap();
            }
        }
        assert!(self.tokens.authenticate(&root).is_some());
    }
    async fn release(&mut self) {
        // Timeline due time uses only the explicit private clock; no sleep.
        self.clock.store(200, Ordering::Release);
        let _ = self
            .scheduler
            .tick_with_feedback(200, Some((&self.feedback, 5000)))
            .await;
        let _ = self
            .router
            .poll(&mut self.scheduler, Some((&self.feedback, 5000)))
            .await;
        let mut owner = TransportOwner::new(
            self.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(self.bytes.clone()), 1),
            )]
            .into(),
        );
        let _ = owner.step().await.unwrap();
    }
}
fn evidence(name: &str, records: &[Value]) {
    if let Ok(root) = std::env::var("ORDINARY_AUTH_ARTIFACT_ROOT") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            PathBuf::from(root).join(name),
            serde_json::to_vec_pretty(records).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn ordinary_http_source_revocation_before_runtime_or_dispatch_must_emit_zero_frames() {
    let mut records = vec![];
    for case in [
        Case::Switch,
        Case::Dimmer,
        Case::Combined,
        Case::MotorOpen,
        Case::MotorStop,
        Case::ModuleSwitch,
        Case::ModuleMotor,
        Case::Immediate,
        Case::Later,
        Case::Relative,
    ] {
        for before_runtime in [true, false] {
            for change in ["role", "scope", "revoke", "logout"] {
                let mut h = Harness::new(case, change == "logout").await;
                let request = h.call(case);
                if before_runtime {
                    h.wait_queued().await;
                    h.change(change);
                }
                let (status, body) = h.finish_http(request).await;
                if !before_runtime {
                    assert!(
                        [200, 202].contains(&status),
                        "baseline request was not accepted: {body}"
                    );
                    h.change(change);
                }
                h.release().await;
                let bytes = h.bytes.lock().unwrap().clone();
                let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
                let written: i64 = sql
                    .query_row(
                        "SELECT count(*) FROM command_journal WHERE status='written'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                records.push(json!({"case":format!("{case:?}"),"before_runtime":before_runtime,"change":change,"http_status":status,"wire":bytes,"written":written,"valid_controller_still_present":true}));
                drop(sql);
                h.bytes.lock().unwrap().clear();
                let root = h.call_root();
                let (root_status, root_body) = h.finish_http(root).await;
                assert!(
                    [200, 202].contains(&root_status),
                    "fresh same-controller root rejected: {root_body}"
                );
                h.release().await;
                h.assert_response_written(&root_body);
                assert!(
                    !h.bytes.lock().unwrap().is_empty(),
                    "zero negative output came from unhealthy runtime rather than source revocation"
                );
                h.actor.clone().shutdown().await.unwrap();
            }
        }
    }
    evidence("ordinary-source-negative.json", &records);
    let failed = records
        .iter()
        .filter(|r| !r["wire"].as_array().unwrap().is_empty() || r["written"] != 0)
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "invalid source produced output: {failed:?}"
    );
}
#[tokio::test]
async fn unchanged_scoped_http_source_can_control_each_route_without_hidden_scope_expansion() {
    let mut records = vec![];
    for case in [
        Case::Switch,
        Case::Dimmer,
        Case::Combined,
        Case::MotorOpen,
        Case::MotorStop,
        Case::ModuleSwitch,
        Case::ModuleMotor,
        Case::Immediate,
        Case::Later,
        Case::Relative,
    ] {
        let mut h = Harness::new(case, false).await;
        let task = h.call(case);
        let (status, body) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{case:?}: {body}");
        h.release().await;
        let bytes = h.bytes.lock().unwrap().clone();
        assert!(
            !bytes.is_empty(),
            "{case:?}: unchanged source failed positive write"
        );
        records
            .push(json!({"case":format!("{case:?}"),"wire":bytes,"scene_only_scope":case.scene()}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-positive.json", &records);
}

// Extra deterministic probes; the baseline functions/assertions above remain.
include!("ordinary_source_continuation_tests.rs");
