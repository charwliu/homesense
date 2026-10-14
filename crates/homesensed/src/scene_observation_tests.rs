//! Synthetic raw-key/actor/database integration. No network or physical output.
use super::*;
use crate::{admission::Options, transport_owner::GatewayRead};
use homesense_core::{ExecutionContext, rule::Value};
use homesense_storage::{Store, variables::Update};
use serde_json::{Value as Json, json};
use std::{path::PathBuf, time::Duration};
fn devices() -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
        {"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}},
        {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
        {"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}},
        {"address":"42","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[
        {"id":"light","name":"Synthetic light","gateway":"bus","address":"15","channel":1,"enabled":true},
        {"id":"switch","name":"Synthetic secondary light","gateway":"bus","address":"03","channel":1,"enabled":true},
        {"id":"key1","name":"Synthetic first key","gateway":"bus","address":"41","channel":1,"enabled":true},
        {"id":"key3","name":"Synthetic second key","gateway":"bus","address":"41","channel":3,"enabled":true},
        {"id":"key5","name":"Synthetic third key","gateway":"bus","address":"41","channel":5,"enabled":true},
        {"id":"other-led","name":"Synthetic second panel","gateway":"bus","address":"42","channel":1,"enabled":true}]})).unwrap()
}
fn automation() -> Json {
    let scenes=[("first",100),("second",60),("third",17),("off",0)].map(|(id,level)|json!({"id":id,"enabled":true,"review_evidence":"synthetic only","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":60000,"device":"light","command":{"kind":"level","value":level,"ramp":false}}]}));
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":scenes,
        "scene_switch_groups":[{"id":"group","variable":"selection","review_evidence":"synthetic toggle group","members":[{"scene":"first","value":1,"off_scene":"off"},{"scene":"second","value":2,"off_scene":"off"},{"scene":"third","value":3,"off_scene":"off"}]}],
        "scene_observation":{"enabled":true,"debounce_ms":30,"max_age_ms":5000,"external_toggle_reviews":[{"group":"group","review_evidence":"synthetic complete external writer inventory"}],"bindings":[
            {"device":"key1","group":"group","member":1,"review_evidence":"synthetic raw key mapping"},
            {"device":"key3","group":"group","member":2,"review_evidence":"synthetic raw key mapping"},
            {"device":"key5","group":"group","member":3,"review_evidence":"synthetic raw key mapping"}]}})
}
fn parse(input: &Json) -> Result<Document, homesense_config::Error> {
    let config =
        homesense_config::Configuration::parse(&devices(), ExecutionContext::Simulation).unwrap();
    Document::parse(&serde_json::to_vec(input).unwrap(), &config)
}
#[test]
fn strict_reviewed_observation_defaults_off_and_rejects_execution_overlap() {
    let base = automation();
    assert!(parse(&base).is_ok());
    let mut disabled = base.clone();
    disabled["scene_observation"]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    for binding in disabled["scene_observation"]["bindings"]
        .as_array_mut()
        .unwrap()
    {
        binding.as_object_mut().unwrap().remove("review_evidence");
    }
    assert!(!parse(&disabled).unwrap().scene_observation.unwrap().enabled);
    let mut invalid = vec![];
    for (field, value) in [
        ("debounce_ms", json!(0)),
        ("max_age_ms", json!(29)),
        ("max_age_ms", json!(30001)),
        ("execute", json!(true)),
    ] {
        let mut item = base.clone();
        item["scene_observation"][field] = value;
        invalid.push(item);
    }
    for (field, value) in [
        ("device", json!("light")),
        ("device", json!("missing")),
        ("group", json!("missing")),
        ("member", json!(0)),
        ("member", json!(7)),
        ("review_evidence", json!("")),
    ] {
        let mut item = base.clone();
        item["scene_observation"]["bindings"][0][field] = value;
        invalid.push(item);
    }
    for reviews in [
        json!([]),
        json!([{ "group":"group", "review_evidence":"" }]),
        json!([{ "group":"missing", "review_evidence":"synthetic" }]),
        json!([{ "group":"group", "review_evidence":"synthetic" }, { "group":"group", "review_evidence":"duplicate" }]),
    ] {
        let mut item = base.clone();
        item["scene_observation"]["external_toggle_reviews"] = reviews;
        invalid.push(item);
    }
    let mut partial = base.clone();
    partial["scene_observation"]["bindings"]
        .as_array_mut()
        .unwrap()
        .pop();
    invalid.push(partial);
    let mut item = base.clone();
    item["scene_observation"]["bindings"][1]["device"] = json!("key1");
    invalid.push(item);
    for trigger in [
        json!({"kind":"panel","device":"key1","event":"pressed"}),
        json!({"kind":"variable_changed","variable":"selection"}),
    ] {
        let mut item = base.clone();
        item["rules"] = json!([{"id":"conflict","enabled":true,"source":"synthetic","review_evidence":"synthetic","trigger":trigger,"actions":[]}]);
        assert!(parse(&item).is_err());
        item["rules"][0]["enabled"] = json!(false);
        assert!(parse(&item).is_ok());
    }
    let guard = json!({"source":"synthetic unknown","action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":"selection"}},"then_actions":[{"source":"synthetic output","action":{"kind":"command","device":"light","command":{"kind":"level","value":50,"ramp":false}}}],"else_actions":[]}});
    let mut read = base.clone();
    read["rules"] = json!([{"id":"unknown-fallback","enabled":true,"source":"synthetic","review_evidence":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[guard.clone()]}]);
    assert!(parse(&read).is_err());
    read["rules"][0]["enabled"] = json!(false);
    assert!(parse(&read).is_ok());
    let mut expanded = base.clone();
    expanded["immediate_scenes"] =
        json!([{"id":"nested","review_evidence":"synthetic","activate":[guard],"deactivate":[]}]);
    expanded["rules"] = json!([{"id":"nested-call","enabled":true,"source":"synthetic","review_evidence":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"synthetic call","action":{"kind":"scene","id":"nested","activate":true}}]}]);
    assert!(parse(&expanded).is_err());
    let mut literal = base.clone();
    literal["rules"] = json!([{"id":"explicit-intent","enabled":true,"source":"synthetic","review_evidence":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"synthetic set","action":{"kind":"set","variable":"selection","value":{"kind":"integer","value":0}}}]}]);
    assert!(parse(&literal).is_ok());
    let mut item = base.clone();
    item["holds"] = json!({"interval_ms":100,"increment":1,"bindings":[{"key":"key1","device":"light","up":true,"enabled":true,"review_evidence":"synthetic","release":"cancel_pending_only"}]});
    assert!(parse(&item).is_err());
    item["holds"]["bindings"][0]["enabled"] = json!(false);
    assert!(parse(&item).is_ok());
    for item in invalid {
        assert!(
            parse(&item).is_err(),
            "accepted invalid observation: {item}"
        );
    }
}
struct Harness {
    actor: Admission,
    feedback: Feedback,
    consumer: Consumer,
    controller: uuid::Uuid,
    generation: TaskGeneration,
    document: Document,
    directory: PathBuf,
}
impl Harness {
    async fn new(value: Option<i64>, input: Json, release: bool) -> Self {
        Self::new_with_initial(value, input, release, None).await
    }
    async fn new_with_initial(
        value: Option<i64>,
        input: Json,
        release: bool,
        pressed: Option<u8>,
    ) -> Self {
        let directory =
            std::env::temp_dir().join(format!("homesense-scene-observer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
        let mut store = Store::open(&path).unwrap();
        let bundle = store
            .publish_automation(
                None,
                &devices(),
                &serde_json::to_vec(&input).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        if value.is_some() {
            store
                .accept_queue_batch_with_variables(
                    "seed",
                    &[],
                    &[],
                    0,
                    None,
                    &[Update {
                        revision: 1,
                        name: "selection".into(),
                        expected_version: 0,
                        value: value.map(Value::Integer),
                    }],
                )
                .unwrap();
        }
        drop(store);
        let controller = uuid::Uuid::new_v4();
        let actor = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        let generation = actor.task_generation().await.unwrap();
        let feedback = Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            uuid::Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            generation.epochs.transport,
        )
        .unwrap();
        let mut harness = Self {
            actor,
            feedback,
            consumer: Consumer::default(),
            controller,
            generation,
            document: bundle.automation.unwrap(),
            directory,
        };
        if release {
            for key in [1, 3, 5] {
                harness.sample(key, false);
            }
        }
        if let Some(key) = pressed {
            harness.sample(key, true);
        }
        harness.tick().await;
        harness
    }
    fn sample_at(&mut self, key: u8, down: bool, at: u64) {
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: self.generation.epochs.transport,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: self.generation.epochs.transport,
                    frames: vec![hb_protocol::RawFrame {
                        address: 0x41,
                        command: 0xd0 + key,
                        value: if down { 0xf1 } else { 0xf2 },
                    }],
                },
            })
            .unwrap();
    }
    fn sample(&mut self, key: u8, down: bool) {
        self.sample_at(key, down, self.actor.monotonic_ms());
    }
    async fn tick(&mut self) {
        self.consumer
            .tick(&self.actor, self.controller, &self.document, &self.feedback)
            .await
            .unwrap();
    }
    async fn selection(&self) -> homesense_storage::scene_observation::Snapshot {
        self.actor
            .scene_selection_snapshot(
                self.controller,
                self.generation.clone(),
                vec!["selection".into()],
            )
            .await
            .unwrap()
            .remove("selection")
            .unwrap()
    }
    async fn press(&mut self, key: u8) {
        self.sample(key, false);
        self.sample(key, true);
        self.tick().await;
    }
    // Start unknown, then converge through two distinct reviewed keys. This
    // never seeds EA state from the local fixture's recorded value.
    async fn select_external(&mut self, key: u8) {
        self.press(if key == 1 { 3 } else { 1 }).await;
        self.press(key).await;
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap()
    }
    fn no_output(&self) {
        for table in ["command_journal", "scene_run", "rule_admission"] {
            assert_eq!(
                self.sql()
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    async fn close(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
#[tokio::test]
async fn current_raw_keys_infer_toggle_and_exclusion_with_no_commands_or_confirmed_rule_value() {
    let mut h = Harness::new(Some(0), automation(), true).await;
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, None);
    for (key, expected) in [(3, 2), (1, 1), (5, 3)] {
        tokio::time::sleep(Duration::from_millis(35)).await;
        h.press(key).await;
        let state = h.selection().await;
        assert_eq!(state.stored.value, Some(Value::Integer(expected)));
        assert!(state.evidence.is_some());
        assert_eq!(
            h.actor
                .variable_snapshot(h.controller, h.generation.clone(), vec!["selection".into()])
                .await
                .unwrap()["selection"]
                .value,
            None
        );
    }
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.sample(5, true);
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(3))); // held
    h.press(5).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    h.no_output();
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM scene_observation", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert!(
        h.actor
            .scene_selection_snapshot(
                uuid::Uuid::new_v4(),
                h.generation.clone(),
                vec!["selection".into()]
            )
            .await
            .is_err()
    );
    h.close().await;
}
#[tokio::test]
async fn unknown_selection_stays_unknown_but_only_known_held_keys_are_suppressed() {
    let mut unknown = Harness::new(None, automation(), true).await;
    unknown.press(1).await;
    let state = unknown.selection().await;
    assert_eq!(state.stored.value, None);
    assert_eq!(
        state.evidence.unwrap().basis,
        homesense_core::scene_observer::Basis::UnknownInitial
    );
    unknown.no_output();
    unknown.close().await;
    let mut fresh = Harness::new(Some(0), automation(), false).await;
    fresh.sample(1, true);
    fresh.tick().await;
    assert_eq!(fresh.selection().await.stored.value, None);
    fresh.no_output();
    fresh.close().await;
    let mut held = Harness::new_with_initial(Some(0), automation(), true, Some(1)).await;
    held.sample(1, true);
    held.tick().await;
    assert_eq!(held.selection().await.stored.value, Some(Value::Integer(0)));
    assert!(held.selection().await.evidence.is_none());
    held.press(1).await;
    assert_eq!(held.selection().await.stored.value, None);
    held.press(3).await;
    assert_eq!(held.selection().await.stored.value, Some(Value::Integer(2)));
    held.no_output();
    held.close().await;
}
#[tokio::test]
async fn stale_keys_are_consumed_and_forged_or_changed_event_proofs_rejected() {
    let mut input = automation();
    input["scene_observation"]["max_age_ms"] = json!(30);
    let mut h = Harness::new(Some(0), input, true).await;
    h.sample(1, true);
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    h.sample(1, true);
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    h.tick().await;
    h.select_external(1).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    let before = h.feedback.snapshot().cursor;
    h.sample(3, true);
    let event = h.feedback.events_since(before).unwrap().remove(0);
    let mut forged = event.clone();
    forged.device_id = homesense_core::DeviceId::new("key5").unwrap();
    assert!(
        h.feedback
            .scene_key_observation(&forged, h.actor.monotonic_ms())
            .is_err()
    );
    let proof = h
        .feedback
        .scene_key_observation(&event, h.actor.monotonic_ms())
        .unwrap();
    h.sample(3, false);
    assert_eq!(
        h.actor
            .observe_scene_key(
                h.controller,
                h.generation.clone(),
                h.consumer.scope.unwrap(),
                proof
            )
            .await,
        Err(WriteStatus::ResourceBusy)
    );
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn a_new_manual_choice_fences_a_previously_received_key_without_clearing_it() {
    let mut h = Harness::new(Some(0), automation(), true).await;
    h.sample(1, true); // captured before later manual choice, deliberately not consumed
    let version = h.selection().await.stored.version;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![],
                subscriptions: vec![],
            },
            vec![Update {
                revision: 1,
                name: "selection".into(),
                expected_version: version,
                value: Some(Value::Integer(2)),
            }],
        )
        .await
        .unwrap();
    h.tick().await;
    let state = h.selection().await;
    assert_eq!(state.stored.value, Some(Value::Integer(2)));
    assert!(state.evidence.is_none());
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, None);
    h.press(3).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(2)));
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn stream_gaps_invalidate_old_inference_and_do_not_replay_buffered_keys() {
    let mut h = Harness::new(Some(0), automation(), true).await;
    h.select_external(3).await;
    let old = h.consumer.scope.unwrap();
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(2)));
    for _ in 0..300 {
        h.sample(3, false);
    }
    h.tick().await;
    assert_ne!(h.consumer.scope.unwrap(), old);
    let state = h.selection().await;
    assert_eq!(state.stored.value, None);
    assert!(state.evidence.is_some());
    h.press(1).await;
    let state = h.selection().await;
    assert_eq!(state.stored.value, None);
    assert_eq!(
        state.evidence.unwrap().basis,
        homesense_core::scene_observer::Basis::UnknownInitial
    );
    // The atomic stream fence invalidates reads without waiting for another key
    // or successfully enqueueing a begin message.
    h.actor.invalidate_scene_observation_now().unwrap();
    assert_eq!(h.selection().await.stored.value, None);
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn disabled_observation_has_no_session_and_transport_generation_drops_old_inference() {
    let mut input = automation();
    input["scene_observation"]["enabled"] = json!(false);
    let mut disabled = Harness::new(Some(0), input, true).await;
    disabled.press(1).await;
    assert!(disabled.consumer.scope.is_none());
    assert_eq!(
        disabled.selection().await.stored.value,
        Some(Value::Integer(0))
    );
    disabled.no_output();
    disabled.close().await;
    let mut h = Harness::new(Some(0), automation(), true).await;
    h.select_external(1).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    let old_generation = h.generation.clone();
    h.actor.transport_failed().await.unwrap();
    h.actor.recover_synchronized(2, 1).await.unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    h.feedback.invalidate().unwrap();
    h.feedback.resume(2, h.actor.monotonic_ms()).unwrap();
    for key in [1, 3, 5] {
        h.sample(key, false);
    }
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, None);
    assert!(
        h.actor
            .scene_selection_snapshot(h.controller, old_generation, vec!["selection".into()])
            .await
            .is_err()
    );
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, None);
    h.no_output();
    h.close().await;
}

