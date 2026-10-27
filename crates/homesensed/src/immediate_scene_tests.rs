//! Synthetic Immediate -> real Control/Admission/causal owner -> memory wire.
#[path = "current_read_model_tests.rs"]
mod current_read_model_tests;
#[path = "scene_retirement_tests.rs"]
mod scene_retirement_tests;
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
type AuthorizationProbe=Arc<dyn Fn(Uuid)+Send+Sync>;
impl Harness {
    async fn new(rules: Json) -> Self {
        Self::from_inputs(data(), rules).await
    }
    async fn from_inputs(data: Vec<u8>, rules: Json) -> Self {
        Self::from_inputs_with_probe(data,rules,None).await
    }
    async fn from_inputs_with_probe(data:Vec<u8>,rules:Json,probe:Option<AuthorizationProbe>)->Self {
        Self::from_inputs_mode(data,rules,probe,crate::admission::SceneWriteMode::Sequential).await
    }
    async fn from_inputs_mode(data:Vec<u8>,rules:Json,probe:Option<AuthorizationProbe>,scene_write_mode:crate::admission::SceneWriteMode)->Self {
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
                scene_write_mode,
                ..Default::default()
            },
            move |id| {
                if let Some(probe)=&probe {probe(id);}
                id == controller || (id == member && permit.load(Ordering::Acquire))
            },
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
        let bytes=Arc::new(Mutex::new(vec![]));
        let mut owner=TransportOwner::new(self.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(EchoMemory{bytes:bytes.clone(),rx:Default::default()},1))].into());
        // Management proof tests deliberately keep their main model delayed.
        // Only staged multi-module recipes receive synthetic wire observations.
        let mut observed=Feedback::new(&self.configuration,ExecutionContext::Simulation,
            Uuid::parse_str(self.actor.boot_id()).unwrap().as_u128(),1).unwrap();
        for _ in 0..128 {
            let Some(outcome)=owner.step().await.unwrap() else {break;};
            outcome.result.unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
            let reply=owner.receive("bus").await.unwrap();
            if self.actor.module_confirmation_active() {
                observed.apply(&reply).unwrap();
                for _ in 0..2 {observed.reconcile_control_phases(&self.actor,self.controller).await.unwrap();}
            }
        }
        bytes.lock().unwrap().clone()
    }
    async fn finish(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
struct Memory(Arc<Mutex<Vec<u8>>>);
struct EchoMemory {bytes:Arc<Mutex<Vec<u8>>>,rx:std::collections::VecDeque<u8>}
impl std::io::Read for EchoMemory {
    fn read(&mut self,out:&mut[u8])->std::io::Result<usize> {
        if self.rx.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into());}
        let n=out.len().min(self.rx.len());for byte in &mut out[..n]{*byte=self.rx.pop_front().unwrap();}Ok(n)
    }
}
impl std::io::Write for EchoMemory {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        for frame in bytes.as_chunks::<5>().0 {
            if frame[0]==0xae && frame[4]==0xee && (1..=8).contains(&frame[2]) {
                self.rx.extend([0xae,frame[1],0xd0+frame[2],frame[3],0xee]);
            }
        }Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
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
        hap_placement:Default::default(),hap_bridges: Default::default(),
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
                hb_transport::Connection::for_simulation(EchoMemory{bytes:bytes.clone(),rx:Default::default()}, 1),
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
fn execute_inputs()->(Vec<u8>,Json) {
    let mut cfg:Json=serde_json::from_slice(&data()).unwrap();cfg["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
    cfg["keypads"]=json!([{"id":"panel","name":"Synthetic manual panel","module_id":"panel-module","enabled":true,"keys":[{"number":1,"label":"Execute","led_endpoint":{"id":"execute-led","name":"LED","enabled":true},"action":{"kind":"scene_execute","scene":"act","review_evidence":"synthetic manual-only AST"}}]}]);
    let mut doc=rules();doc["rules"]=json!([]);(serde_json::to_vec(&cfg).unwrap(),doc)
}
#[test]
fn scene_execute_accepts_only_reviewed_enabled_manual_ast_and_preserves_toggle_requirement(){
    let (cfg,doc)=execute_inputs();let config=Configuration::parse(&cfg,ExecutionContext::Simulation).unwrap();let parsed=Document::parse(&serde_json::to_vec(&doc).unwrap(),&config).unwrap();let ids=homesense_config::keypad::scene_execute_devices(&parsed,&config,"act").unwrap();assert_eq!(ids.into_iter().map(|d|d.as_str().to_owned()).collect::<std::collections::BTreeSet<_>>(),["light".into(),"relay".into()].into());
    for mode in 0..6 {let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();let mut doc=doc.clone();match mode {
        0=>doc["immediate_scenes"][0]["enabled"]=json!(false),
        1=>cfg["keypads"][0]["keys"][0]["action"]["review_evidence"]=json!(""),
        2=>cfg["keypads"][0]["keys"][0]["action"]["scene"]=json!("later"),
        3=>cfg["keypads"][0]["keys"][0]["action"]["unexpected"]=json!(true),
        4=>cfg["keypads"][0]["keys"][0]["action"]["kind"]=json!("scene_toggle"),
        _=>cfg["keypads"][0]["keys"][0]["action"]["scene"]=json!("missing"),
    }let rejected=Configuration::parse(&serde_json::to_vec(&cfg).unwrap(),ExecutionContext::Simulation).and_then(|cfg|Document::parse(&serde_json::to_vec(&doc).unwrap(),&cfg));assert!(rejected.is_err(),"mode{mode} must be rejected");}
}
#[tokio::test]
async fn scene_execute_descriptor_full_branch_scope_atomic_manual_owner_and_revocation(){
    use crate::{web_auth::{Tokens,Grant,Role},source_auth::Source};
    let (cfg,doc)=execute_inputs();let mut h=Harness::from_inputs(cfg,doc).await;h.samples(true);
    let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();const SECRET:&str="synthetic-scene-execute-source-0123456789";
    let action=||Action::SceneAction{controller:h.member,revision:1,scene:"act".into(),execute:true};
    let grant=|devices:&[&str]|Grant{controller:h.member,role:Role::Member,scenes:["act".into()].into(),devices:devices.iter().map(|id|(*id).into()).collect()};
    tokens.insert(credential,SECRET,grant(&["relay","execute-led"])).unwrap();
    let snapshot=crate::management_devices::snapshot(&h.actor,h.controller,h.generation.clone(),vec![homesense_core::DeviceId::new("execute-led").unwrap()],&h.feedback).await.unwrap();
    let limited=crate::management_keypads::devices_json(&snapshot,&grant(&["relay","execute-led"]));assert!(limited.last().unwrap()["keys"][0]["action"].is_null(),"inactive nested branch target still needs scope");
    let complete=crate::management_keypads::devices_json(&snapshot,&grant(&["relay","light","execute-led"]));let key=&complete.last().unwrap()["keys"][0];assert_eq!(key["action"],json!({"kind":"scene_execute","scene":"act","path":"/api/v1/scene-actions"}));assert!(key["card_action"].is_null());
    let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action(),Some("act")).unwrap();let control=h.control.clone();let requested=action();let task=tokio::spawn(async move{control.request_authorized(requested,source).await});assert!(h.drive(task).await.unwrap_err().contains("full-branch"));assert_eq!(h.count("command_journal"),0);
    tokens.insert(credential,SECRET,Grant{controller:h.member,role:Role::Member,scenes:["act".into()].into(),devices:["relay".into(),"light".into(),"execute-led".into()].into()}).unwrap();
    let action=Action::SceneAction{controller:h.member,revision:1,scene:"act".into(),execute:true};let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action,Some("act")).unwrap();let control=h.control.clone();let task=tokio::spawn(async move{control.request_authorized(action,source).await});let reply=h.drive(task).await.unwrap();assert!(matches!(reply,Reply::ImmediateAccepted(_)));assert_eq!(h.count("command_journal"),3);let frames=h.wire().await;assert_eq!(frames,vec![0xae,0x15,1,20,0xee,0xae,0x15,1,40,0xee,0xae,3,1,0xf2,0xee]);
    tokio::time::sleep(Duration::from_millis(2)).await;h.samples(false);
    let action=Action::SceneAction{controller:h.member,revision:1,scene:"act".into(),execute:true};let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action,Some("act")).unwrap();let control=h.control.clone();let task=tokio::spawn(async move{control.request_authorized(action,source).await});h.drive(task).await.unwrap();tokens.revoke(credential).unwrap();assert!(h.wire().await.is_empty(),"revoked exact source cannot write accepted second activation");h.finish().await;
}

