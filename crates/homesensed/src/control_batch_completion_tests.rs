//! Conditional writer-gate tests, downstream of source authorization. The key
//! entry remains disabled until the full Admission commit path is integrated.
use super::*;
use crate::{
    automation_control::Action,
    keypad_actions::{Gesture, Request},
    keypad_completion::{Evidence, FeedbackProof},
    web_auth::{Grant, Role, Tokens},
};
use homesense_core::{
    LevelTransition, Percent,
    queue::Origin,
    rule::{Value, ValueType},
};
use homesense_storage::{keypad_completion::Commit, variables::Update};
use std::{path::PathBuf, sync::Arc};

const SECRET: &str = "synthetic-conditional-writer-secret-0123456789";
struct Fixture {
    owner: Owner,
    feedback: Feedback,
    actor: Admission,
    root: PathBuf,
    boot: u128,
    config: Configuration,
    generation: TaskGeneration,
    controller: Uuid,
    tokens: Arc<Tokens>,
    credential: Uuid,
    source: Source,
    originals: Vec<QueuedCommand>,
    evidence: Evidence,
    leds: Vec<QueuedCommand>,
}
impl Fixture {
    async fn new() -> Self {
        Self::deadline(15_000).await
    }
    async fn deadline(deadline: u64) -> Self {
        let (_, mut feedback, actor, root, boot) = super::budget_tests::fixture().await;
        let generation = actor.task_generation().await.unwrap();
        let config = Configuration::parse(
            include_bytes!("../../../config/test/native-app-keypad/devices.json"),
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        feed(&mut feedback, 100, 0, 17);
        let controller = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let tokens = Arc::new(Tokens::default());
        tokens
            .insert(
                credential,
                SECRET,
                Grant {
                    controller,
                    role: Role::Member,
                    devices: config
                        .devices()
                        .keys()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
                    scenes: Default::default(),
                },
            )
            .unwrap();
        let source = Source::capture(
            tokens
                .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
                .unwrap(),
            &Action::KeypadAction {
                controller,
                request: Request {
                    revision: "1".into(),
                    keypad: "panel".into(),
                    key: 1,
                    request_id: "gesture-1".into(),
                    gesture: Gesture::Activate,
                },
            },
            None,
        )
        .unwrap();
        let originals = vec![command(&generation, 1, "fixture-light", level(0), 2_000)];
        let evidence = Evidence {
            command_fence: 1, generation: generation.clone(),
            variable: ReadCheck { revision: 1, name: "mode".into(), expected_version: 0 },
            written: BTreeMap::from([(1, 10)]), writes_by_device: BTreeMap::from([(id("fixture-light"), 10)]),
            after_device: id("fixture-light"), after_written: 10, after_command: level(0), deadline,
            effect: serde_json::from_value(serde_json::json!({
                "id":"conditional-mode","after":{"device":"fixture-light","on":false},
                "when":[{"device":"fixture-downlight","on":true}],"variable":"mode","value":3,
                "indicators":[{"device":"legacy-key-3","on":true},{"device":"legacy-key-5","on":false}],
                "max_age_ms":5000,"review_evidence":"Synthetic bounded conditional writer proof"})).unwrap(),
        };
        let proof = FeedbackProof::prepare(evidence.clone(), &config, &feedback, 100).unwrap();
        let leds = vec![
            command(
                &generation,
                3,
                "legacy-key-3",
                Command::PanelLed(true),
                proof.send_before(),
            ),
            command(
                &generation,
                4,
                "legacy-key-5",
                Command::PanelLed(false),
                proof.send_before(),
            ),
        ];
        let mut owner = Owner::default();
        let prepared = owner
            .prepare_completion(
                &config,
                &proof,
                source.clone(),
                controller,
                originals.clone(),
                &leds,
                &BTreeMap::new(),
                proof.send_before(),
                100,
            )
            .unwrap();
        let mut store = Store::open(root.join("state.sqlite")).unwrap();
        store
            .define_logical_variable(1, "mode", ValueType::Integer)
            .unwrap();
        store
            .accept_commands(generation.boot_id(), &originals, 0)
            .unwrap();
        store
            .transition_command(
                generation.boot_id(),
                1,
                Status::Accepted,
                Status::Writing,
                9,
            )
            .unwrap();
        store
            .transition_command(
                generation.boot_id(),
                1,
                Status::Writing,
                Status::Written,
                10,
            )
            .unwrap();
        // Exercise the real atomic storage API before the gate is installed.
        let receipt = Commit {
            update: Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 0,
                value: Some(Value::Integer(3)),
            },
            policy: evidence.effect.id.clone(),
            sequence: 1,
            endpoint: id("legacy-key-1"),
            request_id: "gesture-1".into(),
            epochs: generation.epochs,
            command_fence: 1,
            written: evidence.written.clone(),
            observations: proof
                .lease()
                .observations
                .iter()
                .map(|(id, at)| (id.clone(), at.unwrap()))
                .collect(),
            after_device: evidence.after_device.clone(),
            after_written: 10,
            original_deadline_ms: deadline,
            send_before_ms: proof.send_before(),
        };
        store
            .accept_keypad_completion(
                generation.boot_id(),
                &leds,
                &[],
                100,
                None,
                &controller.to_string(),
                &receipt,
            )
            .unwrap();
        owner.install_completion(prepared);
        Self {
            owner,
            feedback,
            actor,
            root,
            boot,
            config,
            generation,
            controller,
            tokens,
            credential,
            source,
            originals,
            evidence,
            leds,
        }
    }
    fn store(&self) -> Store {
        Store::open(self.root.join("state.sqlite")).unwrap()
    }
    fn refresh(&mut self, now: u64) -> Result<(), W> {
        let view = self.owner.views().into_iter().next().unwrap();
        let proof = Proof::prepare(view, Some(&self.config), &self.feedback, now).unwrap();
        self.owner.apply(proof, self.boot, now)
    }
    fn audit(&mut self, now: u64) {
        self.owner
            .audit(
                &self.store(),
                Some(&self.generation),
                now,
                |_| true,
                &BTreeMap::new(),
            )
            .unwrap();
    }
    async fn close(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
fn level(value: u8) -> Command {
    Command::SetLevel {
        level: Percent::new(value).unwrap(),
        transition: LevelTransition::Immediate,
    }
}
fn command(
    g: &TaskGeneration,
    n: u64,
    device: &str,
    action: Command,
    deadline_ms: u64,
) -> QueuedCommand {
    QueuedCommand {
        id: n,
        device_id: id(device),
        command: action,
        origin: Origin::Rule,
        epochs: g.epochs,
        deadline_ms,
        run_id: None,
    }
}
fn feed(feedback: &mut Feedback, at: u64, source: u8, condition: u8) {
    feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: source,
                    },
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd2,
                        value: condition,
                    },
                ],
            },
        })
        .unwrap();
}

