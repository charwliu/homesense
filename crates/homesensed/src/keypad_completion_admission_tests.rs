//! Synthetic admission/receipt tests. No TCP connection or real bus is used.
use super::*;
use crate::{
    admission::{Admission, Options},
    automation_control::Action,
    keypad_actions::{Gesture, Request},
    source_auth::Source,
    web_auth::{Grant, Role, Tokens},
};
use hap::server::WriteStatus as W;
use homesense_core::{ExecutionContext, rule::Value};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::{Store, variables::Update};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const SECRET: &str = "synthetic-completion-admission-credential-0123456789";
#[path = "keypad_completion_entry_tests.rs"]
mod entry_tests;

#[tokio::test]
async fn terminal_diagnostics_are_causal_and_redact_credential_payloads() {
    const CHILD: &str = "HOMESENSE_COMPLETION_LOG_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Off)
            .filter_module("homesensed::keypad_completion", log::LevelFilter::Info)
            .write_style(env_logger::WriteStyle::Never)
            .try_init()
            .unwrap();
        let (f, _, feedback) = ready_off(128).await;
        let input = f.submission(&feedback).await;
        assert_eq!(
            f.actor
                .key_completion_proof(f.controller, input)
                .await
                .unwrap()
                .len(),
            2
        );
        f.close().await;
        let (f, _, _) = ready_off(128).await;
        f.time.store(15_100, Ordering::SeqCst);
        assert!(f.views().await.is_empty());
        assert!(f.views().await.is_empty()); // A repeated audit must not log the removed receipt twice.
        f.close().await;
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let output=tokio::task::spawn_blocking(move ||std::process::Command::new(executable)
        .args(["--exact","keypad_completion::admission_tests::terminal_diagnostics_are_causal_and_redact_credential_payloads","--nocapture"])
        .env(CHILD,"1").output().unwrap()).await.unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    let terminal = text
        .lines()
        .filter(|line| line.contains("key_completion terminal "))
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 2, "{text}");
    assert!(
        terminal.iter().any(|line| line.contains("reason=committed")
            && line.contains("source_commands=1 source_written=1")),
        "{text}"
    );
    assert!(
        terminal
            .iter()
            .any(|line| line.contains("reason=original_deadline")),
        "{text}"
    );
    for forbidden in [SECRET, "Bearer", "linked-mode", "source-off-effect"] {
        assert!(
            !text.contains(forbidden),
            "diagnostics leaked an authority or configuration payload"
        );
    }
}
struct Fixture {
    path: PathBuf,
    actor: Admission,
    controller: Uuid,
    tokens: Arc<Tokens>,
    credential: Uuid,
    time: &'static AtomicU64,
}
impl Fixture {
    async fn new(combined: bool, scope: impl FnOnce(&mut Grant)) -> Self {
        Self::capacity(combined, scope, 128).await
    }
    async fn capacity(combined: bool, scope: impl FnOnce(&mut Grant), capacity: usize) -> Self {
        Self::configured(combined, scope, capacity, false, 0).await
    }
    async fn configured(
        combined: bool,
        scope: impl FnOnce(&mut Grant),
        capacity: usize,
        full_policy: bool,
        delay: u64,
    ) -> Self {
        Self::customized(combined, scope, capacity, full_policy, delay, |_| {}).await
    }
    async fn customized(
        combined: bool,
        scope: impl FnOnce(&mut Grant),
        capacity: usize,
        full_policy: bool,
        delay: u64,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> Self {
        let mut config = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}
        ]}],"devices":[
            {"id":"source-light","name":"Source","module_id":"relay","channel":1,"enabled":true},
            {"id":"condition-light","name":"Condition","module_id":"relay","channel":2,"enabled":true}
        ],"keypads":[{"id":"panel","name":"Panel","module_id":"panel-module","enabled":true,"keys":[
            {"number":1,"label":"Source","led_endpoint":{"id":"key1","name":"One","enabled":true},
             "action":{"kind":"scene_toggle","scene":"source-on","review_evidence":"Synthetic exact key"},
             "selection_completion":{"id":"source-off-effect","after":{"device":"source-light","on":false},
                "when":[{"device":"condition-light","on":true}],"variable":"linked-mode","value":3,
                "indicators":[{"device":"key3","on":true},{"device":"key5","on":false}],"max_age_ms":5_000,
                "review_evidence":"Synthetic key-specific conditional effect"}},
            {"number":3,"label":"Alias","led_endpoint":{"id":"key3","name":"Three","enabled":true},
             "action":{"kind":"scene_toggle","scene":"source-on","review_evidence":"Alias deliberately has no completion"}},
            {"number":5,"label":"Other","led_endpoint":{"id":"key5","name":"Five","enabled":true}}
        ]}]});
        if combined {
            config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}));
            config["devices"].as_array_mut().unwrap().push(json!({"id":"condition-level","name":"Combined condition","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100,"supply":{"module_id":"relay","channel":2}}));
            config["keypads"][0]["keys"][0]["selection_completion"]["when"][0]["device"] =
                json!("condition-level");
        }
        let mut rules = json!({"schema_version":1,"configuration_revision":1,
            "variables":[{"id":"source-mode","kind":"integer","lifetime":"persistent"},{"id":"linked-mode","kind":"integer","lifetime":"persistent"}],"rules":[],
            "timed_scenes":[
                {"id":"source-on","enabled":true,"review_evidence":"Synthetic On","max_lateness_ms":5_000,"steps":[{"source":"source/on","device":"source-light","offset_ms":0,"command":{"kind":"switch","on":true}}]},
                {"id":"source-off","enabled":true,"review_evidence":"Synthetic Off","max_lateness_ms":5_000,"steps":[{"source":"source/off","device":"source-light","offset_ms":0,"command":{"kind":"switch","on":false}}]}
            ],"scene_switch_groups":[{"id":"source-group","variable":"source-mode","review_evidence":"Source only",
                "members":[{"scene":"source-on","value":1,"off_scene":"source-off"}],
                "reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5_000,"review_evidence":"Source power only"}}]});
        if full_policy {
            config["keypads"][0]["keys"][2]["action"] = json!({"kind":"scene_toggle",
                "scene":"source-on","review_evidence":"Synthetic unused mapped key"});
            for number in [2, 4, 6] {
                config["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({
                    "number":number,"label":format!("Unused {number}"),
                    "led_endpoint":{"id":format!("key{number}"),"name":"Unused","enabled":true},
                    "action":{"kind":"scene_toggle","scene":"source-on","review_evidence":"Synthetic unused mapped key"}
                }));
            }
            config["keypads"][0]["event_control"] = json!({"owner":"homesense",
                "legacy_writer_isolated":true,"review_evidence":"Synthetic isolated in-memory bus only"});
            rules["indicator_links"] = json!([{"id":"source-led-owner","enabled":true,
                "review_evidence":"Synthetic independent source selection indicator",
                "lights":["source-light"],"leds":["key1"],"scene":"source-on"}]);
            config["keypads"][0]["keys"][0]["local_indicator"] = json!({"kind":"scene_selection","group":"source-group","value":1,"selected_color":"red","unselected_color":"green","review_evidence":"Synthetic source cue"});
            rules["immediate_scenes"] = json!([{"id":"full","enabled":true,"review_evidence":"Synthetic original Full owner",
                "activate":[{"source":"full/mode","action":{"kind":"set","variable":"linked-mode","value":{"kind":"integer","value":5}}},
                    {"source":"full/source","action":{"kind":"command","device":"source-light","command":{"kind":"switch","on":true}}},
                    {"source":"full/condition","action":{"kind":"command","device":"condition-light","command":{"kind":"switch","on":true}}},
                    {"source":"full/day-led","action":{"kind":"command","device":"key3","command":{"kind":"panel_led","on":false}}},
                    {"source":"full/full-led","action":{"kind":"command","device":"key5","command":{"kind":"panel_led","on":true}}}]}]);
            rules["component_mode_confirmations"] = json!([{"id":"full-owner","variable":"linked-mode","scenes":["full"],"first_value":5,
                "confirm_when_known":true,"timeout_ms":5000,"max_age_ms":5000,"review_evidence":"Synthetic Full physical vector",
                "postconditions":[{"device":"source-light","on":true},{"device":"condition-light","on":true}]}]);
        }
        rules["timed_scenes"][0]["steps"][0]["offset_ms"] = json!(delay);
        rules["timed_scenes"][1]["steps"][0]["offset_ms"] = json!(delay);
        edit(&mut rules);
        let path =
            std::env::temp_dir().join(format!("key-completion-owner-{}.sqlite", Uuid::new_v4()));
        let mut store = Store::open(&path).unwrap();
        let bundle = store
            .publish_automation(
                None,
                &serde_json::to_vec(&config).unwrap(),
                &serde_json::to_vec(&rules).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        store
            .accept_queue_batch_with_variables(
                "seed",
                &[],
                &[],
                0,
                None,
                &[Update {
                    revision: 1,
                    name: "source-mode".into(),
                    expected_version: 0,
                    value: Some(Value::Integer(1)),
                }],
            )
            .unwrap();
        drop(store);
        let controller = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let tokens = Arc::new(Tokens::default());
        let mut grant = Grant {
            controller,
            role: Role::Member,
            devices: bundle
                .configuration
                .devices()
                .keys()
                .map(|id| id.as_str().to_owned())
                .collect(),
            scenes: bundle
                .automation
                .as_ref()
                .unwrap()
                .timed_scenes
                .keys()
                .chain(bundle.automation.as_ref().unwrap().immediate_scenes.keys())
                .cloned()
                .collect(),
        };
        scope(&mut grant);
        tokens.insert(credential, SECRET, grant).unwrap();
        let time = Box::leak(Box::new(AtomicU64::new(100)));
        let actor = Admission::open_with_test_time(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                command_capacity: capacity,
                ..Default::default()
            },
            |_| true,
            time,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        actor.task_generation().await.unwrap();
        // Seed a current-boot known component after generation reset.
        Store::open(&path)
            .unwrap()
            .accept_queue_batch_with_variables(
                "seed-current",
                &[],
                &[],
                100,
                None,
                &[Update {
                    revision: 1,
                    name: "linked-mode".into(),
                    expected_version: 0,
                    value: Some(Value::Integer(5)),
                }],
            )
            .unwrap();
        Self {
            path,
            actor,
            controller,
            tokens,
            credential,
            time,
        }
    }
    fn request(&self) -> Request {
        Request {
            revision: "1".into(),
            keypad: "panel".into(),
            key: 1,
            request_id: Uuid::new_v4().to_string(),
            gesture: Gesture::Activate,
        }
    }
    fn source(&self, request: &Request) -> Source {
        Source::capture(
            self.tokens
                .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
                .unwrap(),
            &Action::KeypadAction {
                controller: self.controller,
                request: request.clone(),
            },
            None,
        )
        .unwrap()
    }
    async fn capture(
        &self,
        request: &Request,
        source: &Source,
    ) -> Result<Arc<AuthorizedCapture>, W> {
        self.actor
            .capture_key_completion(
                self.controller,
                self.actor.task_generation().await.unwrap(),
                request.clone(),
                source.clone(),
            )
            .await
    }
    fn derive(&self, source: &Source, capture: Arc<AuthorizedCapture>) -> Source {
        let origin = crate::keypad_origin::Origin::new(
            capture.receipt.endpoint.clone(),
            1,
            capture.receipt.request_id.clone(),
            false,
            self.time.load(Ordering::SeqCst),
            None,
            vec![],
            None,
            Some(capture),
        )
        .unwrap();
        source
            .derive(&Action::SceneAction {
                controller: self.controller,
                revision: 1,
                scene: "source-on".into(),
                execute: false,
            })
            .unwrap()
            .with_keypad(origin)
    }
    async fn submit(&self, source: Option<Source>, device: &str, on: bool) -> Result<Vec<u64>, W> {
        self.actor
            .with_source(source)
            .submit(Batch {
                controller: self.controller,
                intents: vec![Intent {
                    device: DeviceId::new(device).unwrap(),
                    command: Command::Switch(on),
                }],
                subscriptions: vec![],
            })
            .await
    }
    async fn views(&self) -> Vec<View> {
        self.actor
            .key_completion_views(self.controller, self.actor.task_generation().await.unwrap())
            .await
            .unwrap()
    }
    async fn written(&self) -> u64 {
        self.time.fetch_add(21, Ordering::SeqCst);
        let ticket = self.actor.dispatch().await.unwrap().unwrap();
        self.actor
            .finish_write(ticket.command.id, true)
            .await
            .unwrap();
        ticket.command.id
    }
    async fn close(self) {
        self.actor.shutdown().await.unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        let _ = std::fs::remove_file(self.path.with_extension("runtime.lock"));
    }
    async fn feedback(&self, source: bool, condition: bool) -> crate::feedback::Feedback {
        let generation = self.actor.task_generation().await.unwrap();
        let bundle = self
            .actor
            .automation_bundle(self.controller, generation.clone())
            .await
            .unwrap();
        let mut feedback = crate::feedback::Feedback::new(
            &bundle.configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(generation.boot_id()).unwrap().as_u128(),
            generation.epochs.transport,
        )
        .unwrap();
        let at = self.time.fetch_add(1, Ordering::SeqCst) + 1;
        feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![
                        hb_protocol::RawFrame {
                            address: 3,
                            command: 0xd1,
                            value: if source { 0xf1 } else { 0xf2 },
                        },
                        hb_protocol::RawFrame {
                            address: 3,
                            command: 0xd2,
                            value: if condition { 0xf1 } else { 0xf2 },
                        },
                    ],
                },
            })
            .unwrap();
        feedback
    }
    async fn submission(&self, feedback: &crate::feedback::Feedback) -> Submission {
        let view = self.views().await.pop().unwrap();
        let config = self
            .actor
            .automation_bundle(self.controller, view.evidence.generation.clone())
            .await
            .unwrap()
            .configuration;
        Submission {
            sequence: view.sequence,
            source: view.source,
            proof: FeedbackProof::prepare(
                view.evidence,
                &config,
                feedback,
                self.actor.monotonic_ms(),
            )
            .unwrap(),
        }
    }
}