fn component_inputs()->(Vec<u8>,Json){
    let (cfg,mut doc)=execute_inputs();doc["variables"]=json!([{"id":"v","kind":"integer","lifetime":"persistent"},{"id":"w","kind":"integer","lifetime":"persistent"}]);
    let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();cfg["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({"number":2,"label":"Other","led_endpoint":{"id":"other-led","name":"Other LED","enabled":true},"action":{"kind":"scene_execute","scene":"other","review_evidence":"synthetic independent mode"}}));
    let activate=|var:&str,device:&str,led:&str|json!([{"source":format!("{var}/first"),"action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":var}},"then_actions":[command(&format!("{var}/on"),device,json!({"kind":"switch","on":true})),{"source":format!("{var}/set"),"action":{"kind":"set","variable":var,"value":{"kind":"integer","value":1}}},command(&format!("{var}/led"),led,json!({"kind":"panel_led","on":true}))],"else_actions":[command(&format!("{var}/off"),device,json!({"kind":"switch","on":false})),{"source":format!("{var}/reset"),"action":{"kind":"set","variable":var,"value":{"kind":"integer","value":0}}},command(&format!("{var}/green"),led,json!({"kind":"panel_led","on":false}))]}}]);
    doc["immediate_scenes"]=json!([{"id":"act","enabled":true,"review_evidence":"synthetic mode A","activate":activate("v","relay","execute-led")},{"id":"other","enabled":true,"review_evidence":"synthetic mode B","activate":activate("w","marker","other-led")}]);doc["timed_scenes"]=json!([]);
    doc["component_mode_confirmations"]=json!([{"id":"mode-a","variable":"v","scenes":["act"],"first_value":1,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic first-unknown policy","postconditions":[{"device":"relay","on":true}]},{"id":"mode-b","variable":"w","scenes":["other"],"first_value":1,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic independent policy","postconditions":[{"device":"marker","on":true}]}]);
    (serde_json::to_vec(&cfg).unwrap(),doc)
}
fn mode_value(h:&Harness,name:&str)->Option<homesense_core::rule::Value>{Store::open(h.directory.join("runtime.sqlite")).unwrap().logical_variable(1,name).unwrap().unwrap().value}
fn mode_frame(h:&mut Harness,addr:u8,channel:u8,value:u8){h.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:h.actor.monotonic_ms(),result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:addr,command:0xd0+channel,value}]}}).unwrap();}
fn component_cycle_inputs() -> (Vec<u8>, Json) {
    let (cfg, mut doc) = component_inputs();
    let dispatch = &mut doc["immediate_scenes"][0]["activate"][0]["action"];
    let on = dispatch["then_actions"].clone();
    let off = dispatch["else_actions"].clone();
    dispatch["else_actions"] = json!([{
        "source": "known/toggle",
        "action": {
            "kind": "if",
            "condition": {"kind":"equal", "left":{"kind":"variable", "id":"v"},
                          "right":{"kind":"integer", "value":1}},
            "then_actions": off, "else_actions": on
        }
    }]);
    let mut off = doc["component_mode_confirmations"][0].clone();
    off["id"] = json!("known-off-only");
    off["first_value"] = json!(0);
    off["confirm_when_known"] = json!(true);
    off["postconditions"][0]["on"] = json!(false);
    doc["component_mode_confirmations"].as_array_mut().unwrap().push(off);
    (cfg, doc)
}
#[tokio::test]
async fn component_mode_known_off_policy_does_not_reject_third_press_on() {
    let (cfg, doc) = component_cycle_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    for on in [true, false, true, false] {
        h.request(Action::SceneAction {
            controller: h.controller, revision: 1, scene: "act".into(), execute: true,
        }).await.expect("an inactive opposite-result policy must not reject this toggle");
        assert_eq!(h.wire().await, vec![0xae, 3, 1, if on { 0xf1 } else { 0xf2 }, 0xee]);
        tokio::time::sleep(Duration::from_millis(2)).await;
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        mode_confirm(&h).await;
        assert_eq!(mode_value(&h, "v"), Some(homesense_core::rule::Value::Integer(i64::from(on))));
    }
    assert_eq!(h.count("command_journal"), 8, "exactly one light and one LED per press");
    h.finish().await;
}
#[tokio::test]
async fn component_mode_known_peer_with_unknown_whole_source_keeps_partial_cycle_available() {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["variables"].as_array_mut().unwrap().extend([
        json!({"id":"whole", "kind":"integer", "lifetime":"persistent"}),
        json!({"id":"peer", "kind":"bool", "lifetime":"persistent"}),
    ]);
    doc["immediate_scenes"][0]["activate"].as_array_mut().unwrap().extend([
        command("partial/peer-off", "marker", json!({"kind":"switch", "on":false})),
        json!({"source":"partial/peer-set", "action":{"kind":"set", "variable":"peer",
            "value":{"kind":"bool", "value":false}}}),
    ]);
    doc["component_mode_confirmations"].as_array_mut().unwrap().extend([
        json!({"id":"unknown-peer-off", "variable":"peer", "scenes":["act"],
            "first_value":false, "include_when_first_unknown":true, "timeout_ms":15000,
            "max_age_ms":5000, "review_evidence":"Synthetic partial first enable clears its peer",
            "postconditions":[{"device":"marker", "on":false}]}),
        json!({"id":"whole-zero-peer", "variable":"peer", "scenes":["act"],
            "first_value":false, "basis":"logical_source", "confirm_when_known":true,
            "when_values":{"whole":0}, "timeout_ms":15000, "max_age_ms":5000,
            "review_evidence":"Synthetic known whole-source canonical peer label", "postconditions":[]}),
    ]);
    let mut h = Harness::from_inputs(cfg, doc).await;
    for on in [true, false, true, false] {
        h.request(Action::SceneAction {
            controller: h.controller, revision: 1, scene: "act".into(), execute: true,
        }).await.expect("a dormant whole-source label cannot claim a partial transition");
        assert_eq!(h.wire().await,
            vec![0xae, 3, 1, if on { 0xf1 } else { 0xf2 }, 0xee,
                 0xae, 3, 2, 0xf2, 0xee]);
        tokio::time::sleep(Duration::from_millis(2)).await;
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        mode_frame(&mut h, 3, 2, 0xf2);
        mode_confirm(&h).await;
        assert_eq!(mode_value(&h, "v"), Some(homesense_core::rule::Value::Integer(i64::from(on))));
        assert_eq!(mode_value(&h, "peer"), Some(homesense_core::rule::Value::Bool(false)));
        assert_eq!(mode_value(&h, "whole"), None, "partial controls never invent the whole-source mode");
    }
    assert_eq!(h.count("command_journal"), 12);
    h.finish().await;
}
async fn mode_confirm(h:&Harness)->usize{
    let has_written_stage=!h.actor.control_batch_views(h.controller,h.generation.clone()).await.unwrap().is_empty();
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();if has_written_stage{h.wire().await;}

    let views=h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap();let mut confirmed=0;
    for view in views.into_iter().rev(){let proof=crate::component_modes::Proof::prepare(&view,&h.configuration,&h.feedback,h.actor.monotonic_ms()).unwrap();if h.actor.component_mode_proof(h.controller,proof).await.unwrap(){confirmed+=1;}}
    confirmed
}
#[tokio::test]
async fn component_mode_independent_reservations_actual_written_fresh_vector_and_restart_unknown(){
    let (cfg,doc)=component_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),None);assert_eq!(h.count("command_journal"),2);
    assert!(h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().is_empty());
    assert!(h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.is_err());assert_eq!(h.count("command_journal"),2);
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"other".into(),execute:true}).await.unwrap();assert_eq!(h.count("command_journal"),4);assert_eq!(h.wire().await.len(),10,"only two physical light outputs precede the feedback barrier");
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);
    assert_eq!(mode_confirm(&h).await,2,"B confirms before A without unrelated persistent version invalidation");assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(1)));
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,2,0xf2);mode_confirm(&h).await;assert_eq!(mode_value(&h,"w"),None);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(h.count("command_journal"),4);
    h.actor.clone().shutdown().await.unwrap();let controller=h.controller;let actor=Admission::open(h.directory.join("runtime.sqlite"),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();
    assert_eq!(mode_value(&h,"v"),None,"durable inference audit must not restore current mode after restart");actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();assert!(actor.component_mode_views(controller,generation).await.unwrap().is_empty(),"same physical On cannot create a new owned confirmation");actor.shutdown().await.unwrap();std::fs::remove_dir_all(h.directory).unwrap();
}
#[tokio::test]
async fn component_mode_wrong_level_cannot_confirm_then_actual_level_confirms_and_off_invalidates(){
    let (cfg,mut doc)=component_inputs();let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();cfg["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");
    let actions=doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"].as_array_mut().unwrap();actions[0]=command("v/dim20","light",level(20));doc["component_mode_confirmations"][0]["postconditions"]=json!([{"device":"light","on":true,"level":20}]);
    let mut h=Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(),doc).await;
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h,0x15,1,100);assert_eq!(mode_confirm(&h).await,0);assert_eq!(mode_value(&h,"v"),None,"On at100 is not20% proof");
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,20);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,12);mode_confirm(&h).await;assert_eq!(mode_value(&h,"v"),None,"fresh external level contradicts owned20% inference");assert_eq!(h.count("command_journal"),2);h.finish().await;
}
#[tokio::test]
async fn component_mode_requires_trusted_written_and_preserves_it_after_observed(){
    for trusted in [false,true]{let (cfg,doc)=component_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
        if trusted{assert_eq!(h.wire().await.len(),5,"LED remains Accepted until actual feedback");}
        tokio::time::sleep(Duration::from_millis(2)).await;let now=h.actor.monotonic_ms();
        let sql=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();sql.execute("UPDATE command_journal SET status='observed',updated_ms=?1 WHERE device_id='relay'",[now as i64]).unwrap();if !trusted{sql.execute("UPDATE command_journal SET status='written' WHERE device_id='execute-led'",[]).unwrap();}drop(sql);
        mode_frame(&mut h,3,1,0xf1);let n=mode_confirm(&h).await;assert_eq!(n,usize::from(trusted));assert_eq!(mode_value(&h,"v"),trusted.then_some(homesense_core::rule::Value::Integer(1)));h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_revoke_expiry_and_epoch_drop_pending_without_replay(){
    for reason in ["revoke","expiry","epoch"]{let (cfg,mut doc)=component_inputs();if reason=="expiry"{doc["component_mode_confirmations"][0]["timeout_ms"]=json!(1);}
        let mut h=Harness::from_inputs(cfg,doc).await;let controller=if reason=="revoke"{h.member}else{h.controller};h.request(Action::SceneAction{controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
        match reason{"revoke"=>{h.allowed.store(false,Ordering::Release);assert!(h.wire().await.is_empty());},"expiry"=>{tokio::time::sleep(Duration::from_millis(5)).await;},"epoch"=>{h.actor.transport_failed().await.unwrap();},_=>unreachable!()}
        tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);assert_eq!(mode_value(&h,"v"),None);
        if reason!="epoch"{assert!(h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().is_empty());}
        assert_eq!(h.count("command_journal"),2,"confirmation never retries/replays command");h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_known_branch_is_unchanged_and_late_proof_cannot_overwrite_newer_state(){
    let (cfg,doc)=component_inputs();let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);assert_eq!(mode_confirm(&h).await,1);
    let view=h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().pop().unwrap();let stale=crate::component_modes::Proof::prepare(&view,&h.configuration,&h.feedback,h.actor.monotonic_ms()).unwrap();
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)),"known AST retains eager Set ordering");assert!(!h.actor.component_mode_proof(h.controller,stale).await.unwrap());assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)));assert_eq!(h.wire().await,vec![0xae,3,1,0xf2,0xee]);tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf2);crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert_eq!(h.wire().await,vec![0xae,0x41,1,0xf2,0xee]);assert_eq!(h.count("command_journal"),4);h.finish().await;
}
#[test]
fn component_mode_schema_rejects_unreviewed_or_impossible_level_and_causal_policies(){
    let (cfg,doc)=component_inputs();let c=Configuration::parse(&cfg,ExecutionContext::Simulation).unwrap();assert!(Document::parse(&serde_json::to_vec(&doc).unwrap(),&c).is_ok());
    for mode in 0..7{let mut d=doc.clone();match mode{0=>d["component_mode_confirmations"][0]["review_evidence"]=json!(""),1=>d["component_mode_confirmations"][0]["postconditions"][0]["level"]=json!(20),2=>d["component_mode_confirmations"][0]["timeout_ms"]=json!(60001),3=>d["component_mode_confirmations"][0]["max_age_ms"]=json!(999999999),4=>d["component_mode_confirmations"][0]["variable"]=json!("missing"),5=>d["rules"]=json!([{"id":"bad-cascade","enabled":true,"review_evidence":"synthetic unsupported retained cascade","source":"synthetic/cascade","trigger":{"kind":"variable_changed","variable":"v"},"actions":[command("cascade/output","relay",json!({"kind":"switch","on":true}))]}]),_=>{d["component_mode_confirmations"][0]["postconditions"][0]["on"]=json!(false);d["component_mode_confirmations"][0]["postconditions"][0]["level"]=json!(0);}}assert!(Document::parse(&serde_json::to_vec(&d).unwrap(),&c).is_err(),"invalid mode policy{mode}");}
}
#[tokio::test]
async fn component_mode_unwritten_known_precondition_keeps_original_fresh_proof(){
    let (cfg,mut doc)=component_inputs();let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();cfg["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"][0]=command("v/actual-dim","light",level(20));doc["component_mode_confirmations"][0]["postconditions"]=json!([{"device":"relay","on":true},{"device":"light","on":true,"level":20}]);let mut h=Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(),doc).await;
    h.samples(true);let prior=h.feedback.snapshot().devices[&homesense_core::DeviceId::new("relay").unwrap()].observed_at_ms;
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,20);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(h.feedback.snapshot().devices[&homesense_core::DeviceId::new("relay").unwrap()].observed_at_ms,prior,"unwritten known power prerequisite must not require a post-unrelated-write requery");assert_eq!(h.count("command_journal"),2);h.finish().await;
}
#[tokio::test]
async fn component_mode_combined_off_requires_supply_not_ad8_residual(){
    let (cfg,mut doc)=component_inputs();let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();cfg["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");cfg["devices"][0]["supply"]=json!({"module_id":"relay","channel":1});cfg["devices"][0]["default_on_level"]=json!(100);
    doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"].as_array_mut().unwrap().insert(0,command("v/marker-on","marker",json!({"kind":"switch","on":true})));doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"][1]=command("v/logical-off","light",level(0));doc["component_mode_confirmations"][0]["postconditions"]=json!([{"device":"light","on":false},{"device":"marker","on":true}]);
    doc["immediate_scenes"][0]["activate"][0]["action"]["else_actions"][0]=command("v/logical-off-known","light",level(0));let mut h=Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(),doc).await;mode_frame(&mut h,0x15,1,100);h.samples(true);
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,0);assert_eq!(mode_confirm(&h).await,0,"AD8zero while reliable supplystillOn cannot prove combinedOff");
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,100);mode_frame(&mut h,3,1,0xf2);assert_eq!(mode_confirm(&h).await,1,"fresh actual supplyOff qualifies despite residualAD8level100");h.finish().await;
}
#[tokio::test]
async fn component_mode_unknown_first_companion_known_peer_waits_for_off_feedback_or_invalidates(){
    for succeed in [false,true]{let (cfg,mut doc)=component_inputs();let actions=doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"].as_array_mut().unwrap();actions.push(command("v/clear-peer","marker",json!({"kind":"switch","on":false})));actions.push(json!({"source":"v/peer-set","action":{"kind":"set","variable":"w","value":{"kind":"integer","value":0}}}));
        doc["component_mode_confirmations"].as_array_mut().unwrap().push(json!({"id":"peer-off","variable":"w","scenes":["act"],"first_value":0,"include_when_first_unknown":true,"timeout_ms":500,"max_age_ms":5000,"review_evidence":"synthetic explicit unknown-first clears known peer","postconditions":[{"device":"marker","on":false}]}));
        let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"other".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,2,0xf1);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(1)));
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(1)),"accepted peerOff is not established mode");h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,if succeed{0xf2}else{0xf1});let _=mode_confirm(&h).await;
        if succeed{assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(0)));}else{assert_eq!(mode_value(&h,"v"),None);tokio::time::sleep(Duration::from_millis(505)).await;assert!(h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().is_empty());assert_eq!(mode_value(&h,"w"),None,"failed affected known peer cannot retain a speculative stale component");}
        assert_eq!(h.count("command_journal"),5);h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_optin_known_partial_off_waits_for_actual_off(){
    let (cfg,mut doc)=component_inputs();let mut off=doc["component_mode_confirmations"][0].clone();off["id"]=json!("mode-a-off");off["first_value"]=json!(0);off["confirm_when_known"]=json!(true);off["postconditions"][0]["on"]=json!(false);doc["component_mode_confirmations"].as_array_mut().unwrap().push(off);
    let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);assert_eq!(mode_confirm(&h).await,1);
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);assert_eq!(mode_confirm(&h).await,0);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf2);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)));h.finish().await;
}
#[tokio::test]
async fn component_mode_dynamic_reviewed_outcomes_select_actual_evaluated_vector(){
    for (a,b,result) in [(false,false,0),(true,false,2),(false,true,4),(true,true,6)]{let (cfg,mut doc)=component_inputs();
        let leaves=|on_a:bool,on_b:bool,value:i64|json!([command("dynamic/a","relay",json!({"kind":"switch","on":on_a})),command("dynamic/b","marker",json!({"kind":"switch","on":on_b})),{"source":"dynamic/set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":value}}}]);
        let inner=|on_a:bool|json!([{"source":"dynamic/b-condition","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"device_on","device":"marker"},"right":{"kind":"bool","value":true}},"then_actions":leaves(on_a,true,if on_a{6}else{4}),"else_actions":leaves(on_a,false,if on_a{2}else{0})}}]);
        doc["immediate_scenes"][0]["activate"]=json!([{"source":"dynamic/a-condition","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"device_on","device":"relay"},"right":{"kind":"bool","value":true}},"then_actions":inner(true),"else_actions":inner(false)}}]);
        doc["component_mode_confirmations"]=json!([(false,false,0),(true,false,2),(false,true,4),(true,true,6)].iter().map(|(a,b,v)|json!({"id":format!("dynamic-{v}"),"variable":"v","scenes":["act"],"first_value":v,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic finite branch outcome","postconditions":[{"device":"relay","on":a},{"device":"marker","on":b}]})).collect::<Vec<_>>());
        let mut h=Harness::from_inputs(cfg,doc).await;mode_frame(&mut h,3,1,if a{0xf1}else{0xf2});mode_frame(&mut h,3,2,if b{0xf1}else{0xf2});h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),None);h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,if a{0xf1}else{0xf2});mode_frame(&mut h,3,2,if b{0xf1}else{0xf2});assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(result)));h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_shared_source_unselected_value_preserved_and_default_group_stays_strict(){
    for known in [false,true]{let (cfg,mut doc)=component_inputs();
        doc["timed_scenes"]=json!([{"id":"one","enabled":true,"review_evidence":"synthetic selection member","max_lateness_ms":1000,"steps":[{"source":"one/output","offset_ms":0,"device":"relay","command":{"kind":"switch","on":true}}]},{"id":"off-one","enabled":true,"review_evidence":"synthetic selection off","max_lateness_ms":1000,"steps":[{"source":"off/output","offset_ms":0,"device":"relay","command":{"kind":"switch","on":false}}]}]);
        doc["scene_switch_groups"]=json!([{"id":"shared","variable":"v","review_evidence":"synthetic source5 explicitly no member","unselected_values":[5],"members":[{"scene":"one","value":1,"off_scene":"off-one"}]}]);
        doc["scene_switch_groups"][0]["initialization"]=json!({"enabled":true,"policy":"unique_light_targets","max_age_ms":5000,"review_evidence":"synthetic shared initializer competition"});doc["component_mode_confirmations"][0]["first_value"]=json!(5);doc["component_mode_confirmations"][0]["shared_selection_group"]=json!("shared");
        let turn=|on:bool,value:i64|json!([command("shared/output","relay",json!({"kind":"switch","on":on})),{"source":"shared/set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":value}}}]);
        doc["immediate_scenes"][0]["activate"]=json!([{"source":"shared/unknown","action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":"v"}},"then_actions":turn(true,5),"else_actions":[{"source":"shared/known","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"v"},"right":{"kind":"integer","value":5}},"then_actions":turn(false,0),"else_actions":turn(true,5)}}]}}]);
        let mut h=Harness::from_inputs(cfg,doc).await;
        if known{rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap().execute("UPDATE logical_variable SET value=1,version=version+1 WHERE name='v'",[]).unwrap();}
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),known.then_some(homesense_core::rule::Value::Integer(5)));h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);if !known{let init=crate::scene_initialization::Prepared::prepare(&h.actor,h.controller,h.generation.clone(),&h.document.scene_switch_groups[0],&h.document,&h.feedback).await.unwrap().unwrap();assert_eq!(h.actor.initialize_scene_selection(h.controller,h.generation.clone(),init).await.unwrap_err(),hap::server::WriteStatus::ResourceBusy,"shared initializer cannot overwrite a reserved component outcome");assert_eq!(mode_confirm(&h).await,1);}
        let snapshot=h.actor.scene_selection_snapshot(h.controller,h.generation.clone(),vec!["v".into()]).await.unwrap();assert_eq!(snapshot["v"].stored.value,Some(homesense_core::rule::Value::Integer(5)));let details=crate::scene_switches::details(&h.document,&snapshot).unwrap();assert_eq!(details["one"].selected,Some(false));if !known{assert_eq!(details["one"].selection_source,"local_written_feedback_inference");}
        let values=snapshot.iter().map(|(n,v)|(n.clone(),v.stored.clone())).collect();let mut strict=h.document.clone();strict.scene_switch_groups[0].unselected_values.clear();assert!(crate::scene_switches::states(&strict,&values).is_err(),"another/default group cannot accept arbitrarysource5");
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)));h.wire().await;h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_unwritten_confirmation_vector_is_an_authority_dependency(){
    use crate::{web_auth::{Tokens,Grant,Role},source_auth::Source};
    let (cfg,mut doc)=component_inputs();doc["component_mode_confirmations"][0]["postconditions"].as_array_mut().unwrap().push(json!({"device":"marker","on":true}));let mut h=Harness::from_inputs(cfg,doc).await;mode_frame(&mut h,3,2,0xf1);
    let ids=homesense_config::keypad::scene_execute_devices(&h.document,&h.configuration,"act").unwrap();assert!(ids.contains(&homesense_core::DeviceId::new("marker").unwrap()));
    let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();const SECRET:&str="synthetic-component-scope-0123456789";tokens.insert(credential,SECRET,Grant{controller:h.member,role:Role::Member,scenes:["act".into()].into(),devices:["relay".into(),"execute-led".into()].into()}).unwrap();
    let action=Action::SceneAction{controller:h.member,revision:1,scene:"act".into(),execute:true};let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action,Some("act")).unwrap();let control=h.control.clone();assert!(h.drive(tokio::spawn(async move{control.request_authorized(action,source).await})).await.unwrap_err().contains("full-branch"));assert_eq!(h.count("command_journal"),0);assert_eq!(mode_value(&h,"v"),None);h.finish().await;
}
fn canonical_mode_inputs()->(Vec<u8>,Json){
    let (cfg,mut doc)=component_inputs();doc["variables"].as_array_mut().unwrap().extend([json!({"id":"base","kind":"integer","lifetime":"persistent"}),json!({"id":"tag","kind":"bool","lifetime":"persistent"})]);
    let outputs=|value:i64,a:bool,b:bool|json!([{"source":"source/set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":value}}},command("source/a","relay",json!({"kind":"switch","on":a})),command("source/b","marker",json!({"kind":"switch","on":b}))]);
    let known=json!([{"source":"source/known","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"v"},"right":{"kind":"integer","value":1}},"then_actions":outputs(0,false,false),"else_actions":outputs(4,true,false)}}]);
    let mut actions=json!([{"source":"source/unknown","action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":"v"}},"then_actions":outputs(1,true,true),"else_actions":known}}]);
    // Only pure metadata is appended after the original known source subtree.
    for (value,base,tag) in [(0,0,false),(1,1,false),(4,0,true)]{actions.as_array_mut().unwrap().push(json!({"source":format!("canonical/{value}"),"action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"v"},"right":{"kind":"integer","value":value}},"then_actions":[{"source":format!("canonical/base/{value}"),"action":{"kind":"set","variable":"base","value":{"kind":"integer","value":base}}},{"source":format!("canonical/tag/{value}"),"action":{"kind":"set","variable":"tag","value":{"kind":"bool","value":tag}}}],"else_actions":[]}}));}
    doc["immediate_scenes"][0]["activate"]=actions;
    let mut policies=vec![json!({"id":"source-first","variable":"v","scenes":["act"],"first_value":1,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic explicit source activation","postconditions":[{"device":"relay","on":true},{"device":"marker","on":true}]})];
    for (value,base,tag,a,b) in [(0,0,false,false,false),(1,1,false,true,true),(4,0,true,true,false)]{for (variable,final_value) in [("base",json!(base)),("tag",json!(tag))]{policies.push(json!({"id":format!("canonical-{variable}-{value}"),"variable":variable,"scenes":["act"],"first_value":final_value,"when_values":{"v":value},"confirm_when_known":true,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic finite final source selector","postconditions":[{"device":"relay","on":a},{"device":"marker","on":b}]}));}}
    doc["component_mode_confirmations"]=json!(policies);(cfg,doc)
}
#[tokio::test]
async fn component_mode_final_selectors_distinguish_same_false_and_atomically_confirm_unknown_source(){
    let (cfg,doc)=canonical_mode_inputs();let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();for name in ["v","base","tag"]{assert_eq!(mode_value(&h,name),None);}h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(mode_value(&h,"base"),Some(homesense_core::rule::Value::Integer(1)));assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(false)));
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)),"known sourceSet remains acceptance atomic");h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);assert_eq!(mode_confirm(&h).await,0,"tagfalse must select Mode0 Off proof, not Mode1 On proof");assert_eq!(mode_value(&h,"base"),Some(homesense_core::rule::Value::Integer(1)));tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf2);mode_frame(&mut h,3,2,0xf2);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"base"),Some(homesense_core::rule::Value::Integer(0)));assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(false)));h.finish().await;
}
#[tokio::test]
async fn component_mode_known44_to4_preserves_source_and_late_selector_version_cannot_commit_tags(){
    for late in [false,true]{let (cfg,doc)=canonical_mode_inputs();let mut h=Harness::from_inputs(cfg,doc).await;rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap().execute("UPDATE logical_variable SET value=44,version=version+1 WHERE name='v'",[]).unwrap();h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(4)));assert_eq!(mode_value(&h,"base"),None);assert_eq!(h.wire().await,vec![0xae,3,1,0xf1,0xee,0xae,3,2,0xf2,0xee],"postlude emits no physical output");tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf2);
        if late{let view=h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().pop().unwrap();let proof=crate::component_modes::Proof::prepare(&view,&h.configuration,&h.feedback,h.actor.monotonic_ms()).unwrap();rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap().execute("UPDATE logical_variable SET value=9,version=version+1 WHERE name='v'",[]).unwrap();assert!(!h.actor.component_mode_proof(h.controller,proof).await.unwrap());assert_eq!(mode_value(&h,"base"),None);assert_eq!(mode_value(&h,"tag"),None);assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(9)));}else{assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"base"),Some(homesense_core::rule::Value::Integer(0)));assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(true)));assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(4)));}h.finish().await;
    }
}
#[tokio::test]
async fn component_mode_unknown_selector_has_no_default_or_partial_acceptance(){
    let (cfg,mut doc)=canonical_mode_inputs();doc["component_mode_confirmations"].as_array_mut().unwrap().retain(|p|p["id"]!="canonical-tag-1");let mut h=Harness::from_inputs(cfg,doc).await;let result=h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await;assert!(result.unwrap_err().contains("outcome"));assert_eq!(h.count("command_journal"),0);for name in ["v","base","tag"]{assert_eq!(mode_value(&h,name),None);}h.finish().await;
}
#[tokio::test]
async fn component_mode_first_unknown_companion_cannot_spill_into_unrelated_manual_entry(){
    let (cfg,mut doc)=component_inputs();doc["component_mode_confirmations"].as_array_mut().unwrap().push(json!({"id":"other-companion","variable":"w","scenes":["other"],"first_value":0,"include_when_first_unknown":true,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic entry scoped unknown-first companion","postconditions":[{"device":"marker","on":false}]}));let mut h=Harness::from_inputs(cfg,doc).await;rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap().execute("UPDATE logical_variable SET value=1,version=version+1 WHERE name='w'",[]).unwrap();h.request(Action::SceneActions{controller:h.controller,revision:1,scenes:vec![("act".into(),true),("other".into(),true)]}).await.unwrap();assert_eq!(mode_value(&h,"v"),None);assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(0)),"known other entry keeps original eagerSet even when an independent entry is firstUnknown");assert_eq!(h.wire().await.len(),10,"only two physical light outputs precede the feedback barrier");h.finish().await;
}
fn logical_mode_inputs(physical_unknown:bool)->(Vec<u8>,Json){
    let (cfg,mut doc)=canonical_mode_inputs();let mut policies=vec![];for mut p in doc["component_mode_confirmations"].as_array().unwrap().clone(){if p["variable"]=="v"{policies.push(p);continue;}
        if physical_unknown{let mut original=p.clone();original["id"]=json!(format!("unknown-{}",p["id"].as_str().unwrap()));original["confirm_when_known"]=json!(false);original["include_when_first_unknown"]=json!(true);policies.push(original);}p["basis"]=json!("logical_source");p["postconditions"]=json!([]);policies.push(p);}doc["component_mode_confirmations"]=json!(policies);(cfg,doc)
}
fn seed_mode(h:&Harness,name:&str,value:i64){rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap().execute("UPDATE logical_variable SET value=?1,version=version+1 WHERE name=?2",rusqlite::params![value,name]).unwrap();}
#[tokio::test]
async fn component_mode_logical_source_keeps_same_false_outcomes_honest_and_initial_unknown_physical(){
    for initial in [0,1]{let (cfg,mut doc)=logical_mode_inputs(true);if initial==0{let branch=&mut doc["immediate_scenes"][0]["activate"][0]["action"]["else_actions"][0]["action"]["else_actions"];*branch=json!([{"source":"known/source1","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":1}}},command("known/a","relay",json!({"kind":"switch","on":true})),command("known/b","marker",json!({"kind":"switch","on":true}))]);}let mut h=Harness::from_inputs(cfg,doc).await;seed_mode(&h,"v",initial);h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1-initial)));assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(false)));let audit=Store::open(h.directory.join("runtime.sqlite")).unwrap().component_mode_audit(1,"tag",1).unwrap().unwrap();assert_eq!(audit.provenance,"local_source_canonicalization");let sql=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();let (source,tickets):(i64,String)=sql.query_row("SELECT s.source_value,a.tickets FROM component_mode_source s JOIN component_mode_audit a USING(revision,name,version) WHERE name='tag'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();assert_eq!(source,1-initial);assert!(tickets.is_empty(),"logical normalization never invents Written tickets");drop(sql);assert!(h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap().is_empty());h.wire().await;h.finish().await;}
    let (cfg,doc)=logical_mode_inputs(true);let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();for name in ["v","tag","base"]{assert_eq!(mode_value(&h,name),None,"Unknown source cannot use logical label shortcut");}h.wire().await;tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(false)));assert_eq!(Store::open(h.directory.join("runtime.sqlite")).unwrap().component_mode_audit(1,"tag",1).unwrap().unwrap().provenance,"local_written_with_feedback_inference");h.finish().await;
}
#[tokio::test]
async fn component_mode_logical_cannot_bypass_initial_unknown_or_erase_new_tag_owner(){
    let (cfg,doc)=logical_mode_inputs(false);let mut h=Harness::from_inputs(cfg,doc).await;assert!(h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.is_err());assert_eq!(h.count("command_journal"),0);assert_eq!(mode_value(&h,"v"),None);h.finish().await;
    let (cfg,doc)=logical_mode_inputs(true);let mut h=Harness::from_inputs(cfg,doc).await;seed_mode(&h,"v",44);h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(4)));assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(true)));seed_mode(&h,"tag",0);seed_mode(&h,"v",9);h.actor.component_mode_views(h.controller,h.generation.clone()).await.unwrap();assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(false)),"old logical watch must not erase newer tag version");assert_eq!(mode_value(&h,"base"),None,"still-owned old label invalidates with source version");assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(9)));h.wire().await;h.finish().await;
}
#[tokio::test]
async fn component_mode_logical_pure_set_has_zero_wire_receipts_and_epoch_invalidation(){
    let (cfg,mut doc)=logical_mode_inputs(true);let actions=doc["immediate_scenes"][0]["activate"].as_array_mut().unwrap();actions[0]=json!({"source":"known/pure-source-set","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":4}}});let mut h=Harness::from_inputs(cfg,doc).await;seed_mode(&h,"v",44);h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(h.count("command_journal"),0);assert_eq!(h.count("rule_admission"),1);assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(true)));assert_eq!(mode_value(&h,"base"),Some(homesense_core::rule::Value::Integer(0)));assert!(h.wire().await.is_empty());h.actor.transport_failed().await.unwrap();let _=h.actor.task_generation().await;assert_eq!(mode_value(&h,"tag"),None);assert_eq!(mode_value(&h,"base"),None);h.finish().await;
}
#[test]
fn component_mode_policy_bound_accepts512_rejects513_without_removing_vectors(){
    let (cfg,mut doc)=component_inputs();let config=Configuration::parse(&cfg,ExecutionContext::Simulation).unwrap();let template=doc["component_mode_confirmations"][0].clone();let policies=(0..512).map(|n|{let mut p=template.clone();p["id"]=json!(format!("bounded-{n}"));p["first_value"]=json!(n);p}).collect::<Vec<_>>();doc["component_mode_confirmations"]=json!(policies);assert!(Document::parse(&serde_json::to_vec(&doc).unwrap(),&config).is_ok());let mut extra=template;extra["id"]=json!("bounded-512");extra["first_value"]=json!(512);doc["component_mode_confirmations"].as_array_mut().unwrap().push(extra);assert!(Document::parse(&serde_json::to_vec(&doc).unwrap(),&config).is_err());
}
#[tokio::test]
async fn component_mode_initial_unknown_scope_separates_partial_off_from_known_canonical_false(){
    for known in [false,true]{let (cfg,mut doc)=logical_mode_inputs(true);doc["component_mode_confirmations"].as_array_mut().unwrap().push(json!({"id":"partial-off-unknown-source","variable":"tag","scenes":["act"],"first_value":false,"confirm_when_known":true,"when_initial_unknown":["v"],"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic partial fallback only when sourceUnknown","postconditions":[{"device":"relay","on":false}]}));
        doc["immediate_scenes"][0]["activate"][0]["action"]["then_actions"]=json!([command("partial/off","relay",json!({"kind":"switch","on":false})),{"source":"partial/tag","action":{"kind":"set","variable":"tag","value":{"kind":"bool","value":false}}}]);let mut h=Harness::from_inputs(cfg,doc).await;seed_mode(&h,"tag",1);if known{seed_mode(&h,"v",1);}
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();assert_eq!(mode_value(&h,"tag"),Some(homesense_core::rule::Value::Bool(!known)),"known source uses honest logical Set; unknown source partialOff waits feedback");h.wire().await;
        if !known{tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf2);assert_eq!(mode_confirm(&h).await,1);assert_eq!(mode_value(&h,"v"),None);}
        let version=Store::open(h.directory.join("runtime.sqlite")).unwrap().logical_variable(1,"tag").unwrap().unwrap().version;let audit=Store::open(h.directory.join("runtime.sqlite")).unwrap().component_mode_audit(1,"tag",version).unwrap().unwrap();assert_eq!(audit.provenance,if known{"local_source_canonicalization"}else{"local_written_with_feedback_inference"});h.finish().await;
    }
}