#[tokio::test]
async fn atomic_receipt_installs_only_linked_leds_and_refresh_never_freezes_a_brightness_or_extends_ttl()
 {
    let mut f = Fixture::new().await;
    assert_eq!(f.owner.phases.len(), 2);
    assert!(f.owner.phases.values().all(|phase| phase.led));
    assert!(f.owner.eligible(3, 100));
    assert!(f.owner.eligible(4, 100));
    assert_eq!(f.owner.write_deadline(3), Some(2_100));
    let old = f.owner.authorization(3).unwrap();
    feed(&mut f.feedback, 300, 0, 20); // On remains true at another brightness.
    assert!(!f.owner.eligible(3, 300));
    assert_eq!(old.with_valid(300, || 1), None);
    f.refresh(300).unwrap();
    assert!(f.owner.eligible(3, 300));
    assert_eq!(f.owner.write_deadline(3), Some(2_100));
    assert!(
        f.owner
            .authorization(3)
            .unwrap()
            .with_valid(2_099, || true)
            .unwrap()
    );
    assert_eq!(
        f.owner.authorization(3).unwrap().with_valid(2_100, || true),
        None
    );
    f.audit(2_100);
    assert!(!f.owner.eligible(3, 2_100));
    let cancelled = f.owner.invalidated_leds();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].0, homesense_core::queue::CancelReason::Expired);
    assert_eq!(cancelled[0].1, [3, 4].into());
    assert_eq!(
        f.store().command_status(f.generation.boot_id(), 1).unwrap(),
        Some(Status::Written)
    );
    f.close().await;
}