#[tokio::test]
async fn actual_completion_rpc_commits_once_delivers_only_leds_and_hands_off_to_condition_only_ownership()
 {
    use std::sync::Mutex;
    struct Bytes(Arc<Mutex<Vec<Vec<u8>>>>);
    impl std::io::Write for Bytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl std::io::Read for Bytes {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    }
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source.keypad().unwrap().seal_completion(ids, vec![]);
    f.written().await;
    let feedback = f.feedback(false, true).await;
    let input = f.submission(&feedback).await;
    let sequence = input.sequence;
    let proof = FeedbackProof::prepare(
        input.proof.evidence.clone(),
        &f.actor
            .automation_bundle(f.controller, input.proof.evidence.generation.clone())
            .await
            .unwrap()
            .configuration,
        &feedback,
        f.actor.monotonic_ms(),
    )
    .unwrap();
    let duplicate = Submission {
        sequence,
        source: source.clone(),
        proof,
    };
    let result = f
        .actor
        .key_completion_proof(f.controller, input)
        .await
        .unwrap();
    assert_eq!(result.len(), 2);
    let store = Store::open(&f.path).unwrap();
    let value = store.logical_variable(1, "linked-mode").unwrap().unwrap();
    assert_eq!(value.value, Some(Value::Integer(3)));
    assert_eq!(value.version, 2);
    assert!(
        f.actor
            .key_completion_proof(f.controller, duplicate)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.views().await.is_empty());
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut connection = hb_transport::Connection::for_simulation(Bytes(bytes.clone()), 1);
    let mut tickets = vec![];
    f.time.fetch_add(21, Ordering::SeqCst);
    for expected in &result {
        let ticket = if tickets.is_empty() {
            f.actor.dispatch().await.unwrap().unwrap()
        } else {
            f.actor.extend_write(result[0]).await.unwrap().unwrap()
        };
        assert_eq!(ticket.command.id, *expected);
        assert!(matches!(ticket.command.command, Command::PanelLed(_)));
        assert!(ticket.feedback_barrier.is_some());
        assert!(
            ticket
                .source_authorization
                .as_ref()
                .unwrap()
                .same_authority(&source)
        );
        tickets.push(ticket);
    }
    crate::transport_owner::send_ticket_batch(&tickets, &mut connection, || {
        f.time.load(Ordering::SeqCst)
    })
    .unwrap();
    for ticket in &tickets {
        f.actor.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert_eq!(
        *bytes.lock().unwrap(),
        vec![vec![0xae, 0x41, 3, 0xf1, 0xee, 0xae, 0x41, 5, 0xf2, 0xee]]
    );
    let generation = f.actor.task_generation().await.unwrap();
    let views = f
        .actor
        .component_mode_views(f.controller, generation.clone())
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].policies[0]
            .postconditions
            .iter()
            .map(|c| c.device.as_str())
            .collect::<Vec<_>>(),
        vec!["condition-light"]
    );
    f.submit(None, "source-light", true).await.unwrap();
    f.written().await;
    f.actor
        .component_mode_views(f.controller, generation)
        .await
        .unwrap();
    assert_eq!(
        store
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3)),
        "new source On does not erase independent condition ownership"
    );
    drop(store);
    f.close().await;
}