fn writer_batch_inputs()->(Vec<u8>,Json){
    let(cfg,mut doc)=component_inputs();let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();
    cfg["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({"number":3,"label":"Third","led_endpoint":{"id":"third-led","name":"Third LED","enabled":true}}));
    doc["component_mode_confirmations"]=json!([]);doc["rules"]=json!([]);
    doc["immediate_scenes"][0]["activate"]=json!([
        command("led/a","execute-led",json!({"kind":"panel_led","on":true})),
        command("light/initial","light",level(20)),
        command("relay/on","relay",json!({"kind":"switch","on":true})),
        command("led/b","other-led",json!({"kind":"panel_led","on":false})),
        command("light/final","light",level(40)),
        command("marker/on","marker",json!({"kind":"switch","on":true})),
        command("led/c","third-led",json!({"kind":"panel_led","on":false}))]);
    (serde_json::to_vec(&cfg).unwrap(),doc)
}
struct CountWrites{calls:Arc<Mutex<Vec<Vec<u8>>>>,limit:usize,written:usize,rx:std::collections::VecDeque<u8>}
impl std::io::Read for CountWrites{fn read(&mut self,out:&mut[u8])->std::io::Result<usize>{
    if self.rx.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into());}let n=out.len().min(self.rx.len());for byte in &mut out[..n]{*byte=self.rx.pop_front().unwrap();}Ok(n)
}}
impl std::io::Write for CountWrites{
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        if self.written>=self.limit{return Err(std::io::ErrorKind::BrokenPipe.into());}
        let n=bytes.len().min(self.limit-self.written);self.calls.lock().unwrap().push(bytes[..n].to_vec());self.written+=n;
        for frame in bytes[..n].as_chunks::<5>().0 {if (1..=8).contains(&frame[2]){self.rx.extend([0xae,frame[1],0xd0+frame[2],frame[3],0xee]);}}
        Ok(n)
    }
    fn flush(&mut self)->std::io::Result<()>{panic!("unbuffered")}
}
type BatchWrites=Arc<Mutex<Vec<Vec<u8>>>>;
fn batch_owner(h:&Harness,limit:usize)->(TransportOwner<CountWrites>,BatchWrites){
    let calls=Arc::default();let owner=TransportOwner::new(h.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(CountWrites {calls:Arc::clone(&calls),limit,written:0,rx:Default::default()},1))].into());(owner,calls)
}
async fn write_lighting_prefix<S:std::io::Read+std::io::Write+Send+'static>(h:&mut Harness,owner:&mut TransportOwner<S>,local:bool)->Vec<usize> {
    let mut sizes=vec![];
    let mut completed=0;
    while completed<4 {
        owner.finish_reply_turnaround().await;
        let next=if local {owner.local_step(&Default::default()).await.unwrap()}else{owner.step().await.unwrap()};
        let outcome=match next {Some(value)=>value,None=>{
            panic!("staged writer stalled after {completed} lights; views={:?}; physical={:?}",h.actor.control_batch_views(h.controller,h.generation.clone()).await.unwrap(),h.feedback.snapshot());
        }};
        outcome.result.unwrap();completed+=outcome.completed.len();sizes.push(outcome.completed.len()*5);
        assert!(completed<=4,"no LED may escape before the final feedback");
        tokio::time::sleep(Duration::from_millis(3)).await;
        let reply=owner.receive("bus").await.unwrap();
        if completed<4 {
            h.feedback.apply(&reply).unwrap();
            for _ in 0..2 {crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();}
        }
    }
    owner.finish_reply_turnaround().await;
    sizes
}
#[tokio::test]
async fn batch_daily_four_lights_wait_for_modules_then_three_leds_require_postwrite_final_feedback(){
    let(cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let(mut owner,calls)=batch_owner(&h,usize::MAX);
    let sizes=write_lighting_prefix(&mut h,&mut owner,false).await;assert_eq!(sizes,vec![5,5,5,5]);
    assert!(owner.step().await.unwrap().is_none(),"LEDs remain Accepted before final postwrite feedback");
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert!(owner.step().await.unwrap().is_none(),"prewrite marker cannot release LEDs");
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,20);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert!(owner.step().await.unwrap().is_none(),"intermediate target20 does not satisfy final40");
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,40);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();let leds=owner.step().await.unwrap().unwrap();leds.result.unwrap();assert_eq!(leds.completed.len(),3);
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![5,5,5,5,15]);
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();for id in 1..=7{assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Written));}drop(store);h.finish().await;
}
#[tokio::test]
async fn batch_partial_write_keeps_complete_prefix_partial_uncertain_unsent_failed_and_no_replay(){
    // Deliberate timeline phases retain their atomic-write contract; staged
    // recipes have separate next-module proof tests above and in batch_collection.
    for amount in [0,5,7,10,14,15,19]{
        let(cfg,doc)=delayed_feedback_phase_inputs();let h=Harness::from_inputs(cfg,doc).await;
        let now=h.actor.monotonic_ms();let mut runtime=crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,h.document.timed_scenes["delayed-feedback"].steps.clone(),now,5000).await.unwrap();
        runtime.poll(now).await.unwrap().unwrap();
        let(mut owner,calls)=batch_owner(&h,amount);let outcome=owner.step().await.unwrap().unwrap();assert_eq!(outcome.result.unwrap_err().bytes_written,amount);
        let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
        for index in 0..4{let expected=if amount>=(index+1)*5{homesense_storage::journal::Status::Written}else if amount>index*5{homesense_storage::journal::Status::Uncertain}else{homesense_storage::journal::Status::Failed};assert_eq!(store.command_status(h.actor.boot_id(),index as u64+1).unwrap(),Some(expected));}
        for id in 5..=7{assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Cancelled));}
        assert_eq!(calls.lock().unwrap().iter().map(Vec::len).sum::<usize>(),amount);assert!(owner.step().await.is_err());drop(store);h.finish().await;
    }
}