#[tokio::test]
async fn conditional_refresh_never_requests_queries_and_cannot_use_an_ordinary_or_relabelled_proof()
{
    let mut f = Fixture::new().await;
    let view = f.owner.views().remove(0);
    let targets =
        crate::physical_queries::plan(&f.config, homesense_core::ExecutionContext::Simulation)
            .unwrap();
    assert!(
        view.query_requirements(&f.config, &f.feedback, &targets, 100)
            .is_empty()
    );
    let ordinary = Proof {
        view: view.clone(),
        sample: f.feedback.scene_observation_start(100).unwrap().into(),
    };
    assert!(matches!(
        f.owner.apply(ordinary, f.boot, 100),
        Err(W::InvalidValueInRequest)
    ));
    for changed in 0..5 {
        let mut forged = view.clone();
        match changed {
            0 => forged.condition = None,
            1 => forged.condition.as_mut().unwrap().postconditions[1].on = false,
            2 => forged.condition.as_mut().unwrap().after.clear(),
            3 => forged.deadline += 1,
            _ => forged.managed_away = !forged.managed_away,
        }
        let proof = Proof::prepare(forged, Some(&f.config), &f.feedback, 100).unwrap();
        assert!(matches!(
            f.owner.apply(proof, f.boot, 100),
            Err(W::ResourceBusy)
        ));
    }
    assert!(f.owner.eligible(3, 100));
    f.close().await;
}

#[tokio::test]
async fn changed_condition_or_source_permanently_retires_both_leds_without_touching_original_light()
{
    for (source, condition) in [(0, 0), (50, 17)] {
        let mut f = Fixture::new().await;
        feed(&mut f.feedback, 200, source, condition);
        f.refresh(200).unwrap();
        assert!(!f.owner.eligible(3, 200));
        assert_eq!(f.owner.invalidated_leds()[0].1, [3, 4].into());
        feed(&mut f.feedback, 300, 0, 17);
        assert!(
            f.owner.views().is_empty(),
            "a later restored value cannot rearm this completion"
        );
        assert_eq!(
            f.store().command_status(f.generation.boot_id(), 1).unwrap(),
            Some(Status::Written)
        );
        f.close().await;
    }
}

#[tokio::test]
async fn unknown_feedback_waits_for_fresh_samples_within_the_same_fixed_window() {
    let mut f = Fixture::new().await;
    f.feedback.expire(200, 50).unwrap();
    f.refresh(200).unwrap();
    assert!(!f.owner.eligible(3, 200));
    assert!(
        f.owner.invalidated_leds().is_empty(),
        "unknown is not false"
    );
    feed(&mut f.feedback, 300, 0, 17);
    f.refresh(300).unwrap();
    assert!(f.owner.eligible(3, 300));
    assert_eq!(f.owner.write_deadline(3), Some(2_100));
    f.close().await;
}

#[tokio::test]
async fn a_late_confirmation_uses_the_earlier_original_deadline() {
    let mut f = Fixture::deadline(800).await;
    assert_eq!(f.owner.write_deadline(3), Some(800));
    feed(&mut f.feedback, 799, 0, 30);
    f.refresh(799).unwrap();
    assert!(f.owner.eligible(3, 799));
    assert!(!f.owner.eligible(3, 800));
    assert_eq!(
        f.owner.authorization(3).unwrap().with_valid(800, || true),
        None
    );
    f.close().await;
}