async fn poll_request<F: std::future::Future>(
    h: &Harness,
    receiver: &mut crate::automation_control::Receiver,
    scenes: &mut crate::scene_scheduler::SceneScheduler,
    request: F,
) -> F::Output {
    let done = std::cell::Cell::new(false);
    let client = async {
        let reply = request.await;
        done.set(true);
        reply
    };
    let drive = async {
        while !done.get() {
            receiver
                .poll(&h.actor, &h.document, scenes, &h.feedback, 5000, None)
                .await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(client, drive).0
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn hap_source_web_metadata_and_manual_off_share_one_scoped_selection() {
    use crate::automation_control::{Action, Reply};
    use crate::web_auth::{Grant, Role, Tokens};
    use http_body_util::BodyExt;
    let mut h = Harness::new(Some(0), automation(), true).await;
    h.select_external(3).await;
    let source = crate::scene_selection_hap::Source::new(
        h.actor.clone(),
        h.controller,
        h.document.scene_switch_groups.clone(),
    )
    .await
    .unwrap();
    let states = source.sample().await.unwrap();
    assert_eq!(states["second"], Some(true));
    assert_eq!(states["first"], Some(false));
    let (control, mut receiver) =
        crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let Reply::SceneSelectionDetails(details) = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        control.request(Action::SceneSelectionDetails {
            controller: h.controller,
            revision: 1,
        }),
    )
    .await
    .unwrap() else {
        panic!("wrong selection details")
    };
    assert_eq!(details["second"].selected, Some(true));
    assert_eq!(details["second"].selection_source, "inferred");
    assert_eq!(details["second"].observation_current, Some(true));
    assert!(details["second"].observed_ms.is_some());
    // The actual API filters scene scope and returns provenance without leaking
    // raw panel identifiers, even when the selection was inferred via another key.
    let tokens = std::sync::Arc::new(Tokens::default());
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            uuid::Uuid::new_v4(),
            secret,
            Grant {
                controller: h.controller,
                role: Role::Viewer,
                scenes: ["first".into()].into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    let api = crate::web_api::Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://127.0.0.1:8088".into(),
        tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control: control.clone(),
            revision: 1,
            boot: h.actor.boot_id().into(),
            scenes: [
                ("first".into(), "First".into()),
                ("second".into(), "Second".into()),
            ]
            .into(),
        })),
        drafts: None,
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap(),
        ),
    };
    let request = hyper::Request::builder()
        .uri("/api/v1/scenes")
        .header("host", "127.0.0.1:8088")
        .header("authorization", format!("Bearer {secret}"))
        .body(hyper::body::Bytes::new())
        .unwrap();
    let response = poll_request(&h, &mut receiver, &mut scenes, api.handle(request)).await;
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let payload: Json =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(payload["scenes"][0]["id"], "first");
    assert_eq!(payload["scenes"][0]["selected"], false);
    assert_eq!(payload["scenes"][0]["selection_source"], "inferred");
    assert_eq!(payload["scenes"][0]["observation_current"], true);
    assert!(!payload.to_string().contains("key3"));
    h.no_output();
    let reply = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        control.request(Action::SceneWrites {
            controller: h.controller,
            revision: 1,
            scenes: vec![("second".into(), Some(false))],
        }),
    )
    .await
    .unwrap();
    assert!(matches!(reply,Reply::ScenesAccepted(runs) if runs.len()==1));
    let state = h.selection().await;
    assert_eq!(state.stored.value, Some(Value::Integer(0)));
    assert!(state.evidence.is_none());
    assert_eq!(source.sample().await.unwrap()["second"], Some(false));
    assert_eq!(
        h.sql()
            .query_row("SELECT definition FROM scene_run", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "off"
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    ); // explicitly scheduled Off is delayed; no physical sink
    scenes.stop_now();
    drop(api);
    h.close().await;
}