#[tokio::test]
async fn full_effect_authority_is_required_before_any_source_command_or_variable_mutation() {
    for (combined, missing) in [
        (false, "condition-light"),
        (false, "key3"),
        (false, "key5"),
        (true, "condition-light"),
    ] {
        let f = Fixture::new(combined, |grant| {
            grant.devices.remove(missing);
        })
        .await;
        let request = f.request();
        let source = f.source(&request);
        assert!(
            matches!(
                f.capture(&request, &source).await,
                Err(W::InsufficientPrivileges)
            ),
            "{missing}"
        );
        assert!(f.actor.dispatch().await.unwrap().is_none());
        assert_eq!(
            Store::open(&f.path)
                .unwrap()
                .logical_variable(1, "linked-mode")
                .unwrap()
                .unwrap()
                .value,
            Some(Value::Integer(5))
        );
        f.close().await;
    }
}

async fn ready_off(capacity: usize) -> (Fixture, Source, crate::feedback::Feedback) {
    let f = Fixture::capacity(false, |_| {}, capacity).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source.keypad().unwrap().seal_completion(ids, vec![]);
    f.written().await;
    let feedback = f.feedback(false, true).await;
    (f, source, feedback)
}

#[tokio::test]
async fn queue_busy_retries_cannot_restart_the_first_complete_proof_window() {
    let (f, _, feedback) = ready_off(1).await;
    let first = f.actor.monotonic_ms();
    for elapsed in [0, 1_500] {
        f.time.store(first + elapsed, Ordering::SeqCst);
        let input = f.submission(&feedback).await;
        assert!(matches!(
            f.actor.key_completion_proof(f.controller, input).await,
            Err(W::ResourceBusy)
        ));
        assert_eq!(
            Store::open(&f.path)
                .unwrap()
                .logical_variable(1, "linked-mode")
                .unwrap()
                .unwrap()
                .version,
            1
        );
        assert!(f.actor.dispatch().await.unwrap().is_none());
    }
    f.time.store(first + 2_000, Ordering::SeqCst);
    assert!(
        f.views().await.is_empty(),
        "retry at1500ms cannot move the original2000ms cutoff"
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn stale_or_relabelled_completion_proofs_cannot_commit_and_a_valid_retry_uses_the_retained_capability()
 {
    let (f, source, mut feedback) = ready_off(128).await;
    let mut view = f.views().await.pop().unwrap();
    let config = f
        .actor
        .automation_bundle(f.controller, view.evidence.generation.clone())
        .await
        .unwrap()
        .configuration;
    view.evidence.variable.expected_version += 1;
    let bad = Submission {
        sequence: view.sequence,
        source: source.clone(),
        proof: FeedbackProof::prepare(view.evidence, &config, &feedback, f.actor.monotonic_ms())
            .unwrap(),
    };
    assert!(matches!(
        f.actor.key_completion_proof(f.controller, bad).await,
        Err(W::InvalidValueInRequest)
    ));
    let stale = f.submission(&feedback).await;
    let at = f.time.fetch_add(1, Ordering::SeqCst) + 1;
    feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
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
    assert!(matches!(
        f.actor.key_completion_proof(f.controller, stale).await,
        Err(W::ResourceBusy)
    ));
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .version,
        1
    );
    let mut foreign = f.submission(&feedback).await;
    foreign.source = f.source(&f.request());
    assert!(matches!(
        f.actor.key_completion_proof(f.controller, foreign).await,
        Err(W::InsufficientPrivileges)
    ));
    let good = f.submission(&feedback).await;
    assert_eq!(
        f.actor
            .key_completion_proof(f.controller, good)
            .await
            .unwrap()
            .len(),
        2
    );
    f.close().await;
}

#[tokio::test]
async fn known_false_condition_retires_once_without_mutating_target_or_enqueuing_leds() {
    let (f, _, _) = ready_off(128).await;
    let feedback = f.feedback(false, false).await;
    let input = f.submission(&feedback).await;
    assert_eq!(input.proof.decision, Decision::NoEffect);
    assert!(
        f.actor
            .key_completion_proof(f.controller, input)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.views().await.is_empty());
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(5))
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn already_observed_condition_at_its_exact_boundary_can_complete_without_query_or_borrowing_a_receipt()
 {
    let f = Fixture::new(false, |_| {}).await;
    let previous = f.submit(None, "condition-light", true).await.unwrap()[0];
    f.written().await;
    let mut feedback = f.feedback(true, true).await;
    let observed = f.time.load(Ordering::SeqCst);
    let generation = f.actor.task_generation().await.unwrap();
    Store::open(&f.path)
        .unwrap()
        .transition_command(
            generation.boot_id(),
            previous,
            homesense_storage::journal::Status::Written,
            homesense_storage::journal::Status::Observed,
            observed,
        )
        .unwrap();
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source
        .keypad()
        .unwrap()
        .seal_completion(ids.clone(), vec![]);
    f.written().await;
    let at = f.time.fetch_add(1, Ordering::SeqCst) + 1;
    feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd1,
                    value: 0xf2,
                }],
            },
        })
        .unwrap();
    let input = f.submission(&feedback).await;
    assert_eq!(
        input.proof.lease.observations[&DeviceId::new("condition-light").unwrap()],
        Some(observed)
    );
    assert!(!input.proof.evidence.written.contains_key(&previous));
    assert_eq!(
        f.actor
            .key_completion_proof(f.controller, input)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    f.close().await;
}

