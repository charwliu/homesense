//! Synthetic Immediate -> real Control/Admission/causal owner -> memory wire.
use crate::{
    admission::{Admission, Options, TaskGeneration},
    automation_control::{Action, Control, Receiver, Reply},
    deferred_router::Router,
    feedback::Feedback,
    scene_scheduler::SceneScheduler,
    transport_owner::{GatewayRead, TransportOwner},
};
use homesense_config::{
    Configuration,
    rule_document::{Document, SceneExecutionKind},
};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use serde_json::{Value as Json, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
fn artifact(name: &str, value: Json) {
    if let Ok(p) = std::env::var("IMMEDIATE_ARTIFACT_ROOT") {
        let p = std::path::Path::new(&p).join("synthetic");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(name), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }
}
fn data() -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"dim","address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dim","channel":1,"enabled":true},{"id":"relay","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true},{"id":"marker","name":"Synthetic variable marker","module_id":"relay","channel":2,"enabled":true}]})).unwrap()
}
fn level(n: u8) -> Json {
    json!({"kind":"level","value":n,"ramp":false})
}
fn command(source: &str, device: &str, command: Json) -> Json {
    json!({"source":source,"action":{"kind":"command","device":device,"command":command}})
}
fn rules() -> Json {
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"v","kind":"integer","lifetime":"persistent"},{"id":"relay_on","kind":"bool","lifetime":"feedback_cache","feedback":{"kind":"on","device":"relay"}}],"rules":[{"id":"manual-immediate-scenes","source":"synthetic/variable-rule","enabled":true,"review_evidence":"synthetic variable cascade","trigger":{"kind":"variable_changed","variable":"v"},"actions":[command("variable-change/output","marker",json!({"kind":"switch","on":true}))]}],"immediate_scenes":[{"id":"act","enabled":true,"review_evidence":"explicit synthetic immediate","activate":[{"source":"caller/nested","action":{"kind":"scene","id":"nested","activate":true}},{"source":"caller/set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":1}}},{"source":"caller/conditional","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"relay_on"},"right":{"kind":"bool","value":true}},"then_actions":[command("conditional/then","relay",json!({"kind":"switch","on":false}))],"else_actions":[command("conditional/else","relay",json!({"kind":"switch","on":true}))]}}]},{"id":"nested","enabled":true,"review_evidence":"synthetic ordered overlap","activate":[command("member/first","light",level(20)),command("member/second","light",level(40))]},{"id":"held","enabled":false,"review_evidence":"synthetic disabled","activate":[command("held/output","light",level(99))]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic real timeline","max_lateness_ms":1000,"steps":[{"source":"timed/member","offset_ms":0,"device":"relay","command":{"kind":"switch","on":true}}]}],"scene_metadata":[{"id":"act","name":"Synthetic immediate action"}]})
}
struct Harness {
    actor: Admission,
    document: Document,
    configuration: Configuration,
    feedback: Feedback,
    controller: Uuid,
    member: Uuid,
    generation: TaskGeneration,
    allowed: Arc<AtomicBool>,
    control: Control,
    receiver: Receiver,
    scheduler: SceneScheduler,
    router: Router,
    directory: std::path::PathBuf,
}
impl Harness {
    async fn new(rules: Json) -> Self {
        Self::from_inputs(data(), rules).await
    }
    async fn from_inputs(data: Vec<u8>, rules: Json) -> Self {
        let directory = std::env::temp_dir().join(format!("immediate-scene-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
        let cfg = Configuration::parse(&data, ExecutionContext::Simulation).unwrap();
        Document::parse(&serde_json::to_vec(&rules).unwrap(), &cfg).unwrap();
        let bundle = Store::open(&path)
            .unwrap()
            .publish_automation(
                None,
                &data,
                &serde_json::to_vec(&rules).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let controller = Uuid::new_v4();
        let member = Uuid::new_v4();
        let allowed = Arc::new(AtomicBool::new(true));
        let permit = allowed.clone();
        let actor = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller || (id == member && permit.load(Ordering::Acquire)),
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let generation = actor.task_generation().await.unwrap();
        let (control, receiver) =
            crate::automation_control::channel(controller, generation.clone());
        let scheduler =
            SceneScheduler::new(actor.clone(), controller, 16, actor.monotonic_ms()).unwrap();
        let router = Router::new(actor.clone(), controller, &scheduler, 16).unwrap();
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
                    .filter(|(_, v)| v.lifetime != homesense_core::rule::Lifetime::FeedbackCache)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            )
            .await
            .unwrap();
        let feedback = Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            generation.epochs.transport,
        )
        .unwrap();
        Self {
            actor,
            document: bundle.automation.unwrap(),
            configuration: bundle.configuration,
            feedback,
            controller,
            member,
            generation,
            allowed,
            control,
            receiver,
            scheduler,
            router,
            directory,
        }
    }
    fn samples(&mut self, on: bool) {
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: self.actor.monotonic_ms(),
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 3,
                        command: 0xd1,
                        value: if on { 0xf1 } else { 0xf2 },
                    }],
                },
            })
            .unwrap();
    }
    async fn drive<T: Send + 'static>(&mut self, task: tokio::task::JoinHandle<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !task.is_finished() {
            assert!(tokio::time::Instant::now() < deadline, "control stalled");
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
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        task.await.unwrap()
    }
    async fn request(&mut self, action: Action) -> Result<Reply, String> {
        let control = self.control.clone();
        self.drive(tokio::spawn(async move { control.request(action).await }))
            .await
    }
    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open(self.directory.join("runtime.sqlite"))
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    async fn wire(&self) -> Vec<u8> {
        let bytes = Arc::new(Mutex::new(vec![]));
        let mut owner = TransportOwner::new(
            self.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
            )]
            .into(),
        );
        for _ in 0..64 {
            let Some(outcome) = owner.step().await.unwrap() else {
                break;
            };
            outcome.result.unwrap();
        }
        bytes.lock().unwrap().clone()
    }
    async fn finish(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
struct Memory(Arc<Mutex<Vec<u8>>>);
impl std::io::Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Memory {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn bridge(
    h: &Harness,
) -> (
    homesense_hap::storage::IdentityStore,
    crate::runtime_scene_hap::RuntimeSceneHap,
) {
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let member = h.member;
    let bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![
            homesense_hap::scene_graph::SceneSpec {
                id: "act".into(),
                name: "Synthetic".into(),
            },
            homesense_hap::scene_graph::SceneSpec {
                id: "later".into(),
                name: "Timed".into(),
            },
        ],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        move |who, id| who == member && ["act", "later"].contains(&id),
    )
    .await
    .unwrap();
    (identity, bridge)
}
#[tokio::test]
async fn hap_immediate_preserves_nested_sources_repeated_targets_variables_and_caller() {
    let mut r = rules();
    r["scene_tracking"] = json!([{"scene":"act","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic immediate activity","max_age_ms":5000,"review_evidence":"synthetic activity scope","members":[{"id":"power","device":"relay","predicate":{"kind":"power","on":true},"review_evidence":"synthetic actual relay"}]}]);
    let mut h = Harness::new(r).await;
    h.samples(true);
    let (identity, bridge) = bridge(&h).await;
    let state = &bridge.activities[0].0;
    let state_body = serde_json::to_vec(
        &json!({"characteristics":[{"aid":state.aid,"iid":state.on_iid,"value":false}]}),
    )
    .unwrap();
    assert_eq!(
        bridge
            .activity_write_handler
            .as_ref()
            .unwrap()
            .write_async(h.member, &state_body)
            .await,
        Err(hap::server::WriteStatus::ReadOnlyCharacteristic)
    );
    assert_eq!(
        bridge
            .write_handler
            .write_async(h.member, &state_body)
            .await,
        Err(hap::server::WriteStatus::ResourceDoesNotExist)
    );
    let b_timed = &bridge
        .accessories
        .iter()
        .find(|(b, _)| b.scene == "later")
        .unwrap()
        .0;
    let b_act = &bridge
        .accessories
        .iter()
        .find(|(b, _)| b.scene == "act")
        .unwrap()
        .0;
    let invalid=serde_json::to_vec(&json!({"characteristics":[{"aid":b_act.aid,"iid":b_act.on_iid,"value":true},{"aid":b_timed.aid,"iid":999999,"value":true}]})).unwrap();
    assert!(
        bridge
            .write_handler
            .write_async(h.member, &invalid)
            .await
            .is_err()
    );
    assert_eq!(h.count("command_journal"), 0);
    let b = &bridge
        .accessories
        .iter()
        .find(|(b, _)| b.scene == "act")
        .unwrap()
        .0;
    let body =
        serde_json::to_vec(&json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":true}]}))
            .unwrap();
    let handler = bridge.write_handler.clone();
    let member = h.member;
    assert_eq!(
        handler.write_async(h.controller, &body).await,
        Err(hap::server::WriteStatus::InsufficientPrivileges)
    );
    assert_eq!(h.count("command_journal"), 0);
    h.drive(tokio::spawn(async move {
        handler.write_async(member, &body).await
    }))
    .await
    .unwrap();
    let wire = h.wire().await;
    assert_eq!(
        wire,
        [
            0xae, 0x15, 1, 20, 0xee, 0xae, 0x15, 1, 40, 0xee, 0xae, 3, 1, 0xf2, 0xee, 0xae, 3, 2,
            0xf1, 0xee
        ]
    );
    assert_eq!(h.count("scene_run"), 0);
    assert_eq!(h.count("rule_admission"), 2);
    let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let mut q = sql
        .prepare("SELECT source FROM rule_command ORDER BY command_id")
        .unwrap();
    let sources = q
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        sources[0].contains("caller/nested -> member/first"),
        "{sources:?}"
    );
    assert!(sources[1].ends_with("member/second"));
    assert!(sources[2].ends_with("conditional/then"));
    assert_eq!(sources[3], "variable-change/output");
    drop(q);
    drop(sql);
    let value = h
        .actor
        .variable_snapshot(h.member, h.generation.clone(), vec!["v".into()])
        .await
        .unwrap();
    assert_eq!(
        value["v"].value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    artifact(
        "hap-immediate-ordered-wire.json",
        json!({"wire":wire,"sources":sources,"rule_admissions":2,"scene_runs":0,"runtime_controller_differs_from_caller":h.controller!=h.member,"read_side_commands":0}),
    );
    drop(bridge);
    drop(identity);
    h.finish().await;
}
#[tokio::test]
async fn mixed_batch_validates_every_item_before_atomic_immediate_and_real_timed_admission() {
    let mut h = Harness::new(rules()).await;
    h.samples(false);
    for bad in ["held", "missing", "act"] {
        let writes = vec![("act".into(), Some(true)), (bad.into(), Some(true))];
        let result = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: writes,
            })
            .await;
        assert!(result.is_err(), "{bad}: {result:?}");
        assert_eq!(h.count("command_journal"), 0);
        assert_eq!(h.count("scene_run"), 0);
    }
    let reply = h
        .request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), Some(true)), ("later".into(), Some(true))],
        })
        .await
        .unwrap();
    let Reply::ImmediateAccepted(receipt) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(receipt.scenes, ["act", "later"]);
    assert_eq!(receipt.followup_runs.len(), 1);
    assert_eq!(h.count("scene_run"), 1);
    assert_eq!(h.count("command_journal"), 4);
    let first = h.wire().await;
    assert_eq!(first.len(), 20);
    let (outcomes, unowned) = h
        .router
        .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
        .await
        .unwrap();
    assert!(unowned.is_empty());
    assert!(outcomes.is_empty());
    let second = h.wire().await;
    assert_eq!(second, [0xae, 3, 1, 0xf1, 0xee]);
    let (_, unowned) = h
        .router
        .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
        .await
        .unwrap();
    assert!(unowned.is_empty());
    assert_eq!(h.router.retained_count(), 0);
    artifact(
        "mixed-batch-atomic-receipt.json",
        json!({"receipt":receipt,"first_wire":first,"timed_wire":second,"invalid_batches_output":0,"real_timed_runs":1}),
    );
    h.finish().await;
}
#[tokio::test]
async fn readonly_alias_wrong_role_disabled_nested_and_stale_revision_fail_without_output() {
    let mut r = rules();
    r["immediate_scenes"][1]["enabled"] = false.into();
    let mut h = Harness::new(r).await;
    assert!(!h.document.scene_catalog().contains_key("held"));
    assert_eq!(
        h.document.scene_execution_kind("act"),
        Some(SceneExecutionKind::Immediate)
    );
    for (revision, scene) in [
        (1, "act"),
        (1, "held"),
        (1, "sact:act"),
        (1, "act/activity"),
        (2, "act"),
    ] {
        assert!(
            h.request(Action::SceneAction {
                controller: h.member,
                revision,
                scene: scene.into(),
                execute: true
            })
            .await
            .is_err()
        );
        assert_eq!(h.count("command_journal"), 0);
    }
    assert!(
        h.actor.task_generation().await.is_ok(),
        "invalid request cannot stop runtime"
    );
    assert!(
        h.request(Action::SceneActivityWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), Some(true))]
        })
        .await
        .is_err()
    );
    assert_eq!(h.count("command_journal"), 0);
    h.finish().await;
}
#[tokio::test]
async fn subscription_and_false_are_actions_noops_unknown_feedback_does_not_select_else() {
    let mut h = Harness::new(rules()).await;
    for value in [None, Some(false)] {
        let result = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: vec![("act".into(), value)],
            })
            .await
            .unwrap();
        assert_eq!(result, Reply::ScenesAccepted(vec![]));
        assert_eq!(h.count("command_journal"), 0);
    }
    let result = h
        .request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "act".into(),
            execute: true,
        })
        .await
        .unwrap();
    let Reply::ImmediateAccepted(receipt) = result else {
        panic!("{result:?}")
    };
    assert_eq!(receipt.commands.len(), 3);
    assert_eq!(receipt.unknown_branches.len(), 1);
    let wire = h.wire().await;
    assert_eq!(
        wire,
        [
            0xae, 0x15, 1, 20, 0xee, 0xae, 0x15, 1, 40, 0xee, 0xae, 3, 2, 0xf1, 0xee
        ]
    );
    assert_eq!(h.count("scene_run"), 0);
    h.finish().await;
}
#[tokio::test]
async fn caller_revocation_before_dispatch_and_small_budget_do_not_fall_back_to_owner() {
    let mut h = Harness::new(rules()).await;
    h.samples(true);
    let result = h
        .request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "act".into(),
            execute: true,
        })
        .await
        .unwrap();
    assert!(matches!(result, Reply::ImmediateAccepted(_)));
    h.allowed.store(false, Ordering::Release);
    assert!(h.wire().await.is_empty());
    assert!(h.actor.task_generation().await.is_ok());
    assert!(
        h.request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), None)]
        })
        .await
        .is_err()
    );
    h.finish().await;
    let mut h = Harness::new(rules()).await;
    h.receiver.set_budget(homesense_core::rule::Budget {
        nodes: 2,
        commands: 1,
        depth: 4,
    });
    assert!(
        h.request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "act".into(),
            execute: true
        })
        .await
        .is_err()
    );
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(h.count("scene_run"), 0);
    assert!(h.actor.task_generation().await.is_ok());
    h.finish().await;
}
fn web(h: &Harness) -> Arc<crate::web_api::Api> {
    use crate::web_auth::{Grant, Role, Tokens};
    let tokens = Arc::new(Tokens::default());
    for (secret, role) in [
        (
            "synthetic-immediate-member-abcdefghijklmnopqrstuvwxyz",
            Role::Member,
        ),
        (
            "synthetic-immediate-viewer-abcdefghijklmnopqrstuvwxyz",
            Role::Viewer,
        ),
    ] {
        tokens
            .insert(
                Uuid::new_v4(),
                secret,
                Grant {
                    controller: h.member,
                    role,
                    scenes: ["act".into()].into(),
                    devices: Default::default(),
                },
            )
            .unwrap();
    }
    Arc::new(crate::web_api::Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://localhost".into(),
        tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control: h.control.clone(),
            revision: 1,
            boot: h.actor.boot_id().into(),
            scenes: h.document.scene_catalog(),
        })),
        drafts: None,
        receipts: Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap(),
        ),
    })
}
async fn web_request(
    h: &mut Harness,
    api: Arc<crate::web_api::Api>,
    viewer: bool,
    body: Option<Json>,
) -> (hyper::StatusCode, Json) {
    use http_body_util::BodyExt;
    let secret = if viewer {
        "synthetic-immediate-viewer-abcdefghijklmnopqrstuvwxyz"
    } else {
        "synthetic-immediate-member-abcdefghijklmnopqrstuvwxyz"
    };
    let request = hyper::Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(if body.is_some() {
            "/api/v1/scene-actions"
        } else {
            "/api/v1/scenes"
        })
        .header("host", "localhost")
        .header("origin", "http://localhost")
        .header("authorization", format!("Bearer {secret}"))
        .header("content-type", "application/json")
        .body(hyper::body::Bytes::from(
            body.map_or_else(String::new, |b| b.to_string()),
        ))
        .unwrap();
    let response = h
        .drive(tokio::spawn(async move { api.handle(request).await }))
        .await;
    let status = response.status();
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}
#[tokio::test]
async fn web_immediate_catalog_authorization_real_receipt_and_idempotency_are_not_timeline_success()
{
    let mut h = Harness::new(rules()).await;
    h.samples(true);
    let api = web(&h);
    let (status, list) = web_request(&mut h, api.clone(), false, None).await;
    assert_eq!(status, hyper::StatusCode::OK);
    assert_eq!(list["scenes"].as_array().unwrap().len(), 1);
    let scene = &list["scenes"][0];
    assert_eq!(scene["execution_kind"], "immediate");
    assert_eq!(scene["history_available"], false);
    assert_eq!(scene["last_run"], Json::Null);
    assert_eq!(scene["activity"], "Unknown");
    assert_eq!(scene["activity_source"], "not_configured");
    assert_eq!(scene["can_control"], true);
    assert_eq!(h.count("command_journal"), 0);
    for (viewer, id) in [
        (true, "act"),
        (false, "later"),
        (false, "held"),
        (false, "sact:act"),
    ] {
        let (status, _) = web_request(
            &mut h,
            api.clone(),
            viewer,
            Some(json!({"scene":id,"request_id":"denied"})),
        )
        .await;
        assert_eq!(status, hyper::StatusCode::FORBIDDEN);
        assert_eq!(h.count("command_journal"), 0);
    }
    let body = json!({"scene":"act","request_id":"one-explicit-action"});
    let (status, result) = web_request(&mut h, api.clone(), false, Some(body.clone())).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED);
    assert_eq!(result["outcome"], "accepted");
    assert_eq!(result["execution_kind"], "immediate");
    assert!(result.get("run").is_none());
    assert_eq!(result["receipt"]["commands"].as_array().unwrap().len(), 4);
    assert_eq!(h.count("scene_run"), 0);
    let (status, repeat) = web_request(&mut h, api.clone(), false, Some(body)).await;
    assert_eq!(status, hyper::StatusCode::OK);
    assert_eq!(repeat, result);
    assert_eq!(h.count("command_journal"), 4);
    let wire = h.wire().await;
    assert_eq!(wire.len(), 20);
    artifact(
        "web-immediate-receipt.json",
        json!({"view":list,"receipt":result,"wire":wire,"replayed_commands":0,"field_contact":false}),
    );
    drop(api);
    h.finish().await;
}
#[tokio::test]
async fn production_runtime_loop_drives_immediate_control_and_persistent_restart_does_not_replay() {
    use crate::{
        automation_runtime::{AutomationRuntime, StartupPolicy},
        observed_transport::ObservedTransport,
    };
    let mut h = Harness::new(rules()).await;
    h.samples(true);
    let mut runtime = AutomationRuntime::load_with_startup(
        h.actor.clone(),
        h.controller,
        1_800_000_000_000,
        1000,
        16,
        Default::default(),
        StartupPolicy::Suppress,
    )
    .await
    .unwrap();
    let control = runtime.control();
    let empty = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let feedback = std::mem::replace(&mut h.feedback, empty);
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            h.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
            )]
            .into(),
        ),
        feedback,
    );
    let (stop, receive) = tokio::sync::watch::channel(false);
    let drive = tokio::spawn(async move {
        runtime
            .drive(&mut owner, Duration::from_millis(2), 5000, receive, || {
                Ok(1_800_000_000_000)
            })
            .await
    });
    let receipt = control
        .request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "act".into(),
            execute: true,
        })
        .await
        .unwrap();
    assert!(matches!(receipt, Reply::ImmediateAccepted(_)));
    let end = tokio::time::Instant::now() + Duration::from_secs(2);
    while bytes.lock().unwrap().len() < 20 {
        assert!(tokio::time::Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    stop.send(true).unwrap();
    drive.await.unwrap().unwrap();
    assert_eq!(bytes.lock().unwrap().len(), 20);
    let before = h.count("command_journal");
    h.actor.clone().shutdown().await.unwrap();
    let member = h.member;
    let controller = h.controller;
    let reopened = Admission::open(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == member || id == controller,
    )
    .await
    .unwrap();
    reopened.synchronized(1, 1).await.unwrap();
    let runtime = AutomationRuntime::load_with_startup(
        reopened.clone(),
        controller,
        1_800_000_000_000,
        1000,
        16,
        Default::default(),
        StartupPolicy::Suppress,
    )
    .await
    .unwrap();
    let generation = reopened.task_generation().await.unwrap();
    let value = reopened
        .variable_snapshot(member, generation, vec!["v".into()])
        .await
        .unwrap();
    assert_eq!(
        value["v"].value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    assert!(reopened.dispatch().await.unwrap().is_none());
    assert_eq!(h.count("command_journal"), before);
    drop(runtime);
    reopened.shutdown().await.unwrap();
    artifact(
        "production-control-restart.json",
        json!({"wire":*bytes.lock().unwrap(),"persistent_value":1,"restart_new_commands":0,"actual_runtime_drive":true,"field_contact":false}),
    );
    std::fs::remove_dir_all(h.directory).unwrap();
}
#[tokio::test]
async fn manual_motor_predicate_uses_published_source_scope_and_requires_per_predicate_review() {
    let mut config: Json = serde_json::from_slice(&data()).unwrap();
    config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"motor-module","address":"37","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    config["devices"].as_array_mut().unwrap().push(json!({"id":"motor","name":"Synthetic motor","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic calibration","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"evidence":"synthetic model","reviewed_at_unix_ms":1}}));
    let condition = json!({"kind":"motor_state","device":"motor","state":"stopped","evidence_policy":"reviewed_command_estimate","max_age_ms":5000,"mapping_review":"synthetic mapping","evidence_review":"synthetic individual predicate review"});
    let mut r = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":[{"id":"guarded","enabled":true,"review_evidence":"synthetic standalone scene","activate":[{"source":"motor/condition","action":{"kind":"if","condition":condition,"then_actions":[command("motor/then","relay",json!({"kind":"switch","on":true}))],"else_actions":[command("motor/else","relay",json!({"kind":"switch","on":false}))]}}]}]});
    let cfg = Configuration::parse(
        &serde_json::to_vec(&config).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    for field in ["mapping_review", "evidence_review"] {
        let mut invalid = r.clone();
        invalid["immediate_scenes"][0]["activate"][0]["action"]["condition"][field] = "".into();
        assert!(Document::parse(&serde_json::to_vec(&invalid).unwrap(), &cfg).is_err());
    }
    let mut invalid = r.clone();
    invalid["immediate_scenes"][0]["activate"][0]["action"]["condition"]["evidence_policy"] =
        "actual_only".into();
    assert!(Document::parse(&serde_json::to_vec(&invalid).unwrap(), &cfg).is_err());
    let mut h = Harness::from_inputs(serde_json::to_vec(&config).unwrap(), r.clone()).await;
    let allowed = h.document.enabled_motor_requests().unwrap();
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0].source, "manual-scene/guarded -> motor/condition");
    let result = h
        .request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "guarded".into(),
            execute: true,
        })
        .await
        .unwrap();
    let Reply::ImmediateAccepted(receipt) = result else {
        panic!("{result:?}")
    };
    assert_eq!(receipt.commands.len(), 0);
    assert_eq!(receipt.unknown_branches.len(), 1);
    assert_eq!(h.count("command_journal"), 0);
    assert!(h.actor.task_generation().await.is_ok());
    h.finish().await;
    r["immediate_scenes"][0]["enabled"] = false.into();
    let document = Document::parse(&serde_json::to_vec(&r).unwrap(), &cfg).unwrap();
    assert!(document.enabled_motor_requests().unwrap().is_empty());
}
#[tokio::test]
async fn captured_generation_and_original_request_deadline_cannot_rebind_after_publication() {
    let mut h = Harness::new(rules()).await;
    let old = h.generation.clone();
    let mut config: Json = serde_json::from_slice(&data()).unwrap();
    config["revision"] = 2.into();
    let mut r = rules();
    r["configuration_revision"] = 2.into();
    h.actor
        .begin_automation_publication(
            serde_json::to_vec(&config).unwrap(),
            serde_json::to_vec(&r).unwrap(),
            1,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(h.actor.commit_publication().await.unwrap(), 2);
    h.actor.synchronized(1, 2).await.unwrap();
    let ids = vec![("act".to_owned(), Some(true))];
    let result = h
        .router
        .start_manual_scenes_as(
            crate::deferred_router::ManualRoot {
                controller: h.member,
                writes: &ids,
                generation: &old,
                deadline: h.actor.monotonic_ms() + 1000,
            },
            &mut h.scheduler,
            Some((&h.feedback, 5000)),
            Default::default(),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(h.count("rule_admission"), 0);
    let mut chain = homesense_core::rule_chain::Chain::new(
        3,
        h.actor.monotonic_ms(),
        1000,
        Default::default(),
        32,
    )
    .unwrap();
    let parent = chain.root();
    assert!(
        crate::deferred_rule::execute_manual_scenes_classified(
            crate::deferred_rule::Caller {
                admission: &h.actor,
                controller: h.member,
                generation: &old,
                admit_before: None,
            },
            &ids,
            &mut chain,
            &parent,
            &mut h.scheduler,
            Some((&h.feedback, 5000))
        )
        .await
        .is_err()
    );
    assert_eq!(h.count("command_journal"), 0);
    let current = h.actor.task_generation().await.unwrap();
    let result = h
        .router
        .start_manual_scenes_as(
            crate::deferred_router::ManualRoot {
                controller: h.member,
                writes: &ids,
                generation: &current,
                deadline: h.actor.monotonic_ms(),
            },
            &mut h.scheduler,
            Some((&h.feedback, 5000)),
            Default::default(),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(h.count("command_journal"), 0);
    assert!(h.actor.task_generation().await.is_ok());
    h.finish().await;
}
#[tokio::test]
async fn accepted_delayed_timed_work_outlives_request_ttl_without_retiming() {
    let mut r = rules();
    r["timed_scenes"][0]["steps"][0]["offset_ms"] = 5200.into();
    let mut h = Harness::new(r).await;
    h.samples(true);
    let reply = h
        .request(Action::SceneActions {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), true), ("later".into(), true)],
        })
        .await
        .unwrap();
    let Reply::ImmediateAccepted(receipt) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(receipt.followup_runs.len(), 1);
    assert_eq!(h.wire().await.len(), 20);
    let (outcomes, unowned) = h
        .router
        .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
        .await
        .unwrap();
    assert!(outcomes.is_empty() && unowned.is_empty());
    assert_eq!(h.count("command_journal"), 4);
    tokio::time::sleep(Duration::from_millis(5210)).await;
    let (_, unowned) = h
        .router
        .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
        .await
        .unwrap();
    assert!(unowned.is_empty());
    assert_eq!(h.wire().await, [0xae, 3, 1, 0xf1, 0xee]);
    assert_eq!(h.count("command_journal"), 5);
    artifact(
        "delayed-after-request-ttl.json",
        json!({"timed_source_offset_ms":5200,"control_request_timeout_ms":5000,"command_ttl_ms":2000,"actual_delayed_wire":[174,3,1,241,238],"timeline_retimed":false}),
    );
    h.finish().await;
}
fn native_inputs() -> (Vec<u8>, Json) {
    let mut config: Json = serde_json::from_slice(&data()).unwrap();
    config["devices"].as_array_mut().unwrap().push(json!({"id":"native-load","name":"Synthetic native load","module_id":"dim","channel":2,"enabled":true}));
    config["devices"].as_array_mut().unwrap().push(json!({"id":"hook","name":"Synthetic ordinary variable hook","module_id":"relay","channel":3,"enabled":true}));
    let mut r = rules();
    r["variables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"native.selection","kind":"integer","lifetime":"persistent"}));
    for (id, n, delay) in [
        ("native-a", 80, 50),
        ("native-b", 30, 0),
        ("native-off", 0, 0),
    ] {
        r["timed_scenes"].as_array_mut().unwrap().push(json!({"id":id,"enabled":true,"review_evidence":"synthetic reviewed native provider","max_lateness_ms":1000,"steps":[{"source":format!("native/{id}/output"),"offset_ms":delay,"device":"native-load","command":{"kind":"level","value":n,"ramp":false}}]}));
    }
    r["scene_switch_groups"] = json!([{"id":"native-group","variable":"native.selection","review_evidence":"synthetic existing provider","members":[{"scene":"native-a","value":1,"off_scene":"native-off"},{"scene":"native-b","value":2,"off_scene":"native-off"}],"reset_scenes":["native-off"]}]);
    (serde_json::to_vec(&config).unwrap(), r)
}
async fn set_value(h: &Harness, name: &str, value: i64) {
    let before = h
        .actor
        .variable_snapshot(h.member, h.generation.clone(), vec![name.into()])
        .await
        .unwrap();
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.member,
                intents: vec![],
                subscriptions: vec![],
            },
            vec![homesense_storage::variables::Update {
                revision: 1,
                name: name.into(),
                expected_version: before[name].version,
                value: Some(homesense_core::rule::Value::Integer(value)),
            }],
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn native_provider_on_off_nonselected_noop_and_reset_keep_original_selection_contract() {
    for (initial, target, on, next, expected_native) in [
        (None, "native-a", true, 1, Some(80)),
        (Some(1), "native-a", true, 1, None),
        (Some(2), "native-a", false, 2, None),
        (Some(1), "native-a", false, 0, Some(0)),
        (Some(2), "native-off", true, 0, Some(0)),
    ] {
        let (data, r) = native_inputs();
        let mut h = Harness::from_inputs(data, r).await;
        if let Some(value) = initial {
            set_value(&h, "native.selection", value).await;
        }
        h.samples(true);
        let before = h
            .actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into()],
            )
            .await
            .unwrap()["native.selection"]
            .clone();
        let reply = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: vec![("act".into(), Some(true)), (target.into(), Some(on))],
            })
            .await
            .unwrap();
        let Reply::ImmediateAccepted(receipt) = reply else {
            panic!("{reply:?}")
        };
        assert_eq!(h.wire().await.len(), 20);
        assert_eq!(
            receipt.followup_runs.len(),
            usize::from(expected_native.is_some())
        );
        let after = h
            .actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into()],
            )
            .await
            .unwrap()["native.selection"]
            .clone();
        assert_eq!(
            after.value,
            Some(homesense_core::rule::Value::Integer(next))
        );
        assert_eq!(
            after.version,
            before.version + u64::from(expected_native.is_some())
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        let (_, unowned) = h
            .router
            .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
            .await
            .unwrap();
        assert!(unowned.is_empty());
        let wire = h.wire().await;
        assert_eq!(
            wire,
            expected_native
                .map(|n| vec![0xae, 0x15, 2, n, 0xee])
                .unwrap_or_default()
        );
        artifact(
            &format!("native-provider-{target}-{on}-{initial:?}.json"),
            json!({"source_selection":initial,"next_selection":next,"before_version":before.version,"after_version":after.version,"receipt":receipt,"native_wire":wire}),
        );
        h.finish().await;
    }
}
#[tokio::test]
async fn native_conflict_unknown_off_and_invalid_later_item_commit_no_partial_immediate_state() {
    for tail in [
        vec![
            ("native-a".into(), Some(true)),
            ("native-b".into(), Some(true)),
        ],
        vec![("native-a".into(), Some(false))],
        vec![
            ("native-a".into(), Some(true)),
            ("absent".into(), Some(true)),
        ],
    ] {
        let (data, r) = native_inputs();
        let mut h = Harness::from_inputs(data, r).await;
        let mut writes = vec![("act".into(), Some(true))];
        writes.extend(tail);
        assert!(
            h.request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: writes
            })
            .await
            .is_err()
        );
        assert_eq!(h.count("command_journal"), 0);
        assert_eq!(h.count("scene_run"), 0);
        let values = h
            .actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["v".into(), "native.selection".into()],
            )
            .await
            .unwrap();
        assert!(values.values().all(|v| v.value.is_none() && v.version == 0));
        assert!(h.actor.task_generation().await.is_ok());
        h.finish().await;
    }
}
fn guard_payload(h: &Harness) -> crate::admission::DeferredRule {
    crate::admission::DeferredRule {
        batch: homesense_hap::requests::Batch {
            controller: h.member,
            intents: vec![homesense_hap::requests::Intent {
                device: homesense_core::DeviceId::new("light").unwrap(),
                command: homesense_core::Command::SetLevel {
                    level: homesense_core::Percent::new(20).unwrap(),
                    transition: homesense_core::LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        },
        variables: vec![homesense_storage::variables::Update {
            revision: 1,
            name: "v".into(),
            expected_version: 0,
            value: Some(homesense_core::rule::Value::Integer(7)),
        }],
        starts: vec![],
        audit: homesense_storage::rule_audit::Admission {
            revision: 1,
            correlation: Uuid::new_v4().as_u128(),
            event: 1,
            parent: 0,
            rule: "synthetic-native-read-proof".into(),
        },
    }
}
#[tokio::test]
async fn native_read_proof_and_condition_cas_reject_changed_or_forged_inputs_without_partial_commit()
 {
    for mode in [
        "selection-version",
        "selection-value",
        "condition-version",
        "valid-coalesced",
    ] {
        let (data, r) = native_inputs();
        let h = Harness::from_inputs(data, r).await;
        let mut reads = h
            .actor
            .scene_selection_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into()],
            )
            .await
            .unwrap();
        if mode == "selection-version" {
            set_value(&h, "native.selection", 2).await;
        }
        if mode == "selection-value" {
            reads.get_mut("native.selection").unwrap().stored.value =
                Some(homesense_core::rule::Value::Integer(9));
        }
        let condition = if mode == "condition-version" {
            set_value(&h, "native.selection", 2).await;
            0
        } else {
            reads["native.selection"].stored.version
        };
        let result = h
            .actor
            .submit_deferred_rule_for_entry(
                h.generation.clone(),
                guard_payload(&h),
                None,
                Some(vec!["synthetic/actual-output".into()]),
                crate::admission::RuleEntry {
                    deadline: Some(h.actor.monotonic_ms() + 1000),
                    selection_reads: if mode == "condition-version" {
                        None
                    } else {
                        Some(reads)
                    },
                    condition_reads: vec![homesense_storage::variables::ReadCheck {
                        revision: 1,
                        name: "native.selection".into(),
                        expected_version: condition,
                    }],
                    ..Default::default()
                },
            )
            .await;
        if mode == "valid-coalesced" {
            assert!(result.is_ok(), "{mode}: {:?}", result.as_ref().err());
            assert_eq!(h.count("command_journal"), 1);
            assert_eq!(h.wire().await, [0xae, 0x15, 1, 20, 0xee]);
        } else {
            assert!(result.is_err(), "{mode}: {:?}", result.as_ref().err());
            assert_eq!(h.count("command_journal"), 0);
            assert_eq!(h.count("rule_admission"), 0);
            assert!(
                h.actor
                    .variable_snapshot(h.member, h.generation.clone(), vec!["v".into()])
                    .await
                    .unwrap()["v"]
                    .value
                    .is_none()
            );
        }
        assert!(h.actor.task_generation().await.is_ok());
        h.finish().await;
    }
}
#[tokio::test]
async fn native_metadata_writes_do_not_add_hooks_but_explicit_immediate_assignments_keep_causal_hooks()
 {
    for ordinary in [false, true] {
        let (data, mut r) = native_inputs();
        r["rules"].as_array_mut().unwrap().push(json!({"id":"native-variable-hook","enabled":true,"source":"synthetic explicit ordinary hook","review_evidence":"synthetic","trigger":{"kind":"variable_changed","variable":"native.selection"},"actions":[command("native-variable-hook/output","hook",json!({"kind":"switch","on":true}))]}));
        if ordinary {
            r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().insert(0,json!({"source":"ordinary/native-variable-set","action":{"kind":"set","variable":"native.selection","value":{"kind":"integer","value":2}}}));
        }
        let mut h = Harness::from_inputs(data, r).await;
        h.samples(true);
        let result = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: vec![("act".into(), Some(true)), ("native-a".into(), Some(true))],
            })
            .await
            .unwrap();
        let Reply::ImmediateAccepted(receipt) = result else {
            panic!("{result:?}")
        };
        assert_eq!(receipt.commands.len(), 4 + usize::from(ordinary));
        let wire = h.wire().await;
        assert_eq!(wire.len(), 20 + 5 * usize::from(ordinary));
        assert_eq!(
            wire.as_chunks::<5>()
                .0
                .iter()
                .filter(|frame| frame[1..4] == [3, 3, 0xf1])
                .count(),
            usize::from(ordinary)
        );
        h.finish().await;
    }
}
#[tokio::test]
async fn joint_condition_only_persistent_and_temporary_reads_keep_versions_while_native_updates_commit()
 {
    let (data, mut r) = native_inputs();
    r["variables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"temporary-read","kind":"integer","lifetime":"temporary"}));
    r["variables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"persistent-read","kind":"integer","lifetime":"persistent"}));
    for id in ["temporary-read", "persistent-read"] {
        r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().insert(0,json!({"source":format!("condition-only/{id}"),"action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":id}},"then_actions":[],"else_actions":[]}}));
    }
    let mut h = Harness::from_inputs(data, r).await;
    h.samples(true);
    let names = vec!["temporary-read".into(), "persistent-read".into()];
    let before = h
        .actor
        .variable_snapshot(h.member, h.generation.clone(), names.clone())
        .await
        .unwrap();
    assert!(
        h.request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), Some(true)), ("native-a".into(), Some(true))]
        })
        .await
        .is_ok()
    );
    let after = h
        .actor
        .variable_snapshot(h.member, h.generation.clone(), names)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        h.actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into()]
            )
            .await
            .unwrap()["native.selection"]
            .value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    h.finish().await;
}
#[tokio::test]
async fn joint_native_noop_preserves_genuine_key_inference_and_new_key_invalidates_old_proof() {
    for changed in [false, true] {
        let (data, mut r) = native_inputs();
        let mut config: Json = serde_json::from_slice(&data).unwrap();
        config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"panel","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}));
        for channel in [1, 3] {
            config["devices"].as_array_mut().unwrap().push(json!({"id":format!("key{channel}"),"name":"Synthetic reviewed key","module_id":"panel","channel":channel,"enabled":true}));
        }
        r["scene_observation"] = json!({"enabled":true,"debounce_ms":30,"max_age_ms":5000,"external_toggle_reviews":[{"group":"native-group","review_evidence":"synthetic complete external writer inventory"}],"bindings":[{"device":"key1","group":"native-group","member":1,"review_evidence":"synthetic observed mapping"},{"device":"key3","group":"native-group","member":2,"review_evidence":"synthetic observed mapping"}]});
        let mut h = Harness::from_inputs(serde_json::to_vec(&config).unwrap(), r).await;
        let mut consumer = crate::scene_observation_runtime::Consumer::default();
        let sample = |h: &mut Harness, key: u8, down: bool| {
            h.feedback
                .apply(&GatewayRead {
                    gateway_id: "bus".into(),
                    transport_epoch: 1,
                    received_at_ms: h.actor.monotonic_ms(),
                    result: hb_transport::Receive::Frames {
                        transport_epoch: 1,
                        frames: vec![hb_protocol::RawFrame {
                            address: 0x41,
                            command: 0xd0 + key,
                            value: if down { 0xf1 } else { 0xf2 },
                        }],
                    },
                })
                .unwrap();
        };
        for key in [1, 3] {
            sample(&mut h, key, false);
        }
        consumer
            .tick(&h.actor, h.controller, &h.document, &h.feedback)
            .await
            .unwrap();
        for key in [3, 1] {
            sample(&mut h, key, false);
            sample(&mut h, key, true);
            consumer
                .tick(&h.actor, h.controller, &h.document, &h.feedback)
                .await
                .unwrap();
        }
        h.samples(true);
        let before = h
            .actor
            .scene_selection_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into()],
            )
            .await
            .unwrap();
        assert_eq!(
            before["native.selection"].stored.value,
            Some(homesense_core::rule::Value::Integer(1))
        );
        assert!(
            before["native.selection"].evidence.is_some()
                && before["native.selection"].evidence_current
        );
        assert!(
            h.actor
                .variable_snapshot(
                    h.member,
                    h.generation.clone(),
                    vec!["native.selection".into()]
                )
                .await
                .unwrap()["native.selection"]
                .value
                .is_none()
        );
        if changed {
            sample(&mut h, 3, false);
            sample(&mut h, 3, true);
            consumer
                .tick(&h.actor, h.controller, &h.document, &h.feedback)
                .await
                .unwrap();
            let result = h
                .actor
                .submit_deferred_rule_for_entry(
                    h.generation.clone(),
                    guard_payload(&h),
                    None,
                    Some(vec!["synthetic/source".into()]),
                    crate::admission::RuleEntry {
                        selection_reads: Some(before),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_err());
            assert_eq!(h.count("command_journal"), 0);
            assert!(
                h.actor
                    .variable_snapshot(h.member, h.generation.clone(), vec!["v".into()])
                    .await
                    .unwrap()["v"]
                    .value
                    .is_none()
            );
        } else {
            let result = h
                .request(Action::SceneWrites {
                    controller: h.member,
                    revision: 1,
                    scenes: vec![("act".into(), Some(true)), ("native-a".into(), Some(true))],
                })
                .await
                .unwrap();
            let Reply::ImmediateAccepted(receipt) = result else {
                panic!("{result:?}")
            };
            assert!(receipt.followup_runs.is_empty());
            h.wire().await;
            let after = h
                .actor
                .scene_selection_snapshot(
                    h.member,
                    h.generation.clone(),
                    vec!["native.selection".into()],
                )
                .await
                .unwrap();
            let old = &before["native.selection"];
            let new = &after["native.selection"];
            assert_eq!(new.stored, old.stored);
            assert_eq!(new.evidence, old.evidence);
            assert!(new.evidence_current);
            artifact(
                "joint-genuine-inference-noop.json",
                json!({"before_version":old.stored.version,"after_version":new.stored.version,"inferred_value":1,"inference_kept":true,"new_native_runs":0,"actual_key_sequence":[3,1],"immediate_commands":receipt.commands}),
            );
        }
        h.finish().await;
    }
}
#[tokio::test]
async fn native_effective_off_stays_after_immediate_when_earlier_group_address_is_passive_or_nonselected()
 {
    for passive in [None, Some(false)] {
        let (data, mut r) = native_inputs();
        r["variables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"seen-selection","kind":"integer","lifetime":"persistent"}));
        r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().insert(0,json!({"source":"ordinary/read-selection","action":{"kind":"set","variable":"seen-selection","value":{"kind":"variable","id":"native.selection"}}}));
        let mut h = Harness::from_inputs(data, r).await;
        set_value(&h, "native.selection", 2).await;
        h.samples(true);
        let result = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: vec![
                    ("native-a".into(), passive),
                    ("act".into(), Some(true)),
                    ("native-b".into(), Some(false)),
                ],
            })
            .await
            .unwrap();
        assert!(matches!(result, Reply::ImmediateAccepted(_)));
        let values = h
            .actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["seen-selection".into(), "native.selection".into()],
            )
            .await
            .unwrap();
        assert_eq!(
            values["seen-selection"].value,
            Some(homesense_core::rule::Value::Integer(2))
        );
        assert_eq!(
            values["native.selection"].value,
            Some(homesense_core::rule::Value::Integer(0))
        );
        h.wire().await;
        let (_, unowned) = h
            .router
            .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
            .await
            .unwrap();
        assert!(unowned.is_empty());
        assert_eq!(h.wire().await, [0xae, 0x15, 2, 0, 0xee]);
        h.finish().await;
    }
}
#[tokio::test]
async fn joint_capacity_rejection_and_caller_revoke_before_acceptance_leave_other_controls_healthy()
{
    let (data, r) = native_inputs();
    let mut h = Harness::from_inputs(data, r).await;
    h.samples(true);
    let held = h.scheduler.reserve_slots(16).unwrap();
    assert!(
        h.request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), Some(true)), ("native-a".into(), Some(true))]
        })
        .await
        .is_err()
    );
    assert_eq!(h.count("command_journal"), 0);
    assert!(
        h.actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap()
            .ready()
    );
    assert!(
        h.actor
            .variable_snapshot(
                h.controller,
                h.generation.clone(),
                vec!["v".into(), "native.selection".into()]
            )
            .await
            .unwrap()
            .values()
            .all(|s| s.value.is_none() && s.version == 0)
    );
    drop(held);
    assert!(
        h.request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("act".into(), Some(true)), ("native-a".into(), Some(true))]
        })
        .await
        .is_ok()
    );
    h.finish().await;
    let (data, r) = native_inputs();
    let mut h = Harness::from_inputs(data, r).await;
    let mut session = crate::deferred_session::Session::new_for_caller_before(
        h.actor.clone(),
        h.member,
        &h.scheduler,
        Default::default(),
        1000,
        Some(h.actor.monotonic_ms() + 1000),
    )
    .await
    .unwrap();
    h.allowed.store(false, Ordering::Release);
    assert!(
        session
            .start_manual_scenes(
                &[("act".into(), Some(true)), ("native-a".into(), Some(true))],
                &mut h.scheduler,
                Some((&h.feedback, 5000))
            )
            .await
            .is_err()
    );
    assert!(session.failure_isolated());
    assert_eq!(h.count("command_journal"), 0);
    assert!(
        h.actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap()
            .ready()
    );
    assert!(
        h.actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap()["v"]
            .value
            .is_none()
    );
    h.finish().await;
}
#[tokio::test]
async fn shared_native_shutdown_runs_once_and_updates_all_original_groups_with_both_call_sources() {
    for reset in [false, true] {
        let (data, mut r) = native_inputs();
        r["variables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"second.selection","kind":"integer","lifetime":"persistent"}));
        for (id, n) in [("second-a", 50), ("second-b", 60)] {
            r["timed_scenes"].as_array_mut().unwrap().push(json!({"id":id,"enabled":true,"review_evidence":"synthetic second group","max_lateness_ms":1000,"steps":[{"source":format!("{id}/output"),"offset_ms":0,"device":"native-load","command":{"kind":"level","value":n,"ramp":false}}]}));
        }
        r["scene_switch_groups"].as_array_mut().unwrap().push(json!({"id":"second-group","variable":"second.selection","review_evidence":"synthetic shared shutdown provider","members":[{"scene":"second-a","value":1,"off_scene":"native-off"},{"scene":"second-b","value":2,"off_scene":"native-off"}],"reset_scenes":["native-off"]}));
        let mut h = Harness::from_inputs(data, r).await;
        set_value(&h, "native.selection", 1).await;
        set_value(&h, "second.selection", 2).await;
        h.samples(true);
        let mut writes = vec![("act".into(), Some(true))];
        if reset {
            writes.push(("native-off".into(), Some(true)))
        } else {
            writes.extend([
                ("native-a".into(), Some(false)),
                ("second-b".into(), Some(false)),
            ]);
        }
        let result = h
            .request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: writes,
            })
            .await
            .unwrap();
        let Reply::ImmediateAccepted(receipt) = result else {
            panic!("{result:?}")
        };
        assert_eq!(receipt.followup_runs.len(), 1);
        assert_eq!(h.count("scene_run"), 1);
        let values = h
            .actor
            .variable_snapshot(
                h.member,
                h.generation.clone(),
                vec!["native.selection".into(), "second.selection".into()],
            )
            .await
            .unwrap();
        assert!(
            values
                .values()
                .all(|s| s.value == Some(homesense_core::rule::Value::Integer(0)))
        );
        assert_eq!(h.wire().await.len(), 20);
        let (_, unowned) = h
            .router
            .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
            .await
            .unwrap();
        assert!(unowned.is_empty());
        assert_eq!(h.wire().await, [0xae, 0x15, 2, 0, 0xee]);
        let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        let source: String = sql
            .query_row("SELECT source FROM scene_cause", [], |r| r.get(0))
            .unwrap();
        assert!(
            source.contains("native-group/") && source.contains("second-group/"),
            "{source}"
        );
        drop(sql);
        artifact(
            &format!("native-shared-reset-{reset}.json"),
            json!({"native_runs":1,"group_selections":[0,0],"source":source,"native_wire":[174,21,2,0,238]}),
        );
        h.finish().await;
    }
}
#[tokio::test]
async fn native_transfer_with_old_member_off_uses_positive_member_without_intermediate_shutdown() {
    let (data, mut r) = native_inputs();
    r["variables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"seen-selection","kind":"integer","lifetime":"persistent"}));
    r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().insert(0,json!({"source":"ordinary/seen-before-native-on","action":{"kind":"set","variable":"seen-selection","value":{"kind":"variable","id":"native.selection"}}}));
    let mut h = Harness::from_inputs(data, r).await;
    set_value(&h, "native.selection", 1).await;
    h.samples(true);
    let result = h
        .request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![
                ("native-a".into(), Some(false)),
                ("act".into(), Some(true)),
                ("native-b".into(), Some(true)),
            ],
        })
        .await
        .unwrap();
    let Reply::ImmediateAccepted(receipt) = result else {
        panic!("{result:?}")
    };
    assert_eq!(receipt.followup_runs.len(), 1);
    let values = h
        .actor
        .variable_snapshot(
            h.member,
            h.generation.clone(),
            vec!["native.selection".into(), "seen-selection".into()],
        )
        .await
        .unwrap();
    assert_eq!(
        values["seen-selection"].value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    assert_eq!(
        values["native.selection"].value,
        Some(homesense_core::rule::Value::Integer(2))
    );
    assert_eq!(h.wire().await.len(), 20);
    let (_, unowned) = h
        .router
        .poll(&mut h.scheduler, Some((&h.feedback, 5000)))
        .await
        .unwrap();
    assert!(unowned.is_empty());
    assert_eq!(h.wire().await, [0xae, 0x15, 2, 30, 0xee]);
    let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT definition FROM scene_run", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "native-b"
    );
    drop(sql);
    h.finish().await;
}
fn quiescent_deactivation_rules() -> Json {
    let mut d = rules();
    d["scene_deactivations"] = json!([{"scene":"act","target":"later","enabled":true,"review_evidence":"synthetic explicit global control-work scope","active_execution_policy":"require_quiescent_runtime"}]);
    d["rules"] = json!([{"id":"off-rule","enabled":true,"review_evidence":"synthetic reviewed off call","source":"synthetic/source","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"pre-set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":9}}},{"source":"qualified/off","action":{"kind":"scene","id":"act","activate":false}}]}]);
    d
}
async fn execute_quiescent_off(
    h: &mut Harness,
) -> Result<crate::deferred_rule::Outcome, crate::deferred_rule::ExecutionFailure> {
    let mut chain = homesense_core::rule_chain::Chain::new(
        Uuid::new_v4().as_u128(),
        h.actor.monotonic_ms(),
        60000,
        homesense_core::rule::Budget::default(),
        32,
    )
    .unwrap();
    let root = chain.root();
    crate::deferred_rule::execute_classified(
        &h.actor,
        h.controller,
        "off-rule",
        &mut chain,
        &root,
        &mut h.scheduler,
        None,
    )
    .await
}
#[tokio::test]
async fn deactivation_excludes_background_query_tickets_queued_and_writing() {
    for writing in [false, true] {
        let mut h = Harness::new(quiescent_deactivation_rules()).await;
        h.actor
            .module_query(
                h.controller,
                h.generation.clone(),
                "relay".into(),
                crate::module_queries::Scope::WholeModule,
                true,
            )
            .await
            .unwrap();
        let ticket = if writing {
            Some(h.actor.dispatch().await.unwrap().unwrap())
        } else {
            None
        };
        let result = execute_quiescent_off(&mut h).await.unwrap();
        assert_eq!(result.scene_runs.len(), 1);
        assert!(result.command_ids.is_empty());
        if let Some(ticket) = ticket {
            assert_eq!(ticket.command.command, homesense_core::Command::Query);
            h.actor.finish_write(ticket.command.id, true).await.unwrap();
        }
        let ready = h
            .actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap();
        assert!(!ready.failed);
        h.finish().await;
    }
}
#[tokio::test]
async fn deactivation_rejects_queued_and_writing_control_without_new_state_and_other_work_remains_healthy()
 {
    for writing in [false, true] {
        let mut h = Harness::new(quiescent_deactivation_rules()).await;
        h.actor
            .submit(homesense_hap::requests::Batch {
                controller: h.member,
                intents: vec![homesense_hap::requests::Intent {
                    device: homesense_core::DeviceId::new("marker").unwrap(),
                    command: homesense_core::Command::Switch(true),
                }],
                subscriptions: vec![],
            })
            .await
            .unwrap();
        let ticket = if writing {
            Some(h.actor.dispatch().await.unwrap().unwrap())
        } else {
            None
        };
        let before = h
            .actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap();
        assert!(matches!(
            execute_quiescent_off(&mut h).await,
            Err(crate::deferred_rule::ExecutionFailure::BusyBeforeAcceptance)
        ));
        let after = h
            .actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap();
        assert_eq!(before, after);
        let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        assert_eq!(
            sql.query_row("select count(*) from scene_run", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            sql.query_row("select count(*) from command_journal", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(sql);
        let ticket = if let Some(ticket) = ticket {
            ticket
        } else {
            h.actor.dispatch().await.unwrap().unwrap()
        };
        h.actor.finish_write(ticket.command.id, true).await.unwrap();
        assert!(
            !h.actor
                .diagnostics(h.controller, h.generation.clone())
                .await
                .unwrap()
                .failed
        );
        assert!(execute_quiescent_off(&mut h).await.is_ok());
        h.finish().await;
    }
}
#[tokio::test]
async fn deactivation_rejects_uncertain_receipt_and_expired_unsettled_source_with_no_partial_effects()
 {
    let mut h = Harness::new(quiescent_deactivation_rules()).await;
    let ids = h
        .actor
        .submit(homesense_hap::requests::Batch {
            controller: h.member,
            intents: vec![homesense_hap::requests::Intent {
                device: homesense_core::DeviceId::new("marker").unwrap(),
                command: homesense_core::Command::Switch(true),
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    h.actor.finish_write(ticket.command.id, true).await.unwrap();
    // Inject the durable evidence state, without fabricating queue or actor failure.
    let mut store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
    store
        .transition_command(
            h.actor.boot_id(),
            ids[0],
            homesense_storage::journal::Status::Written,
            homesense_storage::journal::Status::Uncertain,
            h.actor.monotonic_ms(),
        )
        .unwrap();
    drop(store);
    let before = h
        .actor
        .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
        .await
        .unwrap();
    assert!(matches!(
        execute_quiescent_off(&mut h).await,
        Err(crate::deferred_rule::ExecutionFailure::BusyBeforeAcceptance)
    ));
    assert_eq!(
        before,
        h.actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap()
    );
    assert_eq!(h.count("scene_run"), 0);
    assert_eq!(h.count("command_journal"), 1);
    assert!(
        !h.actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap()
            .failed
    );
    h.finish().await;
    let mut d = quiescent_deactivation_rules();
    d["timed_scenes"].as_array_mut().unwrap().push(json!({"id":"source-timed","enabled":true,"review_evidence":"synthetic tiny expiry","max_lateness_ms":1,"steps":[{"source":"source-only","offset_ms":0,"device":"marker","command":{"kind":"switch","on":true}}]}));
    d["scene_deactivations"][0]["scene"] = "source-timed".into();
    d["rules"][0]["actions"][1]["action"]["id"] = "source-timed".into();
    let mut h = Harness::new(d).await;
    let source = h
        .scheduler
        .start_document(&h.document, "source-timed", h.actor.monotonic_ms())
        .await
        .unwrap();
    let started = h.actor.monotonic_ms();
    tokio::time::sleep(Duration::from_millis(3)).await;
    assert!(h.actor.monotonic_ms() > started + 1);
    let before = h
        .actor
        .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
        .await
        .unwrap();
    assert!(matches!(
        execute_quiescent_off(&mut h).await,
        Err(crate::deferred_rule::ExecutionFailure::BusyBeforeAcceptance)
    ));
    assert_eq!(
        before,
        h.actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap()
    );
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(
        h.scheduler.status(source),
        Some(homesense_core::scene::Status::Pending)
    );
    h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
    assert!(h.actor.dispatch().await.unwrap().is_none());
    assert!(
        !h.actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap()
            .failed
    );
    h.finish().await;
}
#[tokio::test]
async fn deactivation_blocks_same_admission_source_activation_and_charges_future_fade_budget() {
    let mut d = quiescent_deactivation_rules();
    let on = json!({"source":"same-batch/source-on","action":{"kind":"scene","id":"act","activate":true}});
    d["rules"][0]["actions"]
        .as_array_mut()
        .unwrap()
        .insert(1, on);
    let mut h = Harness::new(d).await;
    let before = h
        .actor
        .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
        .await
        .unwrap();
    assert!(matches!(
        execute_quiescent_off(&mut h).await,
        Err(crate::deferred_rule::ExecutionFailure::Planning(_))
    ));
    assert_eq!(
        before,
        h.actor
            .variable_snapshot(h.controller, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap()
    );
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(h.count("scene_run"), 0);
    h.finish().await;
    let mut d = quiescent_deactivation_rules();
    d["timed_scenes"][0]["steps"][0]["device"] = "light".into();
    d["timed_scenes"][0]["steps"][0]["command"] = level(30);
    d["timed_scenes"][0]["steps"][0]["fade"] =
        json!({"duration_ms":1000,"interval_ms":100,"lateness_ms":200});
    let mut h = Harness::new(d).await;
    let budget = homesense_core::rule::Budget {
        commands: 4,
        ..Default::default()
    };
    let mut chain = homesense_core::rule_chain::Chain::new(
        Uuid::new_v4().as_u128(),
        h.actor.monotonic_ms(),
        60000,
        budget,
        32,
    )
    .unwrap();
    let root = chain.root();
    assert!(matches!(
        crate::deferred_rule::execute_classified(
            &h.actor,
            h.controller,
            "off-rule",
            &mut chain,
            &root,
            &mut h.scheduler,
            None
        )
        .await,
        Err(crate::deferred_rule::ExecutionFailure::Planning(
            homesense_core::rule_chain::Error::Budget
        ))
    ));
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(h.count("scene_run"), 0);
    h.finish().await;
}
