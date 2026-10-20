//! Actual actor/Feedback/Control/HAP/Web read paths; synthetic inputs, no socket.
use crate::{
    admission::{Admission, Options, TaskGeneration},
    automation_control::{Action, Control, Receiver, Reply},
    feedback::Feedback,
    scene_activity::SceneActivity,
    scene_scheduler::SceneScheduler,
    scene_tracking::{self, Projection},
    transport_owner::GatewayRead,
};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{DeviceId, ExecutionContext};
use homesense_storage::Store;
use serde_json::{Value as Json, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
fn artifact(name: &str, value: &Json) {
    if let Ok(root) = std::env::var("TRACKING_ARTIFACT_ROOT") {
        let path = std::path::Path::new(&root).join("synthetic");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
}
fn devices(combined: bool) -> Vec<u8> {
    let mut v = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"dim","address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dim","channel":1,"enabled":true},{"id":"relay","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true},{"id":"ignored","name":"Ignored synthetic light","module_id":"relay","channel":2,"enabled":true}]});
    if combined {
        v["devices"][0]["supply"] = json!({"module_id":"relay","channel":1});
        v["devices"][0]["default_on_level"] = 100.into();
    }
    serde_json::to_vec(&v).unwrap()
}
fn rules() -> Json {
    json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"scene","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic final target deliberately differs","offset_ms":0,"device":"light","command":{"kind":"level","value":100,"ramp":false}}]}],"scene_metadata":[{"id":"scene","name":"Friendly synthetic name"}],"scene_tracking":[{"scene":"scene","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic activity","max_age_ms":5000,"review_evidence":"explicit synthetic tracking","members":[{"id":"level","device":"light","predicate":{"kind":"level_range","minimum":20,"maximum":60},"review_evidence":"synthetic actual powered range"},{"id":"power","device":"relay","predicate":{"kind":"power","on":false},"review_evidence":"synthetic actual power"},{"id":"unused","device":"ignored","predicate":{"kind":"ignore","reason":"explicit irrelevant tracking member"},"review_evidence":"synthetic ignored"}]}]})
}

