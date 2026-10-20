//! Real HTTP handler, key preparation, runtime receiver, scene/native manager,
//! physical writer and raw protocol feedback. Everything stays in simulation.
use super::*;
use crate::{
    automation_control::{Control, Receiver, Reply},
    feedback::Feedback,
    scene_scheduler::SceneScheduler,
};
use http_body_util::BodyExt;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::Mutex,
};

struct Model {
    source: bool,
    condition: bool,
    reads: VecDeque<u8>,
    writes: Vec<Vec<u8>>,
}
struct Wire(Arc<Mutex<Model>>);
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let mut model = self.0.lock().unwrap();
        if model.reads.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = bytes.len().min(model.reads.len());
        for byte in &mut bytes[..n] {
            *byte = model.reads.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut m = self.0.lock().unwrap();
        m.writes.push(bytes.to_vec());
        assert_eq!(bytes.len() % 5, 0);
        for frame in bytes.as_chunks::<5>().0 {
            assert_eq!(frame[0], 0xae);
            assert_eq!(frame[4], 0xee);
            match frame[1] {
                3 => {
                    let channel = match frame[2] {
                        1 | 2 => {
                            let on = frame[3] == 0xf1;
                            assert!([0xf1, 0xf2].contains(&frame[3]));
                            if frame[2] == 1 {
                                m.source = on
                            } else {
                                m.condition = on
                            };
                            frame[2]
                        }
                        0xa1 => {
                            if frame[3] == 0xf1 {
                                m.source = !m.source;
                            } else {
                                assert_eq!(frame[3], 0xf2);
                            }
                            1
                        }
                        _ => panic!("unexpected relay frame {frame:?}"),
                    };
                    let on = if channel == 1 { m.source } else { m.condition };
                    m.reads
                        .extend([0xae, 3, 0xd0 + channel, if on { 0xf1 } else { 0xf2 }, 0xee]);
                }
                0x41 => assert!([0xf1, 0xf2].contains(&frame[3])),
                _ => panic!("unexpected module write {frame:?}"),
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Harness {
    f: Fixture,
    doc: homesense_config::rule_document::Document,
    feedback: Feedback,
    receiver: Receiver,
    scenes: SceneScheduler,
    router: crate::deferred_router::Router,
    control: Control,
    api: Arc<crate::web_api::Api>,
    web_path: PathBuf,
    owner: crate::transport_owner::TransportOwner<Wire>,
    model: Arc<Mutex<Model>>,
    completion: Consumer,
    modes: crate::component_modes::Consumer,
    batches: crate::control_batch::Consumer,
    indicators: crate::indicator_sync::Synchronizer,
    panels: crate::panel_rules::PanelRules,
    write_enabled: bool,
}
impl Harness {
    async fn new(source: bool, condition: bool, unknown: bool, delay: u64) -> Self {
        Self::program(source, condition, unknown, delay, None).await
    }
    async fn program(
        source: bool,
        condition: bool,
        unknown: bool,
        delay: u64,
        off_steps: Option<Vec<serde_json::Value>>,
    ) -> Self {
        Self::edited(source, condition, unknown, delay, move |rules| {
            if let Some(steps) = off_steps {
                rules["timed_scenes"][1]["steps"] = serde_json::json!(steps);
            }
        })
        .await
    }
    async fn edited(
        source: bool,
        condition: bool,
        unknown: bool,
        delay: u64,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> Self {
        let f = Fixture::customized(false, |_| {}, 128, true, delay, edit).await;
        let generation = f.actor.task_generation().await.unwrap();
        let bundle = f
            .actor
            .automation_bundle(f.controller, generation.clone())
            .await
            .unwrap();
        let doc = bundle.automation.unwrap();
        let mut store = Store::open(&f.path).unwrap();
        for (name, value) in [
            (
                "source-mode",
                (!unknown).then_some(Value::Integer(i64::from(source))),
            ),
            ("linked-mode", Some(Value::Integer(0))),
        ] {
            let version = store.logical_variable(1, name).unwrap().unwrap().version;
            store
                .accept_queue_batch_with_variables(
                    "entry-baseline",
                    &[],
                    &[],
                    100,
                    None,
                    &[Update {
                        revision: 1,
                        name: name.into(),
                        expected_version: version,
                        value,
                    }],
                )
                .unwrap();
        }
        drop(store);
        let mut feedback = Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut frames = vec![hb_protocol::RawFrame {
            address: 3,
            command: 0xd2,
            value: if condition { 0xf1 } else { 0xf2 },
        }];
        if !unknown {
            frames.push(hb_protocol::RawFrame {
                address: 3,
                command: 0xd1,
                value: if source { 0xf1 } else { 0xf2 },
            });
        }
        feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: 100,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames,
                },
            })
            .unwrap();
        let (control, receiver) = crate::automation_control::channel(f.controller, generation);
        let scenes = SceneScheduler::new(f.actor.clone(), f.controller, 16, 100).unwrap();
        let router =
            crate::deferred_router::Router::new(f.actor.clone(), f.controller, &scenes, 16)
                .unwrap();
        let web_path = f.path.with_extension("receipts.sqlite");
        let api = Arc::new(crate::web_api::Api {
            daily_backup: Default::default(),
            hap_placement:Default::default(),hap_bridges: Default::default(),
            hap_status: Default::default(),
            backup_source: None,
            publisher: None,
            origin: "http://localhost".into(),
            tokens: f.tokens.clone(),
            runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
                control: control.clone(),
                revision: 1,
                boot: f.actor.boot_id().into(),
                scenes: doc.scene_catalog(),
            })),
            receipts: Mutex::new(crate::web_receipts::Receipts::open(&web_path).unwrap()),
            drafts: None,
        });
        let model = Arc::new(Mutex::new(Model {
            source,
            condition,
            reads: VecDeque::new(),
            writes: vec![],
        }));
        let owner = crate::transport_owner::TransportOwner::new(
            f.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(model.clone()), 1),
            )]
            .into(),
        );
        let mut panels = crate::configured_rules::ConfiguredRules::load_document(
            f.actor.clone(),
            f.controller,
            &doc,
            homesense_core::rule::Budget::default(),
        )
        .await
        .unwrap()
        .panels;
        panels.bind_feedback(&feedback).unwrap();
        let mut result = Self {
            f,
            doc,
            feedback,
            receiver,
            scenes,
            router,
            control,
            api,
            web_path,
            owner,
            model,
            completion: Consumer::default(),
            modes: Default::default(),
            batches: Default::default(),
            indicators: Default::default(),
            panels,
            write_enabled: true,
        };
        // A "known" matrix starts after the ordinary runtime has projected
        // its physical baseline. A raw persisted integer alone is not truth.
        if !unknown {
            result.settle().await;
            let states = result
                .f
                .actor
                .scene_selection_snapshot(
                    result.f.controller,
                    result.f.actor.task_generation().await.unwrap(),
                    vec!["source-mode".into()],
                )
                .await
                .unwrap();
            assert_eq!(
                states["source-mode"].stored.value,
                Some(Value::Integer(i64::from(source)))
            );
            assert!(
                states["source-mode"]
                    .physical
                    .as_ref()
                    .is_some_and(|p| p.observed_ms.is_some())
            );
        }
        result
    }
    async fn tick(&mut self) {
        self.f.time.fetch_add(25, Ordering::SeqCst);
        self.panels
            .consume_feedback(&self.feedback, self.f.actor.monotonic_ms(), 5000)
            .await
            .unwrap();
        self.receiver
            .enqueue_native(self.panels.take_native_dispatch())
            .unwrap();
        self.receiver
            .poll_causal(
                &self.f.actor,
                &self.doc,
                &mut self.scenes,
                &self.feedback,
                5000,
                None,
                Some(&mut self.router),
            )
            .await;
        let ticks = self
            .scenes
            .tick_with_feedback(self.f.actor.monotonic_ms(), Some((&self.feedback, 5000)))
            .await
            .unwrap();
        for tick in ticks {
            tick.result.unwrap();
        }
        self.completion
            .tick(&self.f.actor, self.f.controller, &self.feedback)
            .await
            .unwrap();
        self.modes
            .tick(&self.f.actor, self.f.controller, &self.doc, &self.feedback)
            .await
            .unwrap();
        self.batches
            .tick(&self.f.actor, self.f.controller, &self.feedback)
            .await
            .unwrap();
        if self.write_enabled
            && let Some(outcome) = self.owner.step().await.unwrap()
        {
            outcome.result.unwrap();
        }
        if !self.model.lock().unwrap().reads.is_empty() {
            self.f.time.fetch_add(1, Ordering::SeqCst);
            let read = self.owner.receive("bus").await.unwrap();
            self.feedback.apply(&read).unwrap();
        }
        crate::scene_reconciliation::tick(
            &self.f.actor,
            self.f.controller,
            &self.doc,
            &self.scenes,
            &self.feedback,
        )
        .await
        .unwrap();
        self.indicators
            .tick(
                &self.f.actor,
                self.f.controller,
                &self.doc,
                &self.scenes,
                &self.feedback,
                5000,
            )
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    async fn drive<T: Send + 'static>(&mut self, task: tokio::task::JoinHandle<T>) -> T {
        for _ in 0..100 {
            if task.is_finished() {
                return task.await.unwrap();
            }
            self.tick().await;
        }
        panic!("HTTP/runtime did not reply");
    }
    async fn http(&mut self, input: &Request) -> (hyper::StatusCode, serde_json::Value) {
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/api/v1/keypad-actions")
            .header("host", "localhost")
            .header("origin", "http://localhost")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {SECRET}"))
            .body(hyper::body::Bytes::from(serde_json::to_vec(input).unwrap()))
            .unwrap();
        let api = self.api.clone();
        self.drive(tokio::spawn(async move {
            let response = api.handle(request).await;
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice(&body).unwrap())
        }))
        .await
    }
    async fn cancel(&mut self, run: u64) {
        let action = Action::SceneCancel {
            controller: self.f.controller,
            revision: 1,
            run,
        };
        let source = Source::capture(
            self.f
                .tokens
                .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
                .unwrap(),
            &action,
            Some("source-off"),
        )
        .unwrap();
        let control = self.control.clone();
        let reply = self
            .drive(tokio::spawn(async move {
                control.request_authorized(action, source).await
            }))
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Cancelled(_)));
    }
    async fn settle(&mut self) {
        for _ in 0..24 {
            self.tick().await;
        }
    }
    async fn click(&mut self, key: u8) {
        self.model.lock().unwrap().reads.extend([
            0xae,
            0x41,
            0xd0 + key,
            0xf1,
            0xee,
            0xae,
            0x41,
            0xd0 + key,
            0xf2,
            0xee,
        ]);
        let read = self.owner.receive("bus").await.unwrap();
        self.feedback.apply(&read).unwrap();
        self.settle().await;
    }
    async fn refresh_truth(&mut self) {
        self.f.time.fetch_add(1, Ordering::SeqCst);
        {
            let mut model = self.model.lock().unwrap();
            let source = if model.source { 0xf1 } else { 0xf2 };
            let condition = if model.condition { 0xf1 } else { 0xf2 };
            model
                .reads
                .extend([0xae, 3, 0xd1, source, 0xee, 0xae, 3, 0xd2, condition, 0xee]);
        }
        let read = self.owner.receive("bus").await.unwrap();
        self.feedback.apply(&read).unwrap();
    }
    fn value(&self) -> Option<Value> {
        Store::open(&self.f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value
    }
    fn commits(&self) -> i64 {
        rusqlite::Connection::open(&self.f.path)
            .unwrap()
            .query_row("SELECT count(*) FROM keypad_completion_audit", [], |row| {
                row.get(0)
            })
            .unwrap()
    }
    fn frames(&self) -> Vec<[u8; 5]> {
        self.model
            .lock()
            .unwrap()
            .writes
            .iter()
            .flat_map(|bytes| bytes.as_chunks::<5>().0.iter().copied())
            .collect()
    }
    async fn full(&mut self) {
        let action = Action::SceneAction {
            controller: self.f.controller,
            revision: 1,
            scene: "full".into(),
            execute: true,
        };
        let source = Source::capture(
            self.f
                .tokens
                .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
                .unwrap(),
            &action,
            None,
        )
        .unwrap();
        let control = self.control.clone();
        let reply = self
            .drive(tokio::spawn(async move {
                control.request_authorized(action, source).await
            }))
            .await
            .unwrap();
        assert!(matches!(reply, Reply::ImmediateAccepted(_)));
        self.settle().await;
        assert_eq!(self.value(), Some(Value::Integer(5)));
        let store = Store::open(&self.f.path).unwrap();
        let version = store
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .version;
        let audit = store
            .component_mode_audit(1, "linked-mode", version)
            .unwrap()
            .expect("old Full must be physically confirmed, not merely accepted or pending");
        assert_eq!(audit.policy, "full-owner");
        assert_eq!(audit.provenance, "local_written_with_feedback_inference");
        drop(store);
        assert!(
            self.f
                .actor
                .component_mode_views(
                    self.f.controller,
                    self.f.actor.task_generation().await.unwrap()
                )
                .await
                .unwrap()
                .iter()
                .any(|v| v.policies.iter().any(|p| p.id == "full-owner"))
        );
    }
    async fn close(mut self) {
        self.panels.disconnect();
        drop(self.api);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.web_path.display()));
        }
        self.f.close().await;
    }
}