// Independent completion audit: only the test fixture adds a second light.
// Observe a real raw-key inference through Consumer/Admission/SQLite, then
// submit the production indicator synchronizer against fresh physical feedback.
async fn audit_led_partial_off(selected_key: u8, expected: i64) {
    let mut input = automation();
    input["timed_scenes"][0]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "source":"synthetic secondary light", "offset_ms":60000,
            "device":"switch", "command":{"kind":"switch","on":true}
        }));
    input["indicator_links"] = json!([{
        "id":"first-off", "enabled":true, "review_evidence":"synthetic audit only",
        "scene":"first", "lights":["light","switch"], "leds":["key1","other-led"]
    }]);
    let mut h = Harness::new(Some(0), input, true).await;
    h.select_external(selected_key).await;
    let before = h.selection().await;
    assert!(before.evidence_current);
    h.no_output();
    tokio::time::sleep(Duration::from_millis(3)).await;
    let now = h.actor.monotonic_ms();
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: now,
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: 100,
                    },
                    hb_protocol::RawFrame {
                        address: 0x03,
                        command: 0xd1,
                        value: 0xf2,
                    },
                ],
            },
        })
        .unwrap();
    let scenes =
        crate::scene_scheduler::SceneScheduler::new(h.actor.clone(), h.controller, 8, now).unwrap();
    crate::indicator_sync::Synchronizer::default()
        .tick(
            &h.actor,
            h.controller,
            &h.document,
            &scenes,
            &h.feedback,
            60000,
        )
        .await
        .unwrap();
    let after = h.selection().await;
    let ledger = h.sql();
    let scenes_created: i64 = ledger
        .query_row("SELECT count(*) FROM scene_run", [], |r| r.get(0))
        .unwrap();
    let commands: Vec<String> = ledger
        .prepare("SELECT device_id FROM command_journal")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(ledger);
    drop(scenes);
    h.close().await;
    assert_eq!(
        scenes_created, 0,
        "independent LED sync must not execute a scene"
    );
    assert_eq!(
        commands,
        vec!["key1", "other-led"],
        "exactly the two reviewed panels, with no lighting commands"
    );
    assert_eq!(
        after.stored.value,
        Some(Value::Integer(expected)),
        "partial-off reconciliation must clear only the inferred selected member; before={before:?}, after={after:?}"
    );
    if selected_key == 3 {
        assert_eq!(
            after.stored.version, before.stored.version,
            "unrelated LED clearing must not rewrite the active inferred selection"
        );
        assert!(
            after.evidence_current,
            "unrelated LED clearing must preserve provenance"
        );
    }
}
#[tokio::test]
async fn audit_led_partial_off_clears_the_inferred_selected_member() {
    audit_led_partial_off(1, 0).await;
}
#[tokio::test]
async fn audit_led_partial_off_preserves_a_different_inferred_member() {
    audit_led_partial_off(3, 2).await;
}