#[tokio::test]
async fn any_lit_scene_requires_both_shared_supply_and_dimmer_not_just_a_powered_relay() {
    let mut rules = rules();
    rules["scene_tracking"][0]["aggregation"] = json!("any");
    rules["scene_tracking"][0]["members"] = json!([
        {"id":"paired","device":"relay","predicate":{"kind":"powered_light","dimmer":"light"},"review_evidence":"synthetic shared channel"}
    ]);
    let mut h = Harness::new(false, rules).await;
    h.samples(None, Some(true));
    assert_eq!(h.project().await.activity, SceneActivity::Unknown, "supply On cannot guess a missing dimmer reading");
    for (level, supply, expected) in [(0,true,SceneActivity::Inactive),(100,false,SceneActivity::Inactive),(10,true,SceneActivity::Active),(100,true,SceneActivity::Active)] {
        h.samples(Some(level), Some(supply));
        let projection = h.project().await;
        assert_eq!(projection.activity, expected);
        assert_eq!(projection.members[0].on, Some(expected==SceneActivity::Active));
    }
    h.actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(h.directory).unwrap();
}
struct Harness {
    actor: Admission,
    configuration: Configuration,
    document: Document,
    feedback: Feedback,
    controller: Uuid,
    generation: TaskGeneration,
    authorized: Arc<AtomicBool>,
    control: Control,
    receiver: Receiver,
    scheduler: SceneScheduler,
    directory: std::path::PathBuf,
}
impl Harness {
    async fn new(combined: bool, rules: Json) -> Self {
        Self::from_inputs(devices(combined), rules).await
    }
    async fn from_inputs(data: Vec<u8>, rules: Json) -> Self {
        let directory = std::env::temp_dir().join(format!("reviewed-tracking-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
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
        let authorized = Arc::new(AtomicBool::new(true));
        let check = authorized.clone();
        let actor = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            move |id| id == controller && check.load(Ordering::Acquire),
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let generation = actor.task_generation().await.unwrap();
        let feedback = Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            generation.epochs.transport,
        )
        .unwrap();
        let (control, receiver) =
            crate::automation_control::channel(controller, generation.clone());
        let scheduler =
            SceneScheduler::new(actor.clone(), controller, 8, actor.monotonic_ms()).unwrap();
        Self {
            actor,
            configuration: bundle.configuration,
            document: bundle.automation.unwrap(),
            feedback,
            controller,
            generation,
            authorized,
            control,
            receiver,
            scheduler,
            directory,
        }
    }
    fn samples_at(&mut self, level: Option<u8>, relay: Option<bool>, at: u64) {
        let mut frames = vec![];
        if let Some(level) = level {
            frames.push(hb_protocol::RawFrame {
                address: 0x15,
                command: 0xd1,
                value: level,
            });
        }
        if let Some(on) = relay {
            frames.push(hb_protocol::RawFrame {
                address: 3,
                command: 0xd1,
                value: if on { 0xf1 } else { 0xf2 },
            });
        }
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames,
                },
            })
            .unwrap();
    }
    fn samples(&mut self, level: Option<u8>, relay: Option<bool>) {
        self.samples_at(level, relay, self.actor.monotonic_ms());
    }
    async fn project(&self) -> Projection {
        scene_tracking::project(
            &self.document.scene_tracking["scene"],
            &self.actor,
            self.controller,
            &self.generation,
            &self.feedback,
        )
        .await
    }
    async fn drive<T: Send + 'static>(&mut self, task: tokio::task::JoinHandle<T>) -> T {
        let end = tokio::time::Instant::now() + Duration::from_secs(2);
        while !task.is_finished() {
            assert!(tokio::time::Instant::now() < end, "read stalled/deadlocked");
            self.receiver
                .poll(
                    &self.actor,
                    &self.document,
                    &mut self.scheduler,
                    &self.feedback,
                    5000,
                    None,
                )
                .await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        task.await.unwrap()
    }
    async fn tracking(&mut self) -> Projection {
        let c = self.control.clone();
        let who = self.controller;
        match self
            .drive(tokio::spawn(async move {
                c.request(Action::SceneTracking {
                    controller: who,
                    revision: 1,
                    scene: "scene".into(),
                })
                .await
            }))
            .await
            .unwrap()
        {
            Reply::SceneTracking(Some(v)) => v,
            other => panic!("{other:?}"),
        }
    }
    fn commands(&self) -> i64 {
        rusqlite::Connection::open(self.directory.join("runtime.sqlite"))
            .unwrap()
            .query_row("select count(*) from command_journal", [], |r| r.get(0))
            .unwrap()
    }
    async fn finish(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
#[tokio::test]
async fn actual_deviation_ignore_and_explicit_all_any_unknown_policies() {
    let mut h = Harness::new(false, rules()).await;
    Store::open(h.directory.join("runtime.sqlite"))
        .unwrap()
        .remember_light_levels(
            1,
            &[(
                DeviceId::new("light").unwrap(),
                homesense_core::Percent::new(77).unwrap(),
            )],
        )
        .unwrap();
    h.samples(Some(35), Some(false));
    let truth = h.feedback.module_truth().module_state("dim", 1).unwrap();
    assert_eq!(
        truth.observed,
        Some(homesense_core::Observation::Level(
            homesense_core::Percent::new(35).unwrap()
        ))
    );
    let physical_before = h.feedback.snapshot();
    let p = h.tracking().await;
    assert_eq!(
        h.feedback.snapshot(),
        physical_before,
        "tracking only reads canonical truth"
    );
    assert_eq!(p.activity, SceneActivity::Active);
    assert_eq!(p.members[0].actual_level, Some(35));
    assert!(p.members[2].ignored);
    assert_eq!(p.display_name.as_deref(), Some("Friendly synthetic name"));
    assert!(p.action_available);
    assert!(
        !p.activity_writable,
        "activity is read-only while the original action remains available"
    );
    h.samples(Some(35), Some(true));
    assert_eq!(h.project().await.activity, SceneActivity::Inactive);
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .aggregation = homesense_config::scene_tracking::Aggregation::Any;
    assert_eq!(h.project().await.activity, SceneActivity::Active);
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    h.samples(Some(35), None);
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .unknown_policy = homesense_config::scene_tracking::UnknownPolicy::DecisiveKnown;
    assert_eq!(h.project().await.activity, SceneActivity::Active);
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    h.samples(None, Some(true));
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .aggregation = homesense_config::scene_tracking::Aggregation::All;
    assert_eq!(
        h.project().await.activity,
        SceneActivity::Inactive,
        "decisive known false in All does not invent missing brightness"
    );
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .unknown_policy = homesense_config::scene_tracking::UnknownPolicy::RequireAllKnown;
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    assert_eq!(h.commands(), 0);
    assert!(h.actor.dispatch().await.unwrap().is_none());
    h.finish().await;
}
#[tokio::test]
async fn combined_dark_light_never_uses_recorded_or_physical_brightness_as_active_input() {
    let mut r = rules();
    r["scene_tracking"][0]["members"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    let mut h = Harness::new(true, r).await;
    h.samples(Some(35), Some(false));
    let p = h.project().await;
    assert_eq!(p.activity, SceneActivity::Inactive);
    assert_eq!(p.members[0].on, Some(false));
    assert_eq!(p.members[0].actual_level, None);
    h.samples(Some(35), Some(true));
    assert_eq!(h.project().await.activity, SceneActivity::Active);
    assert_eq!(h.commands(), 0);
    h.finish().await;
}
#[tokio::test]
async fn unknown_missing_stale_future_boot_epoch_and_generation_fail_closed() {
    let mut h = Harness::new(false, rules()).await;
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.samples(Some(35), Some(false));
    assert_eq!(h.project().await.activity, SceneActivity::Active);
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .max_age_ms = 1;
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .max_age_ms = 5000;
    let original = h.generation.clone();
    let foreign = Harness::new(false, rules()).await;
    h.generation = foreign.generation.clone();
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    foreign.finish().await;
    for mode in 0..3 {
        h.generation = original.clone();
        match mode {
            0 => h.generation.epochs.transport += 1,
            1 => h.generation.epochs.configuration += 1,
            _ => h.generation.epochs.control += 1,
        };
        assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    }
    h.generation = original;
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::new_v4().as_u128(),
        1,
    )
    .unwrap();
    h.samples(Some(35), Some(false));
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    h.samples_at(Some(35), Some(false), h.actor.monotonic_ms() + 1000);
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    assert_eq!(h.commands(), 0);
    h.finish().await;
}
#[tokio::test]
async fn pending_member_ignored_pending_disconnect_revoke_and_publication_are_not_actual_activity()
{
    use homesense_core::Command;
    use homesense_hap::requests::{Batch, Intent};
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    h.actor
        .submit(Batch {
            controller: h.controller,
            intents: vec![Intent {
                device: DeviceId::new("ignored").unwrap(),
                command: Command::Switch(true),
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    assert_eq!(h.project().await.activity, SceneActivity::Active);
    assert_eq!(h.commands(), 1);
    h.actor
        .submit(Batch {
            controller: h.controller,
            intents: vec![Intent {
                device: DeviceId::new("relay").unwrap(),
                command: Command::Switch(true),
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    assert_eq!(h.project().await.activity, SceneActivity::Pending);
    assert_eq!(h.commands(), 2, "tracking adds no commands");
    h.finish().await;
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    h.authorized.store(false, Ordering::Release);
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.authorized.store(true, Ordering::Release);
    h.feedback.invalidate().unwrap();
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    h.actor.transport_failed().await.unwrap();
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    assert_eq!(h.commands(), 0);
    h.finish().await;
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let mut cfg: Json = serde_json::from_slice(&devices(false)).unwrap();
    cfg["revision"] = 2.into();
    let mut next = rules();
    next["configuration_revision"] = 2.into();
    h.actor
        .begin_automation_publication(
            serde_json::to_vec(&cfg).unwrap(),
            serde_json::to_vec(&next).unwrap(),
            1,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(h.project().await.activity, SceneActivity::Unknown);
    assert_eq!(h.commands(), 0);
    h.actor.abort_publication().await.unwrap();
    h.finish().await;
}
async fn hap_read(h: &mut Harness, handle: hap::server::AccessoryHandle) -> Result<Json, String> {
    h.drive(tokio::spawn(async move {
        let mut a = handle.lock().await;
        a.get_mut_service(hap::HapType::Switch)
            .unwrap()
            .get_mut_characteristic(hap::HapType::PowerState)
            .unwrap()
            .get_value()
            .await
            .map_err(|e| e.to_string())
    }))
    .await
}

#[tokio::test]
async fn app_only_activity_keeps_the_existing_hap_action_without_allocating_an_extra_accessory() {
    use homesense_hap::{scene_graph::SceneSpec, storage::IdentityStore};
    let mut rules=rules();rules["scene_tracking"][0]["hap_activity"]=json!(false);
    let mut h=Harness::new(false,rules).await;
    h.samples(Some(35),Some(false));
    assert_eq!(h.project().await.activity,SceneActivity::Active);
    let mut identity=IdentityStore::open(h.directory.join("identity")).await.unwrap();
    hap::storage::Storage::save_config(&mut identity,&hap::Config::default()).await.unwrap();
    let bridge=crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,&h.document,vec![SceneSpec{id:"scene".into(),name:"全部".into()}],
        h.control.clone(),h.actor.clone(),h.controller,|_,_|true).await.unwrap();
    assert_eq!(bridge.accessories.len(),1);
    assert!(bridge.activities.is_empty());
    assert_eq!(h.commands(),0);
    drop(bridge);h.finish().await;
}
#[tokio::test]
async fn actual_tracking_flows_through_existing_hap_read_and_scoped_web_api_without_writes() {
    use crate::web_auth::{Grant, Role, Tokens};
    use homesense_hap::{scene_graph::SceneSpec, storage::IdentityStore};
    use http_body_util::BodyExt;
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let mut identity = IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![SceneSpec {
            id: "scene".into(),
            name: "ID is not a display name".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    let handle = bridge.activities[0].1.clone();
    assert_eq!(bridge.activities[0].0.scene, "scene");
    {
        let mut accessory = handle.lock().await;
        let name = accessory
            .get_mut_service(hap::HapType::AccessoryInformation)
            .unwrap()
            .get_mut_characteristic(hap::HapType::Name)
            .unwrap()
            .get_value()
            .await
            .unwrap();
        assert_eq!(name, json!("Synthetic activity"));
        drop(accessory);
        let mut action = bridge.accessories[0].1.lock().await;
        assert_eq!(
            action
                .get_mut_service(hap::HapType::AccessoryInformation)
                .unwrap()
                .get_mut_characteristic(hap::HapType::Name)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            json!("Friendly synthetic name")
        );
    }
    assert_eq!(hap_read(&mut h, handle.clone()).await.unwrap(), json!(true));
    h.samples(Some(80), Some(false));
    assert_eq!(
        hap_read(&mut h, handle.clone()).await.unwrap(),
        json!(false)
    );
    h.samples(Some(35), Some(false));
    let tokens = Arc::new(Tokens::default());
    let secret = "synthetic-tracking-test-secret-abcdefghijklmnopqrstuvwxyz";
    tokens
        .insert(
            Uuid::new_v4(),
            secret,
            Grant {
                controller: h.controller,
                role: Role::Member,
                scenes: ["scene".into()].into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    let api = Arc::new(crate::web_api::Api {
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
            scenes: [("scene".into(), "raw control ID".into())].into(),
        })),
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("receipts.sqlite")).unwrap(),
        ),
        drafts: None,
    });
    let request = hyper::Request::builder()
        .uri("/api/v1/scenes")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {secret}"))
        .body(hyper::body::Bytes::new())
        .unwrap();
    let cloned = api.clone();
    let response = h
        .drive(tokio::spawn(async move { cloned.handle(request).await }))
        .await;
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let body: Json =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    artifact("web-actual-tracking.json", &body);
    let scene = &body["scenes"][0];
    assert_eq!(scene["id"], "scene");
    assert_eq!(scene["name"], "Friendly synthetic name");
    assert_eq!(scene["activity"], "Active");
    assert_eq!(scene["activity_source"], "reviewed_tracking");
    assert_eq!(scene["switch_kind"], "momentary");
    assert_eq!(scene["activity_kind"], "read_only");
    assert_eq!(scene["can_control"], true);
    assert_eq!(scene["tracking"]["activity_writable"], false);
    assert!(
        scene["selected"].is_null(),
        "actual activity is not local selection intent"
    );
    assert_eq!(scene["tracking"]["members"][0]["actual_level"], 35);
    h.authorized.store(false, Ordering::Release);
    assert!(hap_read(&mut h, handle.clone()).await.is_err());
    h.authorized.store(true, Ordering::Release);
    h.feedback.invalidate().unwrap();
    assert!(hap_read(&mut h, handle).await.is_err());
    assert_eq!(h.commands(), 0);
    assert!(h.actor.dispatch().await.unwrap().is_none());
    drop(bridge);
    drop(api);
    drop(identity);
    h.finish().await;
}
#[path = "../../homesense-import/tests/support/reviewed_scene.rs"]
mod converter_fixture;
#[tokio::test]
async fn reviewed_converter_policy_reaches_runtime_and_hap_without_inventing_raw_code_meanings() {
    use homesense_config::scene_tracking::{Aggregation, PendingPolicy, Predicate, UnknownPolicy};
    use homesense_import::reviewed_scenes::{
        Disposition, TrackingContract, TrackingMember, convert,
    };
    let scene = converter_fixture::scene(
        50100,
        "unresolved",
        &[(2000, 100, 10, 0), (2001, 0, 10, 0), (2002, 0, 10, 0)],
    )
    .replacen("synthetic-no-tracking", "0", 1)
    .replace("synthetic-no-tracking", "4");
    let source = converter_fixture::project(&scene, "", "", true);
    let config = converter_fixture::config(true);
    let mut review = converter_fixture::review(&source);
    let c = &mut review.scenes[0];
    c.tracking = Some(TrackingContract {
        aggregation: Aggregation::All,
        unknown_policy: UnknownPolicy::RequireAllKnown,
        pending_policy: PendingPolicy::BlockScene,
        control_policy:
            homesense_config::scene_tracking::ControlPolicy::SeparateActionAndReadOnlyActivity,
        activity_name: "Synthetic activity".into(),
        max_age_ms: 5000,
        review_evidence: "synthetic explicit policy unrelated to undocumented original codes"
            .into(),
        members: vec![
            TrackingMember {
                source_member_index: 0,
                predicate: Predicate::LevelRange {
                    minimum: 20,
                    maximum: 60,
                },
                review_evidence: "explicit actual brightness range, not raw track_lvl meaning"
                    .into(),
            },
            TrackingMember {
                source_member_index: 1,
                predicate: Predicate::Power { on: false },
                review_evidence: "explicit actual power".into(),
            },
            TrackingMember {
                source_member_index: 2,
                predicate: Predicate::Ignore {
                    reason: "explicit excluded member".into(),
                },
                review_evidence: "explicit irrelevant member".into(),
            },
        ],
    });
    for (path, field) in &mut c.fields {
        if path.ends_with("track_mode[0]") || path.ends_with("track_lvl[0]") {
            field.disposition = Disposition::Tracking;
        }
    }
    let bundle = convert(&source, &config, &review).unwrap();
    artifact(
        "converter-disabled-tracking-bundle.json",
        &serde_json::to_value(&bundle).unwrap(),
    );
    assert_eq!(bundle.scenes[0].status, "converted_disabled");
    assert_eq!(bundle.document["scene_tracking"][0]["enabled"], false);
    assert_eq!(bundle.document["timed_scenes"][0]["enabled"], false);
    assert_eq!(bundle.document["rules"], json!([]));
    let mut rules = bundle.document;
    rules["timed_scenes"][0]["enabled"] = true.into();
    rules["scene_tracking"][0]["enabled"] = true.into();
    let id = "control4:scene:7:50100";
    let mut h = Harness::from_inputs(converter_fixture::config_bytes(true), rules).await;
    let at = h.actor.monotonic_ms();
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x20,
                        command: 0xd1,
                        value: 35,
                    },
                    hb_protocol::RawFrame {
                        address: 0x10,
                        command: 0xd2,
                        value: 0xf2,
                    },
                ],
            },
        })
        .unwrap();
    let value = scene_tracking::read(
        &h.document,
        id,
        &h.actor,
        h.controller,
        &h.generation,
        &h.feedback,
    )
    .await
    .unwrap()
    .unwrap();
    artifact(
        "converter-runtime-actual-tracking.json",
        &serde_json::to_value(&value).unwrap(),
    );
    assert_eq!(value.activity, SceneActivity::Active);
    assert_eq!(value.members[0].actual_level, Some(35));
    assert!(value.members[2].ignored);
    assert_eq!(
        h.document.timed_scenes[id].steps[0].command,
        homesense_core::Command::SetLevel {
            level: homesense_core::Percent::new(100).unwrap(),
            transition: homesense_core::LevelTransition::Immediate
        }
    );
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![homesense_hap::scene_graph::SceneSpec {
            id: id.into(),
            name: "control identifier is not a label".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    assert_eq!(
        hap_read(&mut h, bridge.activities[0].1.clone())
            .await
            .unwrap(),
        json!(true)
    );
    let at = h.actor.monotonic_ms();
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x20,
                    command: 0xd1,
                    value: 80,
                }],
            },
        })
        .unwrap();
    assert_eq!(
        hap_read(&mut h, bridge.activities[0].1.clone())
            .await
            .unwrap(),
        json!(false),
        "archived is_active=True and final target=100 do not override actual tracking"
    );
    assert_eq!(h.commands(), 0);
    assert!(h.actor.dispatch().await.unwrap().is_none());
    drop(bridge);
    drop(identity);
    h.finish().await;
}
#[tokio::test]
async fn immediate_policy_observation_and_separate_action_are_both_publishable()
{
    let mut r = rules();
    r["timed_scenes"] = json!([]);
    r["immediate_scenes"] = json!([{"id":"scene","enabled":true,"review_evidence":"synthetic","activate":[{"source":"synthetic","action":{"kind":"command","device":"light","command":{"kind":"level","value":100,"ramp":false}}}]}]);
    let mut h = Harness::new(false, r).await;
    h.samples(Some(35), Some(false));
    let p = h.tracking().await;
    assert_eq!(p.activity, SceneActivity::Active);
    assert!(p.action_available);
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![homesense_hap::scene_graph::SceneSpec {
            id: "scene".into(),
            name: "Synthetic immediate".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    assert_eq!(bridge.accessories.len(),1);
    assert_eq!(bridge.activities.len(),1);
    drop(bridge);
    assert_eq!(h.commands(), 0);
    drop(identity);
    h.finish().await;
}
#[tokio::test]
async fn existing_scene_observation_subscription_projects_tracking_and_invalidates_on_revocation() {
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let c = h.control.clone();
    let stream = h
        .drive(tokio::spawn(
            async move { c.subscribe("scene".into()).await },
        ))
        .await
        .unwrap();
    {
        let initial = stream.borrow();
        let observed = initial.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(observed.activity, SceneActivity::Active);
        assert!(observed.last_run.is_none());
        assert_eq!(observed.boot, h.actor.boot_id());
    }
    h.samples(Some(80), Some(false));
    tokio::time::sleep(Duration::from_millis(101)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    assert_eq!(
        stream.borrow().as_ref().unwrap().as_ref().unwrap().activity,
        SceneActivity::Inactive
    );
    h.authorized.store(false, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(101)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    assert!(stream.borrow().as_ref().unwrap().is_err());
    assert_eq!(h.commands(), 0);
    drop(stream);
    h.finish().await;
}
#[tokio::test]
async fn read_true_cannot_accept_false_and_real_subscriptions_notify_changes_unavailable_and_recovery()
 {
    use hap::server::{EventEmitter, ServerEvent};
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let mut bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![homesense_hap::scene_graph::SceneSpec {
            id: "scene".into(),
            name: "Synthetic activity".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    let handle = bridge.activities[0].1.clone();
    let binding = bridge.activities[0].0.clone();
    let events = Arc::new(std::sync::Mutex::new(Vec::<Json>::new()));
    let out = events.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |e| {
        if let ServerEvent::CharacteristicValueChanged { aid, iid, value } = e
            && *aid == binding.aid
            && *iid == binding.on_iid
        {
            out.lock().unwrap().push(value.clone());
        }
        Box::pin(async {})
    }));
    {
        let mut a = handle.lock().await;
        a.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
            emitter,
        ))));
        let c = a
            .get_mut_service(hap::HapType::Switch)
            .unwrap()
            .get_mut_characteristic(hap::HapType::PowerState)
            .unwrap();
        assert!(
            !c.get_perms()
                .contains(&hap::characteristic::Perm::PairedWrite)
        );
        assert!(c.get_perms().contains(&hap::characteristic::Perm::Events));
        c.set_event_notifications(Some(true));
    }
    assert_eq!(hap_read(&mut h, handle.clone()).await.unwrap(), json!(true));
    for value in [false, true] {
        let body = serde_json::to_vec(
            &json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":value}]}),
        )
        .unwrap();
        assert_eq!(
            bridge
                .activity_write_handler
                .as_ref()
                .unwrap()
                .write_async(h.controller, &body)
                .await,
            Err(hap::server::WriteStatus::ReadOnlyCharacteristic)
        );
    }
    let c = h.control.clone();
    let who = h.controller;
    let rejected = h
        .drive(tokio::spawn(async move {
            c.request(Action::SceneActivityWrites {
                controller: who,
                revision: 1,
                scenes: vec![("scene".into(), Some(false))],
            })
            .await
        }))
        .await;
    assert!(rejected.unwrap_err().contains("read-only"));
    artifact(
        "readonly-write-rejection.json",
        &json!({"initial_hap_read":true,"hap_false_write":"ReadOnlyCharacteristic","hap_true_write":"ReadOnlyCharacteristic","same_admission_path_false":"rejected_read_only","extra_commands":h.commands()}),
    );
    assert_eq!(h.commands(), 0);
    let handler = bridge.activity_write_handler.as_ref().unwrap().clone();
    let who = h.controller;
    let body = serde_json::to_vec(
        &json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"ev":true}]}),
    )
    .unwrap();
    h.drive(tokio::spawn(async move {
        handler.write_async(who, &body).await
    }))
    .await
    .unwrap();
    // Notifications below are produced from the live SceneObservation stream;
    // there is no HAP GET between actual feedback changes and delivered events.
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(true)));
    h.samples(Some(80), Some(false));
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(false)));
    h.feedback.invalidate().unwrap();
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&Json::Null));
    let count = events.lock().unwrap().len();
    bridge.flush_resets().await.unwrap();
    assert_eq!(
        events.lock().unwrap().len(),
        count,
        "unchanged unknown is deduplicated"
    );
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    h.samples(Some(35), Some(false));
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(true)));
    h.authorized.store(false, Ordering::Release);
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&Json::Null));
    h.authorized.store(true, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(true)));
    artifact(
        "hap-subscription-events.json",
        &json!({"events":*events.lock().unwrap(),"get_between_notifications":false,"extra_commands":h.commands()}),
    );
    assert_eq!(h.commands(), 0);
    assert!(h.actor.dispatch().await.unwrap().is_none());
    drop(bridge);
    drop(identity);
    h.finish().await;
}
#[tokio::test]
async fn subscription_expires_without_worker_refresh_and_publication_invalidates_without_get() {
    use hap::server::{EventEmitter, ServerEvent};
    let mut rules = rules();
    rules["scene_tracking"][0]["max_age_ms"] = 200.into();
    let mut h = Harness::new(false, rules).await;
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let mut bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![homesense_hap::scene_graph::SceneSpec {
            id: "scene".into(),
            name: "Synthetic activity".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::<Json>::new()));
    let out = events.clone();
    let mut emitter = EventEmitter::new();
    emitter.add_listener(Box::new(move |e| {
        if let ServerEvent::CharacteristicValueChanged { value, .. } = e {
            out.lock().unwrap().push(value.clone());
        }
        Box::pin(async {})
    }));
    bridge.activities[0]
        .1
        .lock()
        .await
        .set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
            emitter,
        ))));
    h.samples(Some(35), Some(false));
    let c = h.control.clone();
    let who = h.controller;
    h.drive(tokio::spawn(async move {
        c.request(Action::SceneTracking {
            controller: who,
            revision: 1,
            scene: "scene".into(),
        })
        .await
    }))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(true)));
    tokio::time::sleep(Duration::from_millis(201)).await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(
        events.lock().unwrap().last(),
        Some(&Json::Null),
        "expired stream cannot replay cached Active when worker stalls"
    );
    h.samples(Some(35), Some(false));
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&json!(true)));
    let mut cfg: Json = serde_json::from_slice(&devices(false)).unwrap();
    cfg["revision"] = 2.into();
    let mut next = crate::scene_tracking_tests::rules();
    next["configuration_revision"] = 2.into();
    h.actor
        .begin_automation_publication(
            serde_json::to_vec(&cfg).unwrap(),
            serde_json::to_vec(&next).unwrap(),
            1,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    bridge.flush_resets().await.unwrap();
    assert_eq!(events.lock().unwrap().last(), Some(&Json::Null));
    h.actor.abort_publication().await.unwrap();
    assert_eq!(h.commands(), 0);
    artifact(
        "subscription-expiry-publication-events.json",
        &json!({"events":*events.lock().unwrap(),"no_get":true,"no_worker_refresh_during_expiry":true,"extra_commands":0}),
    );
    drop(bridge);
    drop(identity);
    h.finish().await;
}
struct MemoryWire(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Read for MemoryWire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for MemoryWire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn original_action_identity_executes_while_separate_activity_identity_cannot_actuate_or_cross_roles()
 {
    use homesense_hap::{
        scene_graph::{SceneGraph, SceneSpec},
        storage::IdentityStore,
    };
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let mut identity = IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let old = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "scene".into(),
            name: "Original action".into(),
        }],
    )
    .await
    .unwrap()
    .bindings[0]
        .clone();
    let allowed = Arc::new(AtomicBool::new(true));
    let authorize = allowed.clone();
    let bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![SceneSpec {
            id: "scene".into(),
            name: "Ignored fallback".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        move |_, id| id == "scene" && authorize.load(Ordering::Acquire),
    )
    .await
    .unwrap();
    let action = bridge.accessories[0].0.clone();
    let activity = bridge.activities[0].0.clone();
    assert_eq!(action, old);
    assert_ne!(action.aid, activity.aid);
    assert_eq!(activity.scene, action.scene);
    assert_eq!(
        hap_read(&mut h, bridge.accessories[0].1.clone())
            .await
            .unwrap(),
        json!(false)
    );
    assert_eq!(
        hap_read(&mut h, bridge.activities[0].1.clone())
            .await
            .unwrap(),
        json!(true)
    );
    let other = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "scene/activity".into(),
            name: "Actual independent action with a suffix-like ID".into(),
        }],
    )
    .await
    .unwrap()
    .bindings[0]
        .clone();
    assert!(other.aid != action.aid && other.aid != activity.aid);
    let body = |aid, iid, value| {
        serde_json::to_vec(&json!({"characteristics":[{"aid":aid,"iid":iid,"value":value}]}))
            .unwrap()
    };
    let state_write = body(activity.aid, activity.on_iid, false);
    assert_eq!(
        bridge
            .activity_write_handler
            .as_ref()
            .unwrap()
            .write_async(h.controller, &state_write)
            .await,
        Err(hap::server::WriteStatus::ReadOnlyCharacteristic)
    );
    assert_eq!(
        bridge
            .write_handler
            .write_async(h.controller, &state_write)
            .await,
        Err(hap::server::WriteStatus::ResourceDoesNotExist)
    );
    let action_write = body(action.aid, action.on_iid, true);
    assert_eq!(
        bridge
            .activity_write_handler
            .as_ref()
            .unwrap()
            .write_async(h.controller, &action_write)
            .await,
        Err(hap::server::WriteStatus::ResourceDoesNotExist)
    );
    allowed.store(false, Ordering::Release);
    assert_eq!(
        bridge
            .write_handler
            .write_async(h.controller, &action_write)
            .await,
        Err(hap::server::WriteStatus::InsufficientPrivileges)
    );
    assert_eq!(
        bridge
            .activity_write_handler
            .as_ref()
            .unwrap()
            .write_async(h.controller, &state_write)
            .await,
        Err(hap::server::WriteStatus::InsufficientPrivileges)
    );
    allowed.store(true, Ordering::Release);
    assert_eq!(h.commands(), 0);
    let handler = bridge.write_handler.clone();
    let who = h.controller;
    h.drive(tokio::spawn(async move {
        handler.write_async(who, &action_write).await
    }))
    .await
    .unwrap();
    h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
    let bytes = Arc::new(std::sync::Mutex::new(vec![]));
    let mut owner = crate::transport_owner::TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(MemoryWire(bytes.clone()), 1),
        )]
        .into(),
    );
    let output = owner.step().await.unwrap().unwrap();
    output.result.unwrap();
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(*bytes.lock().unwrap(), [0xae, 0x15, 1, 100, 0xee]);
    assert_eq!(h.commands(), 1);
    assert_eq!(
        h.project().await.activity,
        SceneActivity::Pending,
        "Written target is not actual feedback; post-write observation is still required"
    );
    let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let definitions = sql
        .prepare("SELECT definition FROM scene_run ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(definitions, ["scene"]);
    drop(sql);
    h.document
        .scene_metadata
        .insert("scene".into(), "Renamed action".into());
    h.document
        .scene_tracking
        .get_mut("scene")
        .unwrap()
        .activity_name = "Renamed activity".into();
    let renamed = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![SceneSpec {
            id: "scene".into(),
            name: "fallback".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    assert_eq!(renamed.accessories[0].0, action);
    assert_eq!(renamed.activities[0].0, activity);
    artifact(
        "separate-role-action-wire.json",
        &json!({"action_aid":action.aid,"action_iid":action.on_iid,"activity_aid":activity.aid,"activity_iid":activity.on_iid,"canonical_scene":action.scene,"renaming_preserves_both_identities":true,"suffix_like_real_scene_has_distinct_identity":true,"wire":*bytes.lock().unwrap(),"activity_writes_accepted":0,"action_commands":1,"definitions":definitions}),
    );
    drop(renamed);
    drop(bridge);
    drop(identity);
    h.finish().await;
}
use crate::hap_test_support as paired_wire_support;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_localhost_hap_event_wire_delivers_activity_unavailable_and_recovery_without_get_refresh()
 {
    use hap::{
        pairing::{Pairing, Permissions},
        server::{AccessoryHandle, IpServer, Server},
        storage::Storage,
    };
    use std::net::{TcpListener, TcpStream};
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let mut identity =
        homesense_hap::storage::IdentityStore::open(h.directory.join("wire-identity"))
            .await
            .unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let lights =
        homesense_hap::graph::LightGraph::build(&identity, "Synthetic wire bridge", vec![])
            .await
            .unwrap();
    let who = h.controller;
    let mut bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        vec![homesense_hap::scene_graph::SceneSpec {
            id: "scene".into(),
            name: "Synthetic action".into(),
        }],
        h.control.clone(),
        h.actor.clone(),
        who,
        move |peer, id| peer == who && id == "scene",
    )
    .await
    .unwrap();
    let action = bridge.accessories[0].0.clone();
    let activity = bridge.activities[0].0.clone();
    let server = IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    let mut handles: Vec<AccessoryHandle> = vec![Arc::new(hap::futures::lock::Mutex::new(
        Box::new(lights.bridge),
    ))];
    handles.extend(bridge.accessories.iter().map(|(_, h)| h.clone()));
    handles.extend(bridge.activities.iter().map(|(_, h)| h.clone()));
    let router = homesense_hap::write_router::WriteRouter::new(vec![
        (
            vec![(action.aid, action.on_iid)],
            bridge.write_handler.clone(),
        ),
        (
            vec![(activity.aid, activity.on_iid)],
            bridge.activity_write_handler.as_ref().unwrap().clone(),
        ),
    ])
    .unwrap();
    let router: Arc<dyn hap::server::WriteBatchHandler> = Arc::new(router);
    server
        .install_startup_graph(handles.clone(), router.clone())
        .await
        .unwrap();
    drop(server);
    let signing = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
    identity
        .save_pairing(&Pairing::new(
            who,
            Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let server = IpServer::new_loopback(identity.load_config().await.unwrap(), identity.clone())
        .await
        .unwrap();
    server.install_startup_graph(handles, router).await.unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<usize>();
    let received = Arc::new(std::sync::Mutex::new(Vec::<Json>::new()));
    let messages = received.clone();
    let client = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match TcpStream::connect(address) {
                Ok(s) => break s,
                Err(e) => {
                    assert!(std::time::Instant::now() < deadline, "{e}");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut session =
            paired_wire_support::verify(&mut stream, who, &signing, &accessory_id, &accessory_key);
        let request = format!(
            "GET /characteristics?id={}.{},{}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            activity.aid, activity.on_iid, action.aid, action.on_iid
        );
        let (header, body) = session.request(&mut stream, request.as_bytes());
        assert!(header.starts_with("HTTP/1.1 200"), "{header}");
        let values: Json = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            values["characteristics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["aid"] == activity.aid)
                .unwrap()["value"],
            true
        );
        assert_eq!(
            values["characteristics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["aid"] == action.aid)
                .unwrap()["value"],
            false
        );
        let false_write =
            json!({"characteristics":[{"aid":activity.aid,"iid":activity.on_iid,"value":false}]})
                .to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{false_write}",
            false_write.len()
        );
        let (header, body) = session.request(&mut stream, request.as_bytes());
        assert!(header.starts_with("HTTP/1.1 207"), "{header}");
        let denied: Json = serde_json::from_slice(&body).unwrap();
        assert_eq!(denied["characteristics"][0]["status"], -70404);
        let sub = json!({"characteristics":[{"aid":activity.aid,"iid":activity.on_iid,"ev":true}]})
            .to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{sub}",
            sub.len()
        );
        let (header, _) = session.request(&mut stream, request.as_bytes());
        assert!(header.starts_with("HTTP/1.1 204"), "{header}");
        send.send(0).unwrap();
        for (i, expected) in [json!(true), json!(false), Json::Null, json!(true)]
            .into_iter()
            .enumerate()
        {
            let (header, body) = session.request(&mut stream, b"");
            assert!(header.starts_with("EVENT/1.0 200 OK"), "{header}");
            let event: Json = serde_json::from_slice(&body).unwrap();
            assert_eq!(event["characteristics"][0]["aid"], activity.aid);
            assert_eq!(event["characteristics"][0]["iid"], activity.on_iid);
            assert_eq!(event["characteristics"][0]["value"], expected);
            messages
                .lock()
                .unwrap()
                .push(json!({"header":header,"event":event}));
            send.send(i + 1).unwrap();
        }
    });
    async fn ack(
        h: &mut Harness,
        receive: &mut tokio::sync::mpsc::UnboundedReceiver<usize>,
        expected: usize,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(index) = receive.try_recv() {
                assert_eq!(index, expected);
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "wire stage {expected} stalled"
            );
            h.receiver
                .poll(
                    &h.actor,
                    &h.document,
                    &mut h.scheduler,
                    &h.feedback,
                    5000,
                    None,
                )
                .await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    ack(&mut h, &mut receive, 0).await;
    bridge.flush_resets().await.unwrap();
    ack(&mut h, &mut receive, 1).await;
    h.samples(Some(80), Some(false));
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    ack(&mut h, &mut receive, 2).await;
    h.feedback.invalidate().unwrap();
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    ack(&mut h, &mut receive, 3).await;
    h.feedback = Feedback::new(
        &h.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    h.samples(Some(35), Some(false));
    tokio::time::sleep(Duration::from_millis(105)).await;
    h.receiver
        .poll(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
        )
        .await;
    bridge.flush_resets().await.unwrap();
    ack(&mut h, &mut receive, 4).await;
    client.await.unwrap();
    assert_eq!(h.commands(), 0);
    assert!(h.actor.dispatch().await.unwrap().is_none());
    artifact(
        "encrypted-hap-event-wire.json",
        &json!({"synthetic_localhost":true,"pair_verify_completed":true,"get_requests_after_subscribe":0,"state_false_write_status":-70404,"wire":*received.lock().unwrap(),"read_side_commands":0,"physical_field_contact":false}),
    );
    task.abort();
    let _ = task.await;
    drop(server);
    drop(bridge);
    drop(identity);
    h.finish().await;
}
#[tokio::test]
async fn web_exposes_readonly_activity_and_keeps_independent_authorized_action_executable() {
    use crate::web_auth::{Grant, Role, Tokens};
    use http_body_util::BodyExt;
    let mut h = Harness::new(false, rules()).await;
    h.samples(Some(35), Some(false));
    let secret = "synthetic-scene-action-secret-abcdefghijklmnopqrstuvwxyz";
    let viewer = "synthetic-scene-viewer-secret-abcdefghijklmnopqrstuvwxyz";
    let tokens = Arc::new(Tokens::default());
    for (secret, role) in [(secret, Role::Member), (viewer, Role::Viewer)] {
        tokens
            .insert(
                Uuid::new_v4(),
                secret,
                Grant {
                    controller: h.controller,
                    role,
                    scenes: ["scene".into()].into(),
                    devices: Default::default(),
                },
            )
            .unwrap();
    }
    let api = Arc::new(crate::web_api::Api {
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
            scenes: [("scene".into(), "canonical scene label".into())].into(),
        })),
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web-actions.sqlite")).unwrap(),
        ),
        drafts: None,
    });
    async fn request(
        h: &mut Harness,
        api: Arc<crate::web_api::Api>,
        secret: &str,
        body: Option<Json>,
    ) -> (hyper::StatusCode, Json) {
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
        let value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        (status, value)
    }
    let (status, view) = request(&mut h, api.clone(), secret, None).await;
    assert_eq!(status, hyper::StatusCode::OK);
    assert_eq!(view["scenes"][0]["activity_kind"], "read_only");
    assert_eq!(view["scenes"][0]["switch_kind"], "momentary");
    assert_eq!(view["scenes"][0]["can_control"], true);
    assert_eq!(view["scenes"][0]["activity"], "Active");
    assert_eq!(view["scenes"][0]["tracking"]["activity_writable"], false);
    for target in ["sact:scene", "scene/activity"] {
        let (status, _) = request(
            &mut h,
            api.clone(),
            secret,
            Some(json!({"scene":target,"request_id":"wrong-role","on":true})),
        )
        .await;
        assert_eq!(status, hyper::StatusCode::FORBIDDEN);
    }
    let (status, _) = request(
        &mut h,
        api.clone(),
        viewer,
        Some(json!({"scene":"scene","request_id":"viewer","on":true})),
    )
    .await;
    assert_eq!(status, hyper::StatusCode::FORBIDDEN);
    assert_eq!(h.commands(), 0);
    let (status, receipt) = request(
        &mut h,
        api.clone(),
        secret,
        Some(json!({"scene":"scene","request_id":"explicit-web-action"})),
    )
    .await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED);
    assert_eq!(receipt["outcome"], "accepted");
    h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
    let bytes = Arc::new(std::sync::Mutex::new(vec![]));
    let mut owner = crate::transport_owner::TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(MemoryWire(bytes.clone()), 1),
        )]
        .into(),
    );
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(*bytes.lock().unwrap(), [0xae, 0x15, 1, 100, 0xee]);
    assert_eq!(h.commands(), 1);
    artifact(
        "web-independent-action-wire.json",
        &json!({"view":view,"receipt":receipt,"wire":*bytes.lock().unwrap(),"action_commands":1,"read_commands":0,"viewer_denied":true,"role_alias_targets_denied":true}),
    );
    drop(api);
    h.finish().await;
}
#[tokio::test]
async fn activity_notification_rechecks_expiry_and_authority_after_waiting_for_hap_lock() {
    use hap::server::{EventEmitter, ServerEvent};
    for expired in [true, false] {
        let mut r = rules();
        if expired {
            r["scene_tracking"][0]["max_age_ms"] = 100.into();
        }
        let mut h = Harness::new(false, r).await;
        let mut identity =
            homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
                .await
                .unwrap();
        hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
            .await
            .unwrap();
        let mut bridge = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
            &identity,
            &h.document,
            vec![homesense_hap::scene_graph::SceneSpec {
                id: "scene".into(),
                name: "Action".into(),
            }],
            h.control.clone(),
            h.actor.clone(),
            h.controller,
            |_, _| true,
        )
        .await
        .unwrap();
        h.samples(Some(35), Some(false));
        let c = h.control.clone();
        let who = h.controller;
        h.drive(tokio::spawn(async move {
            c.request(Action::SceneTracking {
                controller: who,
                revision: 1,
                scene: "scene".into(),
            })
            .await
        }))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        h.receiver
            .poll(
                &h.actor,
                &h.document,
                &mut h.scheduler,
                &h.feedback,
                5000,
                None,
            )
            .await;
        let events = Arc::new(std::sync::Mutex::new(vec![]));
        let out = events.clone();
        let mut emitter = EventEmitter::new();
        emitter.add_listener(Box::new(move |e| {
            if let ServerEvent::CharacteristicValueChanged { value, .. } = e {
                out.lock().unwrap().push(value.clone());
            }
            Box::pin(async {})
        }));
        let handle = bridge.activities[0].1.clone();
        let mut lock = handle.lock().await;
        lock.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(
            emitter,
        ))));
        let pending = tokio::spawn(async move {
            bridge.flush_resets().await.unwrap();
            bridge
        });
        tokio::time::sleep(Duration::from_millis(if expired { 110 } else { 15 })).await;
        if !expired {
            h.authorized.store(false, Ordering::Release);
        }
        drop(lock);
        let bridge = pending.await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            [Json::Null],
            "expired={expired}: cannot publish captured Active after a lock wait"
        );
        assert_eq!(h.commands(), 0);
        drop(bridge);
        drop(identity);
        h.finish().await;
    }
}