#[tokio::test]
async fn real_http_known_matrix_preserves_source_toggle_and_conditional_effect_with_old_full_owner()
{
    for source in [false, true] {
        for condition in [false, true] {
            let mut h = Harness::new(source, condition, false, 0).await;
            if source && condition {
                h.full().await;
            }
            let before = h.frames().len();
            let prior = h.value();
            let input = h.f.request();
            let (status, body) = h.http(&input).await;
            assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
            assert_eq!(body["outcome"], "accepted");
            h.settle().await;
            let actual = h.model.lock().unwrap().source;
            assert_eq!(
                actual,
                !source,
                "initial source={source} condition={condition} receipt={body} frames={:?}",
                h.frames()
            );
            assert_eq!(h.model.lock().unwrap().condition, condition);
            let added = &h.frames()[before..];
            assert!(
                added.contains(&[0xae, 3, 1, if source { 0xf2 } else { 0xf1 }, 0xee]),
                "{added:?}"
            );
            assert!(
                !added.iter().any(|f| f[1] == 3 && f[2] == 2),
                "the source gesture must never actuate the condition light"
            );
            if source && condition {
                assert_eq!(h.value(), Some(Value::Integer(3)));
                assert!(added.contains(&[0xae, 0x41, 3, 0xf1, 0xee]));
                assert!(added.contains(&[0xae, 0x41, 5, 0xf2, 0xee]));
            } else {
                assert_eq!(h.value(), prior);
                assert!(!added.iter().any(|f| f[1] == 0x41 && [3, 5].contains(&f[2])));
            }
            let count = h.frames().len();
            let (status, retry) = h.http(&input).await;
            assert_eq!(status, hyper::StatusCode::OK);
            assert_eq!(retry, body);
            h.settle().await;
            assert_eq!(
                h.frames().len(),
                count,
                "an exact HTTP retry cannot toggle or apply again"
            );
            h.close().await;
        }
    }
}