#[tokio::test]
async fn batch_cross_gateway_led_first_cannot_block_lights_or_bypass_feedback_and_independent_request_progresses(){
    let(cfg,mut doc)=writer_batch_inputs();let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();
    cfg["gateways"].as_array_mut().unwrap().push(json!({"id":"second","endpoint":"127.0.0.1:2","modules":[{"id":"second-relay","address":"04","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}));
    cfg["devices"].as_array_mut().unwrap().push(json!({"id":"second-light","name":"Second bus light","module_id":"second-relay","channel":1,"enabled":true}));
    doc["immediate_scenes"][0]["activate"]=json!([command("led-first","execute-led",json!({"kind":"panel_led","on":true})),command("other-bus-light","second-light",json!({"kind":"switch","on":true}))]);
    let mut h=Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(),doc).await;
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let a=Arc::default();let b=Arc::default();let wire=|calls|hb_transport::Connection::for_simulation(CountWrites {calls,limit:usize::MAX,written:0,rx:Default::default()},1);
    let mut owner=TransportOwner::new(h.actor.clone(),[("bus".into(),wire(Arc::clone(&a))),("second".into(),wire(Arc::clone(&b)))].into());
    let result=owner.step().await.unwrap().unwrap();result.result.unwrap();assert_eq!(result.device_id.as_str(),"second-light");assert!(a.lock().unwrap().is_empty());assert_eq!(b.lock().unwrap()[0].len(),5);
    h.actor.submit(homesense_hap::requests::Batch{controller:h.controller,intents:vec![homesense_hap::requests::Intent{device:homesense_core::DeviceId::new("marker").unwrap(),command:homesense_core::Command::Switch(true)}],subscriptions:vec![]}).await.unwrap();
    let independent=owner.step().await.unwrap().unwrap();independent.result.unwrap();assert_eq!(independent.device_id.as_str(),"marker");assert_eq!(a.lock().unwrap().len(),1);assert!(owner.step().await.unwrap().is_none());
    tokio::time::sleep(Duration::from_millis(2)).await;
    h.feedback.apply(&GatewayRead{gateway_id:"second".into(),transport_epoch:1,received_at_ms:h.actor.monotonic_ms(),result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:4,command:0xd1,value:0xf1}]}}).unwrap();
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(a.lock().unwrap()[1],vec![0xae,0x41,1,0xf1,0xee]);h.finish().await;
}
#[tokio::test]
async fn batch_independent_hap_requests_with_absent_run_ids_never_merge_but_one_two_light_request_does(){
    let h=Harness::new(rules()).await;let(mut owner,calls)=batch_owner(&h,usize::MAX);
    let batch=|devices:&[&str]|homesense_hap::requests::Batch{controller:h.controller,intents:devices.iter().map(|id|homesense_hap::requests::Intent{device:homesense_core::DeviceId::new(*id).unwrap(),command:homesense_core::Command::Switch(true)}).collect(),subscriptions:vec![]};
    h.actor.submit(batch(&["relay","marker"])).await.unwrap();let first=owner.step().await.unwrap().unwrap();first.result.unwrap();assert_eq!(first.completed.len(),2);assert_eq!(calls.lock().unwrap()[0].len(),10);
    h.actor.submit(batch(&["relay"])).await.unwrap();h.actor.submit(batch(&["marker"])).await.unwrap();
    for _ in 0..2{let outcome=owner.step().await.unwrap().unwrap();outcome.result.unwrap();assert_eq!(outcome.completed.len(),1);}
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![10,5,5]);h.finish().await;
}
#[tokio::test]
async fn batch_timed_due_phase_keeps_source_step_receipts_and_delay_boundary(){
    let(cfg,mut doc)=writer_batch_inputs();let sources=doc["immediate_scenes"][0]["activate"].as_array().unwrap().clone();
    doc["timed_scenes"]=json!([{"id":"phase","enabled":true,"review_evidence":"synthetic whole due phase","max_lateness_ms":5000,"steps":sources.iter().map(|node|json!({"source":node["source"],"offset_ms":0,"device":node["action"]["device"],"command":node["action"]["command"]})).chain(std::iter::once(json!({"source":"later/source","offset_ms":10000,"device":"marker","command":{"kind":"switch","on":false}}))).collect::<Vec<_>>()}]);
    let mut h=Harness::from_inputs(cfg,doc).await;let scene=h.document.timed_scenes["phase"].clone();let now=h.actor.monotonic_ms();
    let mut runtime=crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,scene.steps,now,5000).await.unwrap();
    let ids=runtime.poll(now).await.unwrap().unwrap();assert_eq!(ids.len(),7);
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();let rows=store.scene_commands(h.actor.boot_id(),runtime.audit_id(),0,128).unwrap();assert_eq!(rows.len(),7);assert_eq!(rows.iter().map(|r|r.step).collect::<Vec<_>>(),vec![1,2,4,5,0,3,6]);drop(store);
    let(mut owner,calls)=batch_owner(&h,usize::MAX);owner.step().await.unwrap().unwrap().result.unwrap();assert_eq!(calls.lock().unwrap()[0].len(),20);assert!(owner.step().await.unwrap().is_none());
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();owner.step().await.unwrap().unwrap().result.unwrap();runtime.reconcile().await.unwrap();
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![20,15]);assert!(runtime.poll(h.actor.monotonic_ms()).await.unwrap().is_none(),"different due time is not admitted early");runtime.cancel().await.unwrap();h.finish().await;
}