fn linked_automation() -> Json {
    let mut input = automation();
    input["indicator_links"] = json!([{"id":"first-off","enabled":true,"review_evidence":"synthetic only","scene":"first","lights":["light"],"leds":["key1"]}]);
    input
}
fn light_sample(h: &mut Harness, at: u64, level: u8) {
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x15,
                    command: 0xd1,
                    value: level,
                }],
            },
        })
        .unwrap();
}
async fn off_plan(h: &Harness) -> crate::rule_runtime::PreparedRule {
    use homesense_core::rule::{Action, Budget, Condition, Expr, Node};
    let nodes = vec![Node {
        source: "synthetic off".into(),
        action: Action::If {
            condition: Condition::Equal(
                Expr::DeviceOn(homesense_core::DeviceId::new("light").unwrap()),
                Expr::Literal(Value::Bool(false)),
            ),
            then_actions: vec![Node {
                source: "synthetic led".into(),
                action: Action::Command {
                    device: homesense_core::DeviceId::new("key1").unwrap(),
                    command: homesense_core::Command::PanelLed(false),
                },
            }],
            else_actions: vec![],
        },
    }];
    let mut lease = None;
    let mut plan = crate::rule_runtime::PreparedRule::prepare_indicator(
        &h.actor,
        h.controller,
        h.generation.clone(),
        vec!["selection".into()],
        |variables, _, policy| {
            let (plan, read) = h.feedback.plan_indicator_rule(
                &nodes,
                variables,
                Budget::default(),
                h.actor.monotonic_ms(),
                60000,
                &std::collections::BTreeMap::new(),
                policy,
            )?;
            lease = Some(read);
            Ok(plan)
        },
    )
    .await
    .map_err(|_| "preparation failed")
    .unwrap();
    plan.attach_feedback(lease);
    plan
}
#[tokio::test]
async fn indicator_inference_rejects_old_feedback_then_clears_with_post_key_feedback() {
    let mut h = Harness::new(Some(0), linked_automation(), true).await;
    let old = h.actor.monotonic_ms();
    light_sample(&mut h, old, 0);
    tokio::time::sleep(Duration::from_millis(3)).await;
    h.select_external(1).await;
    let before = h.selection().await;
    let scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let mut sync = crate::indicator_sync::Synchronizer::default();
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    assert_eq!(h.selection().await.stored, before.stored);
    h.no_output();
    // Same-millisecond input cannot prove that the sample followed the key.
    let key_time = before.evidence.as_ref().unwrap().observed_ms;
    light_sample(&mut h, key_time, 0);
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    h.no_output();
    tokio::time::sleep(Duration::from_millis(3)).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 0);
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    let after = h.selection().await;
    assert_eq!(after.stored.value, Some(Value::Integer(0)));
    assert_eq!(after.stored.version, before.stored.version + 1);
    assert!(after.evidence.is_none());
    let count: i64 = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    let count: i64 = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "settled repeated off is not replayed");
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.press(1).await;
    let after_key = h.selection().await;
    assert_eq!(after_key.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        after_key.evidence.unwrap().basis,
        homesense_core::scene_observer::Basis::ToggleOff,
        "LED cleared the local selection, but EA's independent selection was still first"
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(scenes);
    h.close().await;
}
#[tokio::test]
async fn indicator_source_guard_rejects_stream_invalidation_and_newer_local_choice() {
    for invalidate in [true, false] {
        let mut h = Harness::new(Some(0), linked_automation(), true).await;
        h.select_external(1).await;
        tokio::time::sleep(Duration::from_millis(3)).await;
        let now = h.actor.monotonic_ms();
        light_sample(&mut h, now, 0);
        let plan = off_plan(&h).await;
        if invalidate {
            h.actor.invalidate_scene_observation_now().unwrap();
        } else {
            let before = h.selection().await;
            h.actor
                .submit_rule_update(
                    h.generation.clone(),
                    homesense_hap::requests::Batch {
                        controller: h.controller,
                        intents: vec![],
                        subscriptions: vec![],
                    },
                    vec![Update {
                        revision: 1,
                        name: "selection".into(),
                        expected_version: before.stored.version,
                        value: Some(Value::Integer(2)),
                    }],
                )
                .await
                .unwrap();
        }
        let before = h.selection().await;
        assert_eq!(
            plan.commit_status(&h.actor).await,
            Err(hap::server::WriteStatus::InvalidValueInRequest)
        );
        let after = h.selection().await;
        assert_eq!(after.stored, before.stored);
        assert_eq!(after.evidence, before.evidence);
        h.no_output();
        h.close().await;
    }
}