#[tokio::test]
async fn real_http_unknown_source_uses_native_execution_and_applies_only_after_final_off() {
    for (initial, condition) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut h = Harness::new(initial, condition, true, 0).await;
        let prior = h.value();
        let input = h.f.request();
        let (status, body) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
        h.settle().await;
        assert_eq!(h.model.lock().unwrap().source, !initial);
        let row =
            h.f.actor
                .native_scene_status(
                    h.f.controller,
                    h.f.actor.task_generation().await.unwrap(),
                    input.request_id.clone(),
                )
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            row.status,
            homesense_storage::native_scene::Status::Observed
        );
        assert!(h.frames().contains(&[0xae, 3, 0xa1, 0xf1, 0xee]));
        assert_eq!(
            h.value(),
            if initial && condition {
                Some(Value::Integer(3))
            } else {
                prior
            }
        );
        assert!(
            !h.frames().iter().any(|f| f[1] == 3 && f[2] == 2),
            "condition feedback never authorizes a condition-light write"
        );
        if !initial || !condition {
            assert!(
                !h.frames()
                    .iter()
                    .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
            );
        }
        assert_eq!(
            h.frames().iter().rev().find(|f| f[1] == 0x41 && f[2] == 1),
            Some(&[0xae, 0x41, 1, if initial { 0xf2 } else { 0xf1 }, 0xee]),
            "source LED follows final source feedback independently of the conditional target"
        );
        let count = h.frames().len();
        let (status, retry) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::OK);
        assert_eq!(retry, body);
        h.settle().await;
        assert_eq!(
            h.frames().len(),
            count,
            "native HTTP retries cannot replay the toggle"
        );
        h.close().await;
    }
}