#[tokio::test]
async fn newer_rule_controls_target_cas_and_revocation_retire_conditional_delivery_but_queries_do_not()
 {
    for cause in 0..6 {
        let mut f = Fixture::new().await;
        match cause {
            0..=2 => f
                .store()
                .accept_commands(
                    f.generation.boot_id(),
                    &[command(
                        &f.generation,
                        20,
                        if cause == 0 {
                            "fixture-light"
                        } else {
                            "fixture-downlight"
                        },
                        if cause == 2 {
                            Command::Query
                        } else {
                            level(17)
                        },
                        1_000,
                    )],
                    200,
                )
                .unwrap(),
            3 => f
                .store()
                .accept_queue_batch_with_variables(
                    "new-target",
                    &[],
                    &[],
                    200,
                    None,
                    &[Update {
                        revision: 1,
                        name: "mode".into(),
                        expected_version: 1,
                        value: Some(Value::Integer(4)),
                    }],
                )
                .unwrap(),
            4 => f.tokens.revoke(f.credential).unwrap(),
            _ => {}
        }
        if cause == 5 {
            f.owner
                .audit(&f.store(), None, 200, |_| true, &BTreeMap::new())
                .unwrap();
        } else {
            f.audit(200);
        }
        assert_eq!(f.owner.eligible(3, 200), cause == 2, "cause {cause}");
        f.close().await;
    }
}

#[tokio::test]
async fn invalid_preparation_never_creates_phases_and_cannot_widen_the_send_window() {
    let f = Fixture::new().await;
    let proof = FeedbackProof::prepare(f.evidence.clone(), &f.config, &f.feedback, 100).unwrap();
    for bad in 0..6 {
        let owner = Owner::default();
        let mut leds = f.leds.clone();
        let mut originals = f.originals.clone();
        let mut deadline = proof.send_before();
        match bad {
            0 => leds[0].command = level(100),
            1 => leds[0].device_id = id("fixture-light"),
            2 => leds[1].id = leds[0].id,
            3 => leds[0].deadline_ms += 1,
            4 => deadline += 1,
            _ => originals[0].id = 2,
        }
        assert!(
            owner
                .prepare_completion(
                    &f.config,
                    &proof,
                    f.source.clone(),
                    f.controller,
                    originals,
                    &leds,
                    &BTreeMap::new(),
                    deadline,
                    100
                )
                .is_err(),
            "case {bad}"
        );
        assert!(owner.phases.is_empty());
    }
    f.close().await;
}

#[tokio::test]
async fn unrelated_module_feedback_can_refresh_the_guard_without_new_dependency_samples_or_queries()
{
    let mut f = Fixture::new().await;
    let observations = f.owner.authorization(3).unwrap().lease.observations.clone();
    f.feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: 200,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x15,
                    command: 0xd3,
                    value: 60,
                }],
            },
        })
        .unwrap();
    assert!(
        !f.owner.eligible(3, 200),
        "an old global feedback lease is no longer valid"
    );
    f.refresh(200).unwrap();
    assert!(f.owner.eligible(3, 200));
    assert_eq!(
        f.owner.authorization(3).unwrap().lease.observations,
        observations
    );
    assert_eq!(f.owner.write_deadline(3), Some(2_100));
    f.close().await;
}

#[tokio::test]
async fn whole_module_off_on_a_nonrepresentative_condition_port_and_manual_override_cancel_delivery()
 {
    for whole in [false, true] {
        let mut f = Fixture::new().await;
        if whole {
            let devices = (1..=8)
                .map(|port| {
                    (
                        port,
                        if port == 2 {
                            "fixture-downlight".into()
                        } else {
                            format!("module-port-{port}")
                        },
                    )
                })
                .collect();
            let effect = homesense_storage::module_off::Effects {
                command: 20,
                module_id: "light-module".into(),
                gateway_id: "bus".into(),
                address: 0x15,
                profile: "ad8_din_rs485_v2_2_1".into(),
                devices,
            };
            f.store()
                .accept_module_off_commands(
                    f.generation.boot_id(),
                    &[command(
                        &f.generation,
                        20,
                        "module-port-1",
                        Command::ModuleAllOff,
                        1_000,
                    )],
                    &[],
                    200,
                    &[effect],
                )
                .unwrap();
            f.audit(200);
        } else {
            f.owner
                .audit(
                    &f.store(),
                    Some(&f.generation),
                    200,
                    |_| true,
                    &BTreeMap::from([(id("fixture-downlight"), 1)]),
                )
                .unwrap();
        }
        assert!(!f.owner.eligible(3, 200));
        assert_eq!(f.owner.invalidated_leds()[0].1, [3, 4].into());
        f.close().await;
    }
}