// Synthetic dispatch receipts only: no socket, gateway or physical write.
async fn completed_zero_setting(h: &Harness) {
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: homesense_core::DeviceId::new("light").unwrap(),
                    command: homesense_core::Command::SetLevel {
                        level: homesense_core::Percent::new(0).unwrap(),
                        transition: homesense_core::LevelTransition::Immediate,
                    },
                }],
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let mut written = false;
    for _ in 0..100 {
        if let Some(ticket) = h.actor.dispatch().await.unwrap() {
            h.actor.finish_write(ticket.command.id, true).await.unwrap();
            written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(written);
}
async fn confirmed_zero_setting(h: &mut Harness) {
    completed_zero_setting(h).await;
    tokio::time::sleep(Duration::from_millis(3)).await;
    let now=h.actor.monotonic_ms();
    light_sample(h,now,0);
    h.actor.remember_levels(1,h.feedback.snapshot(),60000).await.unwrap();
}
#[tokio::test]
async fn inference_off_requires_fresh_actual_off_not_a_prior_zero_setting() {
    let mut h = Harness::new(Some(0), linked_automation(), true).await;
    confirmed_zero_setting(&mut h).await;
    tokio::time::sleep(Duration::from_millis(3)).await;
    h.select_external(1).await;
    let before = h.selection().await;
    tokio::time::sleep(Duration::from_millis(3)).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 100);
    let scenes =
        crate::scene_scheduler::SceneScheduler::new(h.actor.clone(), h.controller, 8, now).unwrap();
    let mut sync = crate::indicator_sync::Synchronizer::default();
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    let after = h.selection().await;
    assert_eq!(after.stored, before.stored);
    assert_eq!(after.evidence, before.evidence);
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    confirmed_zero_setting(&mut h).await;
    tokio::time::sleep(Duration::from_millis(3)).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 0);
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(scenes);
    h.close().await;
}