#[tokio::test]
async fn raw_wall_press_release_uses_same_exact_key_completion_and_alias_does_not() {
    for unknown in [false, true] {
        let mut h = Harness::new(true, true, unknown, 0).await;
        if !unknown {
            h.full().await;
        }
        let before = h.frames().len();
        h.click(1).await;
        let actual = h.model.lock().unwrap().source;
        assert!(
            !actual,
            "wall initial unknown={unknown} frames={:?}",
            h.frames()
        );
        assert!(h.model.lock().unwrap().condition);
        assert_eq!(h.value(), Some(Value::Integer(3)));
        let frames = h.frames();
        let added = &frames[before..];
        assert!(added.contains(&[0xae, 0x41, 3, 0xf1, 0xee]));
        assert!(added.contains(&[0xae, 0x41, 5, 0xf2, 0xee]));
        assert!(!added.iter().any(|f| f[1] == 3 && f[2] == 2));
        // Turning the source back on must not invalidate the independent target.
        h.click(1).await;
        assert!(h.model.lock().unwrap().source);
        assert_eq!(h.value(), Some(Value::Integer(3)));
        h.close().await;

        let mut h = Harness::new(true, true, unknown, 0).await;
        let prior = h.value();
        h.click(3).await;
        assert!(!h.model.lock().unwrap().source);
        assert_eq!(
            h.value(),
            prior,
            "same scene through another key has no completion"
        );
        assert!(
            !h.frames()
                .iter()
                .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
        );
        h.close().await;
    }
}