#[tokio::test]
async fn production_consumers_commit_and_keep_condition_ownership_current_without_static_component_policies()
 {
    let (f, _, mut feedback) = ready_off(128).await;
    let mut completion = Consumer::default();
    completion
        .tick(&f.actor, f.controller, &feedback)
        .await
        .unwrap();
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    f.written().await;
    f.written().await;
    let bundle = f
        .actor
        .automation_bundle(f.controller, f.actor.task_generation().await.unwrap())
        .await
        .unwrap();
    let document = bundle.automation.unwrap();
    assert!(document.component_mode_confirmations.is_empty());
    let mut modes = crate::component_modes::Consumer::default();
    for (at, source, condition) in [(300, 0xf1, 0xf1), (400, 0xf1, 0xf2)] {
        f.time.store(at, Ordering::SeqCst);
        feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![
                        hb_protocol::RawFrame {
                            address: 3,
                            command: 0xd1,
                            value: source,
                        },
                        hb_protocol::RawFrame {
                            address: 3,
                            command: 0xd2,
                            value: condition,
                        },
                    ],
                },
            })
            .unwrap();
        modes
            .tick(&f.actor, f.controller, &document, &feedback)
            .await
            .unwrap();
        assert_eq!(
            Store::open(&f.path)
                .unwrap()
                .logical_variable(1, "linked-mode")
                .unwrap()
                .unwrap()
                .value,
            if condition == 0xf1 {
                Some(Value::Integer(3))
            } else {
                None
            }
        );
    }
    assert!(
        f.actor.dispatch().await.unwrap().is_none(),
        "inference and its refresh issue no control or query"
    );
    let count: i64 = rusqlite::Connection::open(&f.path)
        .unwrap()
        .query_row("SELECT count(*) FROM command_journal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 3,
        "only the original source write and two completion LEDs exist"
    );
    f.close().await;
}