fn audit_led_linked_document() -> Json {
    let mut input = automation();
    input["timed_scenes"][0]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "source":"synthetic secondary light", "offset_ms":60000,
            "device":"switch", "command":{"kind":"switch","on":true}
        }));
    input["indicator_links"] = json!([{"id":"first-off","enabled":true,"review_evidence":"synthetic audit only","scene":"first","lights":["light","switch"],"leds":["key1"]}]);
    input
}
async fn audit_led_prior_zero_with_fresh_switch_off(switch_on: bool) {
    use homesense_core::{Command, DeviceId, LevelTransition, Percent};
    use homesense_hap::requests::{Batch, Intent};
    let mut h = Harness::new(Some(0), audit_led_linked_document(), true).await;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            Batch {
                controller: h.controller,
                intents: vec![Intent {
                    device: DeviceId::new("light").unwrap(),
                    command: Command::SetLevel {
                        level: Percent::new(0).unwrap(),
                        transition: LevelTransition::Immediate,
                    },
                }],
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    h.actor.finish_write(ticket.command.id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    // Complete the earlier local target before the external scene takes over.
    let settled=h.actor.monotonic_ms();light_sample(&mut h,settled,0);
    h.actor.remember_levels(1,h.feedback.snapshot(),60000).await.unwrap();
    h.select_external(1).await;
    let before = h.selection().await;
    assert_eq!(before.stored.value, Some(Value::Integer(1)));
    assert!(before.evidence_current);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let now = h.actor.monotonic_ms();
    assert!(now > before.evidence.as_ref().unwrap().observed_ms);
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: now,
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: 100,
                    },
                    hb_protocol::RawFrame {
                        address: 0x03,
                        command: 0xd1,
                        value: if switch_on { 0xf1 } else { 0xf2 },
                    },
                ],
            },
        })
        .unwrap();
    let scenes =
        crate::scene_scheduler::SceneScheduler::new(h.actor.clone(), h.controller, 8, now).unwrap();
    crate::indicator_sync::Synchronizer::default()
        .tick(
            &h.actor,
            h.controller,
            &h.document,
            &scenes,
            &h.feedback,
            60000,
        )
        .await
        .unwrap();
    let after = h.selection().await;
    let led_count: i64 = h
        .sql()
        .query_row(
            "SELECT count(*) FROM command_journal WHERE device_id='key1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let runs: i64 = h
        .sql()
        .query_row("SELECT count(*) FROM scene_run", [], |r| r.get(0))
        .unwrap();
    drop(scenes);
    h.close().await;
    assert_eq!(runs, 0);
    if switch_on {
        assert_eq!(
            after.stored, before.stored,
            "an old dimmer zero alone is not post-key off evidence"
        );
        assert_eq!(after.evidence, before.evidence);
        assert_eq!(led_count, 0);
    } else {
        assert_eq!(
            after.stored.value,
            Some(Value::Integer(0)),
            "the independent fresh Switch(false) must cancel the selected scene even when a dimmer's old zero is unusable; before={before:?}, after={after:?}, led_count={led_count}"
        );
        assert_eq!(led_count, 1);
    }
}
#[tokio::test]
async fn audit_led_prior_zero_does_not_block_fresh_switch_off_reconciliation() {
    audit_led_prior_zero_with_fresh_switch_off(false).await;
}
#[tokio::test]
async fn audit_led_prior_zero_alone_does_not_cancel_new_inference() {
    audit_led_prior_zero_with_fresh_switch_off(true).await;
}