#[tokio::test]
async fn declared_first_write_after_fifteen_seconds_keeps_original_version_and_budget() {
    let mut h = Harness::new(true, true, false, 20_000).await;
    h.full().await;
    let input = h.f.request();
    let (status, body) = h.http(&input).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
    h.settle().await;
    assert!(
        h.model.lock().unwrap().source,
        "declared delay must not actuate early"
    );
    assert_eq!(
        h.value(),
        Some(Value::Integer(5)),
        "old owner cannot clear captured version before first write"
    );
    h.f.time.fetch_add(20_000, Ordering::SeqCst);
    h.feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: h.f.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd2,
                    value: 0xf1,
                }],
            },
        })
        .unwrap();
    h.settle().await;
    assert!(!h.model.lock().unwrap().source);
    assert_eq!(
        h.value(),
        Some(Value::Integer(3)),
        "completion uses original timeline, without a new first-write budget"
    );
    assert!(h.frames().contains(&[0xae, 0x41, 3, 0xf1, 0xee]));
    h.close().await;
}

fn off_program(led_first: bool) -> Vec<serde_json::Value> {
    let light = serde_json::json!({"source":"off/physical","device":"source-light","offset_ms":0,
        "command":{"kind":"switch","on":false}});
    let led = serde_json::json!({"source":"off/own-led","device":"key1","offset_ms":20_000,
        "command":{"kind":"panel_led","on":false}});
    if led_first {
        let mut early = led;
        early["offset_ms"] = json!(0);
        let mut later = light;
        later["offset_ms"] = json!(20_000);
        vec![early, later]
    } else {
        vec![light, led]
    }
}