#[tokio::test]
async fn completion_handoff_uses_effective_age_without_waiting_for_a_later_consumer_refresh() {
    let (f, _, mut feedback) = ready_off(128).await;
    feedback.set_managed_max_age_ms(500).unwrap();
    let input = f.submission(&feedback).await;
    let observed =
        input.proof.lease.observations[&DeviceId::new("condition-light").unwrap()].unwrap();
    assert_eq!(input.proof.observation_max_age_ms(), 500);
    assert_eq!(
        f.actor
            .key_completion_proof(f.controller, input)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    // No component proof or consumer is run after this handoff. Admission's
    // ordinary deadline audit must invalidate it at the effective age itself.
    f.time.store(observed + 500, Ordering::SeqCst);
    f.actor.task_generation().await.unwrap();
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        None
    );
    f.close().await;
}

#[tokio::test]
async fn changing_managed_age_between_preparation_and_commit_cannot_use_old_completion_proof() {
    let (f, _, mut feedback) = ready_off(128).await;
    let old = f.submission(&feedback).await;
    feedback.set_managed_max_age_ms(500).unwrap();
    assert_eq!(
        f.actor.key_completion_proof(f.controller, old).await,
        Err(W::ResourceBusy)
    );
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(5))
    );
    assert!(
        f.actor.dispatch().await.unwrap().is_none(),
        "invalid proof cannot enqueue any LED"
    );
    let current = f.submission(&feedback).await;
    assert_eq!(current.proof.observation_max_age_ms(), 500);
    assert_eq!(
        f.actor
            .key_completion_proof(f.controller, current)
            .await
            .unwrap()
            .len(),
        2
    );
    f.close().await;
}