#[tokio::test]
async fn batch_gate_expiry_tombstone_never_releases_a_longer_original_led_deadline(){
    use homesense_core::{queue::{QueuedCommand,Origin},Command,DeviceId,Percent,LevelTransition};
    let(cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let commands=vec![QueuedCommand{id:1,device_id:DeviceId::new("light").unwrap(),command:Command::SetLevel{level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate},origin:Origin::Rule,epochs:h.generation.epochs,deadline_ms:1,run_id:Some(1)},QueuedCommand{id:5,device_id:DeviceId::new("execute-led").unwrap(),command:Command::PanelLed(true),origin:Origin::Rule,epochs:h.generation.epochs,deadline_ms:10000,run_id:Some(1)}];
    let mut gate=crate::control_batch::Owner::default();gate.accepted(&h.configuration,&commands,h.generation.clone(),h.controller,None,vec![],&Default::default(),0,0);
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
    for now in [2,100,9999]{gate.audit(&store,Some(&h.generation),now,|_|true,&Default::default()).unwrap();assert!(!gate.eligible(5,now),"expired proof can never turn into a standalone LED request");assert!(gate.views().is_empty());}
    drop(store);h.finish().await;
}

#[tokio::test]
async fn component_mode_nonmatching_unknown_tag_primary_cannot_activate_physical_companion_on_known_source_path(){
    let(cfg,mut doc)=component_inputs();let other=doc["immediate_scenes"][1].clone();doc["variables"]=json!([{"id":"v","kind":"integer","lifetime":"persistent"},{"id":"tag","kind":"bool","lifetime":"persistent"},{"id":"peer","kind":"bool","lifetime":"persistent"}]);doc["immediate_scenes"]=json!([{"id":"act","enabled":true,"review_evidence":"synthetic known selector with unknown canonical tags","activate":[{"source":"read-known","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"variable","id":"v"},"right":{"kind":"integer","value":4}},"then_actions":[{"source":"source0","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":0}}},{"source":"tagfalse","action":{"kind":"set","variable":"tag","value":{"kind":"bool","value":false}}},{"source":"peerfalse","action":{"kind":"set","variable":"peer","value":{"kind":"bool","value":false}}},command("actual-off","relay",json!({"kind":"switch","on":false})),command("led-off","execute-led",json!({"kind":"panel_led","on":false}))],"else_actions":[]}}]}]);
    doc["component_mode_confirmations"]=json!([
        {"id":"tag-unknown-on","variable":"tag","scenes":["act"],"first_value":true,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic inactive opposite physical policy","postconditions":[{"device":"relay","on":true}]},
        {"id":"peer-physical-companion","variable":"peer","scenes":["act"],"first_value":false,"include_when_first_unknown":true,"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic only actual unknown primary may own","postconditions":[{"device":"marker","on":false}]},
        {"id":"tag-known","variable":"tag","scenes":["act"],"first_value":false,"confirm_when_known":true,"basis":"logical_source","when_values":{"v":0},"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic original known source canonical tag","postconditions":[]},
        {"id":"peer-known","variable":"peer","scenes":["act"],"first_value":false,"confirm_when_known":true,"basis":"logical_source","when_values":{"v":0},"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"synthetic original known source canonical peer","postconditions":[]}]);
    doc["immediate_scenes"].as_array_mut().unwrap().push(other);doc["variables"].as_array_mut().unwrap().push(json!({"id":"w","kind":"integer","lifetime":"persistent"}));
    let mut h=Harness::from_inputs(cfg,doc).await;seed_mode(&h,"v",4);h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(0)));let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();for name in ["tag","peer"]{let value=store.logical_variable(1,name).unwrap().unwrap();assert_eq!(value.value,Some(homesense_core::rule::Value::Bool(false)));assert_eq!(store.component_mode_audit(1,name,value.version).unwrap().unwrap().provenance,"local_source_canonicalization");}drop(store);h.finish().await;
}

#[tokio::test]
async fn batch_timed_manual_takeover_cancels_only_one_expanded_member_and_preserves_same_due_peer(){
    use homesense_core::{DeviceId,Command,scene::{Step,Status}};
    for writing in [false,true]{
        let(cfg,mut doc)=writer_batch_inputs();doc["immediate_scenes"][0]["activate"]=json!([command("safe logical","light",level(40))]);let mut cfg:Json=serde_json::from_slice(&cfg).unwrap();cfg["devices"][0]["supply"]=json!({"module_id":"relay","channel":1});cfg["devices"][0]["default_on_level"]=json!(100);
        let h=Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(),doc).await;
        let light=DeviceId::new("light").unwrap();let peer=DeviceId::new("marker").unwrap();
        let steps=vec![Step{device:light.clone(),command:homesense_core::Command::SetLevel{level:homesense_core::Percent::new(40).unwrap(),transition:homesense_core::LevelTransition::Immediate},delay_ms:0,source:"member/combined".into()},Step{device:peer.clone(),command:Command::Switch(true),delay_ms:0,source:"member/peer".into()}];
        let now=h.actor.monotonic_ms();let mut scene=crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,steps,now,5000).await.unwrap();let ids=scene.poll(now).await.unwrap().unwrap();assert_eq!(ids.len(),3);
        let selected=if writing{Some(h.actor.dispatch().await.unwrap().unwrap())}else{None};
        assert_eq!(scene.cancel_device(&light).await.unwrap(),if writing{1}else{2});
        let states=h.actor.command_statuses(h.controller,h.generation.clone(),ids.clone()).await.unwrap();assert_eq!(states[2],homesense_storage::journal::Status::Accepted);assert_eq!(states[1],homesense_storage::journal::Status::Cancelled);
        if let Some(ticket)=selected{assert_eq!(states[0],homesense_storage::journal::Status::Writing);let mut wire=hb_transport::Connection::for_simulation(Memory(Arc::default()),1);crate::transport_owner::send_ticket(&ticket,&mut wire,||h.actor.monotonic_ms()).unwrap();h.actor.finish_write(ticket.command.id,true).await.unwrap();}else{assert_eq!(states[0],homesense_storage::journal::Status::Cancelled);}
        let(mut owner,bytes)=batch_owner(&h,usize::MAX);let outcome=owner.step().await.unwrap().unwrap();outcome.result.unwrap();assert_eq!(outcome.completed,vec![(ids[2],peer)]);assert_eq!(bytes.lock().unwrap().as_slice(),&[vec![0xae,3,2,0xf1,0xee]]);
        scene.reconcile().await.unwrap();assert_eq!(scene.status(),Status::Partial);assert!(owner.step().await.unwrap().is_none());h.finish().await;
    }
}

#[tokio::test]
async fn batch_timed_pre_admission_takeover_of_second_member_preserves_first_without_replaying_override(){
    use homesense_core::{DeviceId,Command,scene::{Step,Status}};
    let(cfg,doc)=writer_batch_inputs();let h=Harness::from_inputs(cfg,doc).await;
    let a=DeviceId::new("relay").unwrap();let b=DeviceId::new("marker").unwrap();let now=h.actor.monotonic_ms();
    let steps=[a.clone(),b.clone()].into_iter().map(|device|Step{source:format!("phase/{}",device.as_str()),device,command:Command::Switch(true),delay_ms:0}).collect();
    let mut scene=crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,steps,now,5000).await.unwrap();
    let manual=h.actor.submit(homesense_hap::requests::Batch{controller:h.controller,intents:vec![homesense_hap::requests::Intent{device:b.clone(),command:Command::Switch(false)}],subscriptions:vec![]}).await.unwrap();
    assert!(scene.poll(now).await.unwrap().is_none());assert_eq!(h.count("command_journal"),1,"known rejected cohort cannot allocate receipts or replay B");
    let(mut owner,bytes)=batch_owner(&h,usize::MAX);let prior=owner.step().await.unwrap().unwrap();prior.result.unwrap();assert_eq!(prior.completed,vec![(manual[0],b)]);
    let kept=scene.poll(now+1).await.unwrap().unwrap();assert_eq!(kept.len(),1);let next=owner.step().await.unwrap().unwrap();next.result.unwrap();assert_eq!(next.completed,vec![(kept[0],a)]);scene.reconcile().await.unwrap();assert_eq!(scene.status(),Status::Partial);
    assert!(scene.poll(now+2).await.unwrap().is_none());assert!(owner.step().await.unwrap().is_none());assert_eq!(bytes.lock().unwrap().as_slice(),&[vec![0xae,3,2,0xf2,0xee],vec![0xae,3,1,0xf1,0xee]]);h.finish().await;
}

fn delayed_feedback_phase_inputs() -> (Vec<u8>,Json) {
    let (cfg, mut doc)=writer_batch_inputs();
    let sources=doc["immediate_scenes"][0]["activate"].as_array().unwrap().clone();
    doc["timed_scenes"]=json!([{"id":"delayed-feedback","enabled":true,"review_evidence":"synthetic bounded feedback wait","max_lateness_ms":5000,
        "steps":sources.iter().map(|node|json!({"source":node["source"],"offset_ms":0,"device":node["action"]["device"],"command":node["action"]["command"]})).collect::<Vec<_>>()}]);
    (cfg,doc)
}
#[tokio::test]
async fn mixed_phase_leds_wait_for_delayed_exact_feedback_without_extending_light_tickets() {
    for delay in [2500, 8600] {
        let (cfg,doc)=delayed_feedback_phase_inputs();
        let mut h = Harness::from_inputs(cfg, doc).await;
        let now = h.actor.monotonic_ms();
        let definition = h.document.timed_scenes["delayed-feedback"].clone();
        let mut runtime = crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,definition.steps,now,5000).await.unwrap();
        assert_eq!(runtime.poll(now).await.unwrap().unwrap().len(),7);
        let connection = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        let deadline = |id| connection.query_row("SELECT deadline_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![h.actor.boot_id(),id],|row|row.get::<_,i64>(0)).unwrap();
        assert_eq!(deadline(5)-deadline(1),13000,"new LED total budget is separate from unchanged light TTL");
        drop(connection);
        let (mut owner,calls) = batch_owner(&h,usize::MAX);
        let _sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
        assert!(owner.step().await.unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(delay)).await;
        mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
        crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();
        let leds = owner.step().await.unwrap().expect("exact physical feedback inside total budget releases LED phase");
        leds.result.unwrap();assert_eq!(leds.completed.len(),3);
        runtime.reconcile().await.unwrap();
        assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![20,15]);
        let store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
        for id in 1..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Written));}
        drop(store);h.finish().await;
    }
}

#[tokio::test]
async fn mixed_phase_led_total_budget_never_overrides_an_earlier_causal_deadline() {
    use homesense_core::{Command,DeviceId,LevelTransition,Percent};
    use homesense_hap::requests::{Batch,Intent};
    let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    let cap=h.actor.monotonic_ms()+1000;
    let batch=Batch {controller:h.controller,subscriptions:vec![],intents:vec![
        Intent {device:DeviceId::new("light").unwrap(),command:Command::SetLevel {level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate}},
        Intent {device:DeviceId::new("execute-led").unwrap(),command:Command::PanelLed(true)},
    ]};
    let ids=h.actor.submit_scene_with_feedback_before(h.generation.clone(),batch,0,None,Some(cap)).await.unwrap();assert_eq!(ids.len(),2);
    let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    for id in &ids {let deadline=connection.query_row("SELECT deadline_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![h.actor.boot_id(),*id as i64],|row|row.get::<_,i64>(0)).unwrap();assert_eq!(deadline,cap as i64);}
    drop(connection);
    let (mut owner,calls)=batch_owner(&h,usize::MAX);let light=owner.step().await.unwrap().unwrap();light.result.unwrap();assert_eq!(light.completed.len(),1);
    tokio::time::sleep(Duration::from_millis(1200)).await;mode_frame(&mut h,0x15,1,40);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert!(owner.step().await.unwrap().is_none());
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![5]);
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();assert_eq!(store.command_status(h.actor.boot_id(),ids[0]).unwrap(),Some(homesense_storage::journal::Status::Written));assert_eq!(store.command_status(h.actor.boot_id(),ids[1]).unwrap(),Some(homesense_storage::journal::Status::Cancelled));drop(store);h.finish().await;
}

#[tokio::test]
async fn mixed_phase_leds_without_feedback_expire_partial_and_late_feedback_never_replays() {
    let (cfg,doc)=delayed_feedback_phase_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    let now=h.actor.monotonic_ms();let definition=h.document.timed_scenes["delayed-feedback"].clone();
    let mut runtime=crate::scene_runtime::SceneRuntime::new(h.actor.clone(),h.controller,definition.steps,now,5000).await.unwrap();
    assert_eq!(runtime.poll(now).await.unwrap().unwrap().len(),7);
    let (mut owner,calls)=batch_owner(&h,usize::MAX);let _sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
    let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let deadline=connection.query_row("SELECT deadline_ms FROM command_journal WHERE boot_id=?1 AND command_id=5",[h.actor.boot_id()],|row|row.get::<_,i64>(0)).unwrap() as u64;
    drop(connection);
    tokio::time::sleep(Duration::from_millis(deadline.saturating_add(30).saturating_sub(h.actor.monotonic_ms()))).await;
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert!(owner.step().await.unwrap().is_none());
    runtime.reconcile().await.unwrap();assert_eq!(runtime.status(),homesense_core::scene::Status::Partial);
    let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    for id in 5..=7 {let row=connection.query_row("SELECT status,cancellation_reason FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![h.actor.boot_id(),id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap();assert_eq!(row,("cancelled".into(),"Expired".into()));}
    drop(connection);
    mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();assert!(owner.step().await.unwrap().is_none());runtime.reconcile().await.unwrap();assert_eq!(runtime.status(),homesense_core::scene::Status::Partial);
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![20]);h.finish().await;
}

#[tokio::test]
async fn production_scene_writes_keep_accepted_feedback_work_after_http_handling_deadline() {
    for timed in [false,true] {
        let (cfg,doc)=delayed_feedback_phase_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
        let id=if timed {"delayed-feedback"} else {"act"};
        h.request(Action::SceneWrites {controller:h.member,revision:1,scenes:vec![(id.into(),Some(true))]}).await.unwrap();
        if timed {h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();}
        let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        let led_deadline=connection.query_row("SELECT deadline_ms FROM command_journal WHERE boot_id=?1 AND command_id=5",[h.actor.boot_id()],|row|row.get::<_,i64>(0)).unwrap() as u64;
        assert!(led_deadline.saturating_sub(h.actor.monotonic_ms())>14000,"HTTP handling cutoff must not become accepted LED execution cutoff");
        drop(connection);
        let (mut owner,calls)=batch_owner(&h,usize::MAX);let mut sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
        assert!(owner.step().await.unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(8600)).await;
        mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
        for _ in 0..2 {crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();}
        let leds=owner.step().await.unwrap().expect("production entry preserves bounded accepted work");leds.result.unwrap();assert_eq!(leds.completed.len(),3);
        sizes.push(15);assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),sizes);
        let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();for id in 1..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Written));}drop(store);h.finish().await;
    }
}