#[tokio::test]
async fn all_declared_phases_must_finish_even_when_first_phase_is_only_a_led() {
    for led_first in [false, true] {
        let mut h = Harness::program(true, true, false, 0, Some(off_program(led_first))).await;
        h.full().await;
        let before = h.frames().len();
        let input = h.f.request();
        let (status, body) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
        h.settle().await;
        assert_eq!(h.model.lock().unwrap().source, led_first);
        assert_eq!(h.value(), Some(Value::Integer(5)));
        assert_eq!(
            h.commits(),
            0,
            "one written phase is not complete execution"
        );
        h.f.time.fetch_add(15_000, Ordering::SeqCst);
        h.refresh_truth().await;
        h.settle().await;
        assert_eq!(
            h.value(),
            Some(Value::Integer(5)),
            "first physical-phase TTL must not replace the declaration bound"
        );
        assert_eq!(h.commits(), 0);
        h.f.time.fetch_add(5_000, Ordering::SeqCst);
        h.refresh_truth().await;
        h.settle().await;
        assert!(!h.model.lock().unwrap().source);
        assert_eq!(h.value(), Some(Value::Integer(3)));
        assert_eq!(h.commits(), 1);
        let frames = h.frames();
        let added = &frames[before..];
        assert_eq!(
            added
                .iter()
                .filter(|f| **f == [0xae, 3, 1, 0xf2, 0xee])
                .count(),
            1
        );
        assert!(added.contains(&[0xae, 0x41, 3, 0xf1, 0xee]));
        assert!(added.contains(&[0xae, 0x41, 5, 0xf2, 0xee]));
        assert!(!added.iter().any(|f| f[1] == 3 && f[2] == 2));
        let count = frames.len();
        let (status, retry) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::OK);
        assert_eq!(retry, body);
        h.settle().await;
        assert_eq!(h.frames().len(), count);
        assert_eq!(h.commits(), 1);
        h.close().await;
    }
}

#[tokio::test]
async fn cancelling_before_or_after_first_physical_phase_never_completes_on_later_feedback() {
    for first_written in [false, true] {
        let steps = if first_written {
            off_program(false)
        } else {
            vec![json!({"source":"off/later",
            "device":"source-light","offset_ms":20_000,"command":{"kind":"switch","on":false}})]
        };
        let mut h = Harness::program(true, true, false, 0, Some(steps)).await;
        h.full().await;
        let before = h.frames().len();
        let input = h.f.request();
        let (status, body) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
        h.settle().await;
        let run = body["execution"]["runs"][0].as_u64().unwrap();
        let audit = h.scenes.audit_id(run).unwrap().to_owned();
        h.cancel(run).await;
        h.settle().await;
        if first_written {
            assert_eq!(
                h.value(),
                None,
                "cancellation must release the held Full version before the original deadline"
            );
        }
        let terminal = Store::open(&h.f.path)
            .unwrap()
            .scene_run(h.f.actor.boot_id(), &audit)
            .unwrap()
            .unwrap();
        assert!(matches!(
            terminal.status,
            homesense_storage::scene_run::Status::Cancelled
                | homesense_storage::scene_run::Status::Partial
        ));
        h.f.time.fetch_add(20_000, Ordering::SeqCst);
        h.refresh_truth().await;
        h.settle().await;
        assert_eq!(h.model.lock().unwrap().source, !first_written);
        assert_eq!(
            h.value(),
            None,
            "the cancelled receipt cannot retain the old hold after freshness expires"
        );
        assert_eq!(h.commits(), 0);
        assert!(
            !h.frames()[before..]
                .iter()
                .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
        );
        let count = h.frames().len();
        let (status, retry) = h.http(&input).await;
        assert_eq!(status, hyper::StatusCode::OK);
        assert_eq!(retry, body);
        h.settle().await;
        assert_eq!(h.frames().len(), count);
        assert_eq!(h.commits(), 0);
        h.close().await;
    }
}

#[tokio::test]
async fn missing_final_phase_retires_completion_despite_a_fresh_source_off_observation() {
    let mut h = Harness::program(true, true, false, 0, Some(off_program(false))).await;
    h.full().await;
    let before = h.frames().len();
    let input = h.f.request();
    let (status, body) = h.http(&input).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
    h.settle().await;
    let run = body["execution"]["runs"][0].as_u64().unwrap();
    let audit = h.scenes.audit_id(run).unwrap().to_owned();
    h.f.time.fetch_add(27_000, Ordering::SeqCst); // Past last phase's5s lateness, still inside35s completion budget.
    h.refresh_truth().await;
    h.settle().await;
    let terminal = Store::open(&h.f.path)
        .unwrap()
        .scene_run(h.f.actor.boot_id(), &audit)
        .unwrap()
        .unwrap();
    assert!(terminal.status.terminal());
    assert_ne!(
        terminal.status,
        homesense_storage::scene_run::Status::CommandsWritten
    );
    assert!(!h.model.lock().unwrap().source);
    assert_eq!(h.commits(), 0);
    assert_eq!(
        h.value(),
        None,
        "a failed required phase must release the captured hold"
    );
    assert!(
        !h.frames()[before..]
            .iter()
            .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
    );
    h.close().await;
}