#[tokio::test]
async fn an_equal_grant_or_an_alias_key_cannot_borrow_the_exact_request_capture() {
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let foreign = f.source(&f.request());
    assert!(!original.same_authority(&foreign));
    let forged = f.derive(&foreign, capture);
    assert!(matches!(
        f.submit(Some(forged), "source-light", false).await,
        Err(W::InsufficientPrivileges)
    ));
    assert!(f.actor.dispatch().await.unwrap().is_none());
    let mut alias = f.request();
    alias.key = 3;
    let alias_source = f.source(&alias);
    assert!(matches!(
        f.capture(&alias, &alias_source).await,
        Err(W::InvalidValueInRequest)
    ));
    assert!(matches!(
        f.capture(&request, &alias_source).await,
        Err(W::InsufficientPrivileges)
    ));
    assert!(f.views().await.is_empty());
    f.close().await;
}

#[tokio::test]
async fn original_writer_callbacks_and_logical_receipt_survive_after_dispatch_source_cleanup() {
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    assert_eq!(capture.receipt.variable.expected_version, 1);
    let source = f.derive(&original, capture);
    assert!(source.derived_from(&original));
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source
        .keypad()
        .unwrap()
        .seal_completion(ids.clone(), vec![]);
    assert!(
        f.views().await.is_empty(),
        "Accepted is not a physical write"
    );
    assert_eq!(f.written().await, ids[0]);
    // A normal diagnostic message has already allowed command source cleanup.
    // The retained completion must still own the exact original Source.
    let views = f.views().await;
    assert_eq!(views.len(), 1);
    assert!(views[0].source.same_authority(&source));
    assert_eq!(views[0].evidence.variable.expected_version, 1);
    assert_eq!(
        views[0]
            .evidence
            .written
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        ids
    );
    f.close().await;
}