#[tokio::test]
async fn selected_led_keeps_short_delivery_fence_through_actual_transport_guard() {
    let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    h.request(Action::SceneWrites {controller:h.member,revision:1,scenes:vec![("act".into(),Some(true))]}).await.unwrap();
    let (mut owner,_)=batch_owner(&h,usize::MAX);write_lighting_prefix(&mut h,&mut owner,false).await;
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
    for _ in 0..2 {crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();}
    let ticket=h.actor.dispatch().await.unwrap().unwrap();assert!(matches!(ticket.command.command,homesense_core::Command::PanelLed(_)));
    let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let total=connection.query_row("SELECT deadline_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![h.actor.boot_id(),ticket.command.id as i64],|r|r.get::<_,i64>(0)).unwrap() as u64;
    assert!(total-ticket.command.deadline_ms>10000,"selected writer must carry2s wire fence, not15s ticket lifetime");drop(connection);
    let calls=Arc::new(Mutex::new(vec![]));let mut wire=hb_transport::Connection::for_simulation(CountWrites {calls:calls.clone(),limit:usize::MAX,written:0,rx:Default::default()},1);
    let expired=ticket.command.deadline_ms;
    let result=crate::transport_owner::send_ticket(&ticket,&mut wire,||expired).unwrap_err();assert_eq!(result.bytes_written,0);assert!(calls.lock().unwrap().is_empty());
    h.actor.finish_write(ticket.command.id,false).await.unwrap();h.finish().await;
}

#[tokio::test]
async fn invalidated_led_cancel_audit_failure_rolls_back_rows_and_fails_closed_without_wire() {
    let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    h.request(Action::SceneWrites {controller:h.member,revision:1,scenes:vec![("act".into(),Some(true))]}).await.unwrap();
    let (mut owner,calls)=batch_owner(&h,usize::MAX);let sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
    let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_led_cancel BEFORE UPDATE OF status ON command_journal WHEN OLD.command_id>=5 AND NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'synthetic LED cancellation audit failure'); END;").unwrap();
    h.allowed.store(false,Ordering::Release);
    assert!(h.actor.dispatch().await.is_err(),"durable cancellation failure must never release pending LEDs");
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();for id in 5..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Accepted),"failed transaction must not claim cancellation success");}drop(store);
    assert!(!h.actor.diagnostics(h.controller,h.generation.clone()).await.map(|d|d.ready()).unwrap_or(false));
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),sizes);
    connection.execute_batch("DROP TRIGGER reject_led_cancel").unwrap();drop(connection);h.finish().await;
}