#[tokio::test]
async fn an_expired_final_ticket_leaves_partial_execution_without_a_completion() {
    let mut h = Harness::program(true, true, false, 0, Some(off_program(false))).await;
    h.full().await;
    let before = h.frames().len();
    let input = h.f.request();
    let (status, body) = h.http(&input).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
    h.settle().await;
    let run = body["execution"]["runs"][0].as_u64().unwrap();
    let audit = h.scenes.audit_id(run).unwrap().to_owned();
    h.write_enabled = false;
    h.f.time.fetch_add(23_000, Ordering::SeqCst);
    h.refresh_truth().await;
    h.settle().await; // Last phase is admitted inside its original lateness.
    assert_eq!(h.commits(), 0);
    h.f.time.fetch_add(12_000, Ordering::SeqCst); // Declaration bound is20s+15s; final ticket still has its own live TTL.
    h.refresh_truth().await;
    h.write_enabled = true;
    h.settle().await;
    let terminal = Store::open(&h.f.path)
        .unwrap()
        .scene_run(h.f.actor.boot_id(), &audit)
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal.status,
        homesense_storage::scene_run::Status::Partial
    );
    assert_eq!(h.commits(), 0);
    assert_ne!(h.value(), Some(Value::Integer(3)));
    assert!(
        !h.frames()[before..]
            .iter()
            .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
    );
    h.close().await;
}

#[tokio::test]
async fn a_late_successful_last_write_cannot_mint_a_new_completion_window() {
    let mut h = Harness::edited(true, true, false, 0, |rules| {
        rules["timed_scenes"][1]["steps"] = json!(off_program(false));
        rules["timed_scenes"][1]["max_lateness_ms"] = json!(20_000);
    })
    .await;
    h.full().await;
    let before = h.frames().len();
    let input = h.f.request();
    let (status, body) = h.http(&input).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
    h.settle().await;
    let run = body["execution"]["runs"][0].as_u64().unwrap();
    let audit = h.scenes.audit_id(run).unwrap().to_owned();
    let started = Store::open(&h.f.path)
        .unwrap()
        .scene_run(h.f.actor.boot_id(), &audit)
        .unwrap()
        .unwrap()
        .started_ms;
    h.write_enabled = false;
    h.f.time.store(started + 34_000, Ordering::SeqCst);
    h.refresh_truth().await;
    h.settle().await;
    assert_eq!(h.commits(), 0);
    h.f.time.store(started + 35_500, Ordering::SeqCst);
    h.refresh_truth().await;
    h.write_enabled = true;
    h.settle().await;
    let terminal = Store::open(&h.f.path)
        .unwrap()
        .scene_run(h.f.actor.boot_id(), &audit)
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal.status,
        homesense_storage::scene_run::Status::CommandsWritten,
        "final write succeeds within its live ticket TTL, but after the original declaration20s+15s bound"
    );
    assert_eq!(h.commits(), 0);
    assert_eq!(h.value(), None);
    assert!(
        !h.frames()[before..]
            .iter()
            .any(|f| f[1] == 0x41 && [3, 5].contains(&f[2]))
    );
    h.close().await;
}

#[tokio::test]
async fn finished_execution_waits_for_fresh_condition_instead_of_treating_stale_as_false() {
    let steps = vec![
        json!({"source":"off/later","device":"source-light","offset_ms":20_000,
        "command":{"kind":"switch","on":false}}),
    ];
    let mut h = Harness::program(true, true, false, 0, Some(steps)).await;
    h.full().await;
    let input = h.f.request();
    let (status, body) = h.http(&input).await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED, "{body}");
    h.settle().await;
    h.f.time.fetch_add(20_000, Ordering::SeqCst);
    h.settle().await;
    assert!(!h.model.lock().unwrap().source);
    assert_eq!(h.commits(), 0);
    assert_eq!(
        h.value(),
        Some(Value::Integer(5)),
        "unknown condition retains the original captured version while waiting"
    );
    h.refresh_truth().await;
    h.settle().await;
    assert_eq!(h.value(), Some(Value::Integer(3)));
    assert_eq!(h.commits(), 1);
    h.close().await;
}