#[tokio::test]
async fn foreign_automated_controls_retire_receipts_without_a_manual_version_change() {
    for (device, action, survives) in [
        ("source-light", Command::Switch(true), false),
        ("condition-light", Command::Switch(true), false),
        ("condition-light", Command::Query, true),
        ("key5", Command::PanelLed(false), true),
    ] {
        let f = Fixture::new(false, |_| {}).await;
        let request = f.request();
        let original = f.source(&request);
        let capture = f.capture(&request, &original).await.unwrap();
        let source = f.derive(&original, capture);
        let ids = f
            .submit(Some(source.clone()), "source-light", false)
            .await
            .unwrap();
        source.keypad().unwrap().seal_completion(ids, vec![]);
        f.written().await;
        assert_eq!(f.views().await.len(), 1);
        let generation = f.actor.task_generation().await.unwrap();
        // A separate rule owner records its accepted command. This deliberately
        // changes no Admission manual fence, as real automated rules do not.
        Store::open(&f.path)
            .unwrap()
            .accept_commands(
                generation.boot_id(),
                &[QueuedCommand {
                    id: 10_000,
                    device_id: DeviceId::new(device).unwrap(),
                    command: action,
                    origin: homesense_core::queue::Origin::Rule,
                    epochs: generation.epochs,
                    deadline_ms: 1_000,
                    run_id: None,
                }],
                f.time.load(Ordering::SeqCst),
            )
            .unwrap();
        assert_eq!(
            !f.views().await.is_empty(),
            survives,
            "{device}: {action:?}"
        );
        f.close().await;
    }
}

#[tokio::test]
async fn revoke_supersession_and_original_deadline_retire_real_owner_receipts() {
    for case in 0..3 {
        let f = Fixture::new(false, |_| {}).await;
        let request = f.request();
        let original = f.source(&request);
        let capture = f.capture(&request, &original).await.unwrap();
        let source = f.derive(&original, capture);
        let ids = f
            .submit(Some(source.clone()), "source-light", false)
            .await
            .unwrap();
        source.keypad().unwrap().seal_completion(ids, vec![]);
        f.written().await;
        assert_eq!(f.views().await.len(), 1);
        match case {
            0 => f.tokens.revoke(f.credential).unwrap(),
            1 => source.keypad().unwrap().invalidate(),
            _ => {
                f.time.store(15_100, Ordering::SeqCst);
            }
        }
        assert!(f.views().await.is_empty(), "case {case}");
        f.close().await;
    }
}

#[tokio::test]
async fn target_version_and_competing_dependency_override_are_not_recaptured() {
    for case in 0..2 {
        let f = Fixture::new(false, |_| {}).await;
        let request = f.request();
        let original = f.source(&request);
        let capture = f.capture(&request, &original).await.unwrap();
        let source = f.derive(&original, capture);
        let ids = f
            .submit(Some(source.clone()), "source-light", false)
            .await
            .unwrap();
        source.keypad().unwrap().seal_completion(ids, vec![]);
        f.written().await;
        assert_eq!(f.views().await.len(), 1);
        if case == 0 {
            Store::open(&f.path)
                .unwrap()
                .accept_queue_batch_with_variables(
                    "newer-mode",
                    &[],
                    &[],
                    0,
                    None,
                    &[Update {
                        revision: 1,
                        name: "linked-mode".into(),
                        expected_version: 1,
                        value: Some(Value::Integer(4)),
                    }],
                )
                .unwrap();
        } else {
            f.submit(None, "condition-light", true).await.unwrap();
        }
        assert!(f.views().await.is_empty(), "case {case}");
        f.close().await;
    }
}