#[test]
fn expired_or_revoked_projection_through_real_owners_cancels_without_poisoning_healthy_control() {
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
    runtime.block_on(async {
        for (local,expiry,audit_fault) in [(false,true,false),(true,true,false),(false,false,false),(true,false,false),(false,false,true)] {
            use crate::{source_auth::Source,web_auth::{Grant,Role,Tokens}};
            use homesense_hap::requests::{Batch,Intent};
            eprintln!("projection owner case local={local} expiry={expiry} audit_fault={audit_fault}");
            let (cfg,doc)=delayed_feedback_phase_inputs();let mut value:Json=serde_json::from_slice(&cfg).unwrap();
            if local {
                let mut module=value["gateways"][0]["modules"].as_array().unwrap().iter().find(|m|m["id"]=="relay").unwrap().clone();module["id"]=json!("independent-module");
                value["gateways"].as_array_mut().unwrap().push(json!({"id":"other","endpoint":"127.0.0.1:50008","modules":[module]}));
                value["devices"].as_array_mut().unwrap().push(json!({"id":"independent","name":"Independent synthetic output","module_id":"independent-module","channel":1,"enabled":true}));
            }
            let mut h=Harness::from_inputs(serde_json::to_vec(&value).unwrap(),doc).await;
            let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();const SECRET:&str="synthetic-projection-revocation-0123456789";
            tokens.insert(credential,SECRET,Grant {controller:h.member,role:Role::Member,scenes:["delayed-feedback".into()].into(),devices:h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect()}).unwrap();
            let action=Action::SceneWrites {controller:h.member,revision:1,scenes:vec![("delayed-feedback".into(),Some(true))]};
            let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action,None).unwrap();
            let control=h.control.clone();let reply=h.drive(tokio::spawn(async move {control.request_authorized(action,source).await})).await.unwrap();
            let Reply::ScenesAccepted(runs)=reply else {panic!("expected timed production receipt")};let run=runs[0];h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
            let calls=Arc::new(Mutex::new(vec![]));let other_calls=Arc::new(Mutex::new(vec![]));
            let mut streams=std::collections::BTreeMap::from([("bus".into(),hb_transport::Connection::for_simulation(CountWrites {calls:calls.clone(),limit:usize::MAX,written:0,rx:Default::default()},1))]);
            if local {streams.insert("other".into(),hb_transport::Connection::for_simulation(CountWrites {calls:other_calls.clone(),limit:usize::MAX,written:0,rx:Default::default()},1));}
            let mut owner=TransportOwner::new(h.actor.clone(),streams);
            let connects=Arc::new(std::sync::atomic::AtomicUsize::new(0));let captured=connects.clone();
            let sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
            if local {owner.enable_local_io(Arc::new(move |_,_| {captured.fetch_add(1,Ordering::SeqCst);Err(std::io::ErrorKind::ConnectionRefused.into())})).unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);for _ in 0..2 {crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();}
            let replacement=Feedback::new(&h.configuration,ExecutionContext::Simulation,Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),1).unwrap();
            let feedback=std::mem::replace(&mut h.feedback,replacement);let mut observed=crate::observed_transport::ObservedTransport::new(owner,feedback);
            let (entered,started)=tokio::sync::oneshot::channel();let (release,wait)=std::sync::mpsc::channel();
            let blocker=tokio::task::spawn_blocking(move || {entered.send(()).unwrap();wait.recv_timeout(Duration::from_secs(10)).unwrap();});started.await.unwrap();
            let writer=tokio::spawn(async move {let result=observed.step().await;(observed,result)});
            let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();let selected_by=tokio::time::Instant::now()+Duration::from_secs(1);
            while !(5..=7).all(|id|store.command_status(h.actor.boot_id(),id).unwrap()==Some(homesense_storage::journal::Status::Writing)) {assert!(tokio::time::Instant::now()<selected_by,"writer did not select complete LED batch");tokio::time::sleep(Duration::from_millis(5)).await;}
            let connection=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
            if audit_fault {connection.execute_batch("CREATE TRIGGER reject_selected_projection BEFORE UPDATE OF status ON command_journal WHEN OLD.command_id>=5 AND OLD.status='writing' AND NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'synthetic selected projection cancellation failure'); END;").unwrap();}
            if expiry {tokio::time::sleep(Duration::from_millis(2200)).await;}else{tokens.revoke(credential).unwrap();}
            assert_eq!(h.actor.task_generation().await.unwrap(),h.generation,"expired wire lease must not expire the original finite result obligation");
            assert!(h.actor.control_batch_views(h.controller,h.generation.clone()).await.unwrap().is_empty(),"actor observes expiry/revocation before writer result arrives");
            release.send(()).unwrap();blocker.await.unwrap();let (mut observed,result)=writer.await.unwrap();
            if audit_fault {
                assert!(result.is_err());for id in 5..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Writing),"failed audit rolls back every cancellation");}
                assert!(h.actor.task_generation().await.is_err());connection.execute_batch("DROP TRIGGER reject_selected_projection").unwrap();
            } else {
                let cancelled=result.unwrap().unwrap();assert!(cancelled.projection_cancelled);assert_eq!(cancelled.result.unwrap_err().bytes_written,0);assert!(cancelled.completed.is_empty());
                for id in 5..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Cancelled));let reason=connection.query_row("SELECT cancellation_reason FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![h.actor.boot_id(),id as i64],|r|r.get::<_,String>(0)).unwrap();assert_eq!(reason,if expiry {"Expired"}else{"RunCancelled"});}
                h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();assert_eq!(h.scheduler.status(run),Some(homesense_core::scene::Status::Partial));
                assert_eq!(h.actor.task_generation().await.unwrap(),h.generation);
                let mut intents=vec![Intent {device:homesense_core::DeviceId::new("marker").unwrap(),command:homesense_core::Command::Switch(false)}];
                if local {intents.push(Intent {device:homesense_core::DeviceId::new("independent").unwrap(),command:homesense_core::Command::Switch(true)});}
                h.actor.submit(Batch {controller:h.controller,intents,subscriptions:vec![]}).await.unwrap();
                for _ in 0..if local {2}else{1} {let next=observed.step().await.unwrap().unwrap();next.result.unwrap();assert!(!next.projection_cancelled);}
                let mut expected=sizes.clone();expected.push(5);assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),expected);if local {assert_eq!(other_calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![5]);}assert_eq!(connects.load(Ordering::SeqCst),0);assert_eq!(h.actor.task_generation().await.unwrap(),h.generation);
            }
            drop(connection);drop(store);drop(observed);h.finish().await;
        }
    });
}