#[tokio::test]
async fn one_fresh_switch_off_cancels_inference_while_other_light_is_unknown() {
    let mut h = Harness::new(Some(0), audit_led_linked_document(), true).await;
    h.select_external(1).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let now = h.actor.monotonic_ms();
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: now,
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd1,
                    value: 0xf2,
                }],
            },
        })
        .unwrap();
    let scenes =
        crate::scene_scheduler::SceneScheduler::new(h.actor.clone(), h.controller, 8, now).unwrap();
    crate::indicator_sync::Synchronizer::default()
        .tick(
            &h.actor,
            h.controller,
            &h.document,
            &scenes,
            &h.feedback,
            60000,
        )
        .await
        .unwrap();
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id!='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(scenes);
    h.close().await;
}

fn release_automation() -> Json {
    let mut input = linked_automation();
    for binding in input["scene_observation"]["bindings"]
        .as_array_mut()
        .unwrap()
    {
        binding["trigger_edge"] = json!("released");
    }
    input
}
#[test]
fn release_edge_config_is_explicit_and_legacy_bindings_default_to_pressed() {
    let legacy = parse(&automation()).unwrap();
    assert!(
        legacy
            .scene_observation
            .unwrap()
            .bindings
            .iter()
            .all(|b| b.trigger_edge == homesense_config::scene_observation::TriggerEdge::Pressed)
    );
    assert!(
        parse(&release_automation())
            .unwrap()
            .scene_observation
            .unwrap()
            .bindings
            .iter()
            .all(|b| b.trigger_edge == homesense_config::scene_observation::TriggerEdge::Released)
    );
    let mut invalid = release_automation();
    invalid["scene_observation"]["bindings"][0]["trigger_edge"] = json!("clicked");
    assert!(parse(&invalid).is_err());
}
#[tokio::test]
async fn held_press_old_lamp_feedback_then_reviewed_release_does_not_prematurely_select_or_cancel()
{
    let mut h = Harness::new(Some(0), release_automation(), true).await;
    h.sample(3, true);
    h.tick().await;
    h.sample(3, false);
    h.tick().await;
    let initial = h.selection().await;
    h.sample(1, true);
    h.tick().await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 0);
    h.tick().await;
    let held = h.selection().await;
    assert_eq!(held.stored, initial.stored);
    assert_eq!(held.evidence, initial.evidence);
    h.no_output();
    tokio::time::sleep(Duration::from_millis(5)).await;
    h.sample(1, false);
    h.tick().await;
    let released = h.selection().await;
    assert_eq!(released.stored.value, Some(Value::Integer(1)));
    assert!(released.evidence_current);
    assert!(released.evidence.as_ref().unwrap().observed_ms > now);
    let scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let mut sync = crate::indicator_sync::Synchronizer::default();
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    assert_eq!(h.selection().await.stored, released.stored);
    h.no_output();
    // A fresh post-release off may now cancel, preserving the off policy.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 0);
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(scenes);
    h.close().await;
}
#[tokio::test]
async fn reviewed_release_after_initial_hold_and_stream_gap_requires_new_press() {
    let mut h = Harness::new_with_initial(Some(0), release_automation(), true, Some(1)).await;
    h.sample(1, false);
    h.tick().await;
    assert!(h.selection().await.evidence.is_none());
    h.sample(1, true);
    h.tick().await;
    assert!(h.selection().await.evidence.is_none());
    h.actor.invalidate_scene_observation_now().unwrap();
    h.consumer = Consumer::default();
    h.tick().await;
    h.sample(1, false);
    h.tick().await;
    assert!(h.selection().await.evidence.is_none());
    h.no_output();
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.sample(1, true);
    h.tick().await;
    h.sample(1, false);
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, None);
    h.sample(3, true);
    h.tick().await;
    h.sample(3, false);
    h.tick().await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(2)));
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn local_manual_choice_does_not_seed_or_override_external_toggle_candidates() {
    let mut h = Harness::new(Some(2), automation(), true).await;
    h.press(3).await;
    assert_eq!(
        h.selection().await.stored.value,
        None,
        "local second is not EA second"
    );
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    let version = h.selection().await.stored.version;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![],
                subscriptions: vec![],
            },
            vec![Update {
                revision: 1,
                name: "selection".into(),
                expected_version: version,
                value: Some(Value::Integer(2)),
            }],
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.press(1).await;
    let off = h.selection().await;
    assert_eq!(off.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        off.evidence.unwrap().basis,
        homesense_core::scene_observer::Basis::ToggleOff
    );
    h.press(3).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(2)));
    h.actor.invalidate_scene_observation_now().unwrap();
    h.consumer = Consumer::default();
    h.tick().await;
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.press(3).await;
    assert_eq!(
        h.selection().await.stored.value,
        None,
        "new stream does not restore external possibilities from persistent value"
    );
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn missed_stale_external_transition_invalidates_known_published_selection() {
    let mut input = automation();
    input["scene_observation"]["max_age_ms"] = json!(30);
    let mut h = Harness::new(Some(0), input, true).await;
    h.select_external(1).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    h.sample(3, false);
    h.sample(3, true);
    tokio::time::sleep(Duration::from_millis(35)).await;
    h.tick().await;
    assert!(h.consumer.scope.is_none());
    assert_eq!(h.selection().await.stored.value, None);
    h.tick().await; // synchronize new stream without replaying the old key
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, None);
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn observed_unknown_startup_full_off_clears_leds_without_seeding_selection() {
    let mut h = Harness::new(None, linked_automation(), true).await;
    let now = h.actor.monotonic_ms();
    light_sample(&mut h, now, 0);
    let scenes =
        crate::scene_scheduler::SceneScheduler::new(h.actor.clone(), h.controller, 8, now).unwrap();
    let mut sync = crate::indicator_sync::Synchronizer::default();
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        60000,
    )
    .await
    .unwrap();
    let selection = h.selection().await;
    assert_eq!(selection.stored.value, None);
    assert_eq!(selection.stored.version, 0);
    assert!(selection.evidence.is_none());
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id!='key1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    h.press(1).await;
    assert_eq!(h.selection().await.stored.value, None);
    h.press(3).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(2)));
    drop(scenes);
    h.close().await;
}

#[path = "scene_initialization_tests.rs"]
mod initialization;