#[tokio::test]
async fn ordinary_off_and_preparation_alone_do_not_create_effect_receipts() {
    let f = Fixture::new(false, |_| {}).await;
    f.submit(None, "source-light", false).await.unwrap();
    f.written().await;
    assert!(f.views().await.is_empty());
    let request = f.request();
    let source = f.source(&request);
    let generation = f.actor.task_generation().await.unwrap();
    let bundle = f
        .actor
        .automation_bundle(f.controller, generation.clone())
        .await
        .unwrap();
    let feedback = f.feedback(false, false).await;
    let result = crate::keypad_actions::prepare(
        &f.actor,
        &generation,
        &bundle.automation.unwrap(),
        f.controller,
        &request,
        &source,
        &feedback,
    )
    .await;
    let prepared = result.unwrap();
    assert!(
        prepared
            .source
            .unwrap()
            .keypad()
            .unwrap()
            .completion
            .is_some()
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    assert!(f.views().await.is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_changed_linked_snapshot_retires_only_the_effect_and_preserves_the_requested_light_action()
 {
    for variable_change in [false, true] {
        let f = Fixture::new(false, |_| {}).await;
        let request = f.request();
        let original = f.source(&request);
        let capture = f.capture(&request, &original).await.unwrap();
        let source = f.derive(&original, capture);
        if variable_change {
            Store::open(&f.path)
                .unwrap()
                .accept_queue_batch_with_variables(
                    "newer-mode",
                    &[],
                    &[],
                    0,
                    None,
                    &[Update {
                        revision: 1,
                        name: "linked-mode".into(),
                        expected_version: 1,
                        value: Some(Value::Integer(4)),
                    }],
                )
                .unwrap();
        } else {
            f.submit(None, "condition-light", true).await.unwrap();
            f.written().await;
        }
        let ids = f
            .submit(Some(source.clone()), "source-light", false)
            .await
            .unwrap();
        source
            .keypad()
            .unwrap()
            .seal_completion(ids.clone(), vec![]);
        assert_eq!(f.written().await, ids[0]);
        assert!(f.actor.dispatch().await.unwrap().is_none());
        assert!(f.views().await.is_empty());
        f.close().await;
    }
}

#[tokio::test]
async fn a_written_receipt_is_not_reconstructed_or_replayed_after_owner_restart() {
    let mut f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source.keypad().unwrap().seal_completion(ids, vec![]);
    f.written().await;
    assert_eq!(f.views().await.len(), 1);
    f.actor.shutdown().await.unwrap();
    f.actor = Admission::open_with_test_time(
        f.path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
        f.time,
    )
    .await
    .unwrap();
    f.actor.synchronized(1, 1).await.unwrap();
    assert!(f.views().await.is_empty());
    let variable = Store::open(&f.path)
        .unwrap()
        .logical_variable(1, "linked-mode")
        .unwrap()
        .unwrap();
    assert_eq!(
        variable.value, None,
        "completion-only target never survives a boot as known"
    );
    assert_eq!(
        variable.version, 2,
        "startup invalidates the exact old version once"
    );
    f.actor.task_generation().await.unwrap();
    assert_eq!(
        Store::open(&f.path)
            .unwrap()
            .logical_variable(1, "linked-mode")
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    assert!(f.submit(Some(source), "source-light", false).await.is_err());
    assert!(f.actor.dispatch().await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn retired_effects_do_not_rearm_or_block_later_phases_of_the_original_operation() {
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    source.keypad().unwrap().seal_completion(ids, vec![]);
    f.written().await;
    assert_eq!(f.views().await.len(), 1);
    f.time.store(15_100, Ordering::SeqCst);
    assert!(f.views().await.is_empty());
    let later = f
        .submit(Some(source.clone()), "source-light", true)
        .await
        .unwrap();
    assert_eq!(f.written().await, later[0]);
    assert!(
        f.views().await.is_empty(),
        "a later phase cannot restart the retired effect budget"
    );
    assert!(
        source.keypad().unwrap().current(),
        "retiring this effect must not invalidate the original source"
    );
    f.close().await;
}

#[tokio::test]
async fn a_source_capture_cannot_be_relabelled_with_another_request_id() {
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let origin = crate::keypad_origin::Origin::new(
        capture.receipt.endpoint.clone(),
        1,
        "different-request".into(),
        false,
        100,
        None,
        vec![],
        None,
        Some(capture),
    )
    .unwrap();
    let source = original
        .derive(&Action::SceneAction {
            controller: f.controller,
            revision: 1,
            scene: "source-on".into(),
            execute: false,
        })
        .unwrap()
        .with_keypad(origin);
    assert!(matches!(
        f.submit(Some(source), "source-light", false).await,
        Err(W::InsufficientPrivileges)
    ));
    assert!(f.actor.dispatch().await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn cancellation_of_a_declared_timeline_retires_the_conditional_receipt() {
    let f = Fixture::new(false, |_| {}).await;
    let request = f.request();
    let original = f.source(&request);
    let capture = f.capture(&request, &original).await.unwrap();
    let source = f.derive(&original, capture);
    let ids = f
        .submit(Some(source.clone()), "source-light", false)
        .await
        .unwrap();
    f.written().await;
    let run = Uuid::new_v4().to_string();
    let mut store = Store::open(&f.path).unwrap();
    store
        .begin_manual_scene_batch_with_variables(
            f.actor.boot_id(),
            1,
            &[homesense_storage::scene_run::ManualStart {
                run: run.clone(),
                definition: "source-on".into(),
            }],
            100,
            &[],
        )
        .unwrap();
    source
        .keypad()
        .unwrap()
        .seal_completion(ids, vec![run.clone()]);
    assert!(
        f.views().await.is_empty(),
        "a still-pending timeline is not complete"
    );
    store
        .checkpoint_scene_run(
            f.actor.boot_id(),
            &run,
            0,
            homesense_storage::scene_run::Status::Cancelled,
            150,
        )
        .unwrap();
    assert!(f.views().await.is_empty());
    assert!(
        source.keypad().unwrap().completion_started(),
        "the consumed gesture cannot rearm after cancellation"
    );
    f.close().await;
}