struct ProjectionIOFailure {
    calls:BatchWrites,
    written:usize,
    limit:usize,
    kind:std::io::ErrorKind,
    rx:std::collections::VecDeque<u8>,
}
impl std::io::Read for ProjectionIOFailure {fn read(&mut self,out:&mut[u8])->std::io::Result<usize>{
    if self.rx.is_empty(){return Err(std::io::ErrorKind::WouldBlock.into());}let n=out.len().min(self.rx.len());for byte in &mut out[..n]{*byte=self.rx.pop_front().unwrap();}Ok(n)
}}
impl std::io::Write for ProjectionIOFailure {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
        if self.written>=self.limit {return Err(self.kind.into());}
        let n=bytes.len().min(self.limit-self.written);self.calls.lock().unwrap().push(bytes[..n].to_vec());self.written+=n;
        for frame in bytes[..n].as_chunks::<5>().0 {if (1..=8).contains(&frame[2]){self.rx.extend([0xae,frame[1],0xd0+frame[2],frame[3],0xee]);}}
        Ok(n)
    }
    fn flush(&mut self)->std::io::Result<()>{panic!("unbuffered")}
}
#[tokio::test]
async fn physical_permission_denial_and_partial_led_io_are_not_projection_cancellations() {
    for (limit,kind,first_status) in [(20,std::io::ErrorKind::PermissionDenied,homesense_storage::journal::Status::Failed),(22,std::io::ErrorKind::BrokenPipe,homesense_storage::journal::Status::Uncertain)] {
        let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
        h.request(Action::SceneWrites {controller:h.member,revision:1,scenes:vec![("act".into(),Some(true))]}).await.unwrap();
        let calls=Arc::new(Mutex::new(vec![]));let mut owner=TransportOwner::new(h.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(ProjectionIOFailure {calls:calls.clone(),written:0,limit,kind,rx:Default::default()},1))].into());
        let mut sizes=write_lighting_prefix(&mut h,&mut owner,false).await;
        tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);for _ in 0..2 {crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();}
        let outcome=owner.step().await.unwrap().unwrap();assert!(!outcome.projection_cancelled);let error=outcome.result.unwrap_err();assert_eq!(error.bytes_written,limit-20);
        let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();assert_eq!(store.command_status(h.actor.boot_id(),5).unwrap(),Some(first_status));for id in 6..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Failed));}drop(store);
        assert!(owner.step().await.is_err(),"physical fault requires explicit recovery");assert!(h.actor.task_generation().await.is_err());
        if limit>20{sizes.push(2);}assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),sizes);h.finish().await;
    }
}

fn short_projection_batch(controller:Uuid)->homesense_hap::requests::Batch {
    use homesense_core::{Command,DeviceId,LevelTransition,Percent};
    use homesense_hap::requests::{Batch,Intent};
    Batch {controller,subscriptions:vec![],intents:vec![
        Intent {device:DeviceId::new("light").unwrap(),command:Command::SetLevel {level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate}},
        Intent {device:DeviceId::new("execute-led").unwrap(),command:Command::PanelLed(true)},
    ]}
}
#[tokio::test]
async fn selected_projection_with_lost_result_keeps_original_finite_watchdog_and_cannot_claim_late_success() {
    let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs(cfg,doc).await;
    let cap=h.actor.monotonic_ms()+1000;
    h.actor.submit_scene_with_feedback_before(h.generation.clone(),short_projection_batch(h.member),0,None,Some(cap)).await.unwrap();
    let (mut owner,_)=batch_owner(&h,usize::MAX);owner.step().await.unwrap().unwrap().result.unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,40);crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();
    let ticket=h.actor.dispatch().await.unwrap().unwrap();assert_eq!(ticket.command.deadline_ms,cap);let id=ticket.command.id;drop(ticket);
    tokio::time::sleep(Duration::from_millis(cap.saturating_add(30).saturating_sub(h.actor.monotonic_ms()))).await;
    assert!(h.actor.task_generation().await.is_err());let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Uncertain));drop(store);
    assert!(h.actor.finish_write(id,true).await.is_err(),"late claim cannot erase missing-result uncertainty");h.finish().await;
}
#[test]
fn returned_zero_projection_precedes_watchdog_only_when_complete_proof_is_already_queued() {
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
    runtime.block_on(async {
        use crate::{source_auth::Source,web_auth::{Grant,Role,Tokens}};
        let enabled=Arc::new(AtomicBool::new(false));let target=Arc::new(Mutex::new(None));
        let (entered,started)=tokio::sync::oneshot::channel();let entered=Arc::new(Mutex::new(Some(entered)));
        let (release_actor,actor_wait)=std::sync::mpsc::channel();let actor_wait=Arc::new(Mutex::new(actor_wait));
        let probe:AuthorizationProbe={let enabled=enabled.clone();let target=target.clone();Arc::new(move |id| {
            if *target.lock().unwrap()==Some(id) && enabled.swap(false,Ordering::SeqCst) {
                entered.lock().unwrap().take().unwrap().send(()).unwrap();actor_wait.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
            }
        })};
        let (cfg,doc)=writer_batch_inputs();let mut h=Harness::from_inputs_with_probe(cfg,doc,Some(probe)).await;*target.lock().unwrap()=Some(h.controller);
        let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();const SECRET:&str="synthetic-returned-proof-source-0123456789";
        tokens.insert(credential,SECRET,Grant {controller:h.member,role:Role::Member,scenes:["act".into()].into(),devices:h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect()}).unwrap();
        let action=Action::SceneWrites {controller:h.member,revision:1,scenes:vec![("act".into(),Some(true))]};let source=Source::capture(tokens.module_off_lease(Some(&format!("Bearer {SECRET}")),None,None).unwrap(),&action,None).unwrap();
        let scoped=h.actor.with_source(Some(source));let cap=h.actor.monotonic_ms()+1000;
        scoped.submit_scene_with_feedback_before(h.generation.clone(),short_projection_batch(h.member),0,None,Some(cap)).await.unwrap();
        let (mut owner,calls)=batch_owner(&h,usize::MAX);owner.step().await.unwrap().unwrap().result.unwrap();tokio::time::sleep(Duration::from_millis(2)).await;mode_frame(&mut h,0x15,1,40);crate::control_batch::tick(&h.actor,h.member,&h.feedback).await.unwrap();
        let (worker_entered,worker_started)=tokio::sync::oneshot::channel();let (release_worker,worker_wait)=std::sync::mpsc::channel();let blocker=tokio::task::spawn_blocking(move || {worker_entered.send(()).unwrap();worker_wait.recv_timeout(Duration::from_secs(10)).unwrap();});worker_started.await.unwrap();
        let (spawned,queued)=tokio::sync::oneshot::channel();owner.notify_next_worker_spawned(spawned);
        let writer=tokio::spawn(async move {let result=owner.step().await;(owner,result)});
        let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();let selected_by=tokio::time::Instant::now()+Duration::from_millis(500);
        while store.command_status(h.actor.boot_id(),2).unwrap()!=Some(homesense_storage::journal::Status::Writing) {assert!(tokio::time::Instant::now()<selected_by);tokio::time::sleep(Duration::from_millis(5)).await;}
        // Writing alone precedes tail collection. Stall the actor only after
        // the actual blocking I/O task has been queued behind the held worker.
        tokio::time::timeout_at(selected_by,queued).await.unwrap().unwrap();
        enabled.store(true,Ordering::Release);let actor=h.actor.clone();let controller=h.controller;let generation=h.generation.clone();let diagnostics=tokio::spawn(async move {actor.diagnostics(controller,generation).await});started.await.unwrap();
        tokens.revoke(credential).unwrap();release_worker.send(()).unwrap();blocker.await.unwrap();
        tokio::time::sleep(Duration::from_millis(cap.saturating_add(30).saturating_sub(h.actor.monotonic_ms()))).await;
        assert!(!writer.is_finished());assert!(!diagnostics.is_finished());release_actor.send(()).unwrap();diagnostics.await.unwrap().unwrap();
        let (mut owner,result)=writer.await.unwrap();let cancelled=result.unwrap().unwrap();assert!(cancelled.projection_cancelled);assert_eq!(cancelled.result.unwrap_err().bytes_written,0);assert_eq!(store.command_status(h.actor.boot_id(),2).unwrap(),Some(homesense_storage::journal::Status::Cancelled));assert_eq!(h.actor.task_generation().await.unwrap(),h.generation);
        h.actor.submit(homesense_hap::requests::Batch {controller:h.controller,subscriptions:vec![],intents:vec![homesense_hap::requests::Intent {device:homesense_core::DeviceId::new("marker").unwrap(),command:homesense_core::Command::Switch(true)}]}).await.unwrap();owner.step().await.unwrap().unwrap().result.unwrap();assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![5,5]);drop(store);h.finish().await;
    });
}

#[path = "scene_presentation_tests.rs"]
mod scene_presentation_tests;

#[path = "scene_component_activity_tests.rs"]
mod scene_component_activity_tests;

#[test]
fn component_confirmation_policies_do_not_install_permanent_short_poll_consumers() {
    let (bytes,value)=component_cycle_inputs();
    let config=Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
    let document=Document::parse(&serde_json::to_vec(&value).unwrap(),&config).unwrap();
    assert!(!document.component_mode_confirmations.is_empty());
    assert!(crate::consumer_refresh::requirements(&config,&document).unwrap().is_empty());
}

#[path = "hap_room_all_tests.rs"]
mod hap_room_all_tests;

#[path = "batch_collection_tests.rs"]
mod batch_collection_tests;

#[path = "batch_query_tests.rs"]
mod batch_query_tests;
