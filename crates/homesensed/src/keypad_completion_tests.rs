use super::*;
use crate::admission::{Admission, Options};
use homesense_core::{ExecutionContext, gateway_scope::WireScope, queue::Origin};
use homesense_storage::{Store, scene_run::Status as SceneStatus};
use uuid::Uuid;

// Obtain an authentic opaque generation from a local test owner. This creates
// no transport and contacts no network gateway; the retained receipt itself is
// deliberately not a Source or permission capability.
async fn generation() -> TaskGeneration {
    let path = std::env::temp_dir().join(format!("completion-receipt-{}.sqlite", Uuid::new_v4()));
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            include_bytes!("../../../config/test/native-app-keypad/devices.json"),
            include_bytes!("../../../config/test/native-app-keypad/automation.json"),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    drop(store);
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let generation = actor.task_generation().await.unwrap();
    actor.shutdown().await.unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
    generation
}
fn device(id: &str) -> DeviceId {
    DeviceId::new(id).unwrap()
}
fn pending(generation: &TaskGeneration) -> Pending {
    Pending::new(Capture {
        endpoint: device("origin-key"),
        request_id: "receipt-gesture".into(),
        admit_before: 1_000,
        command_fence:1,
        effect: serde_json::from_value(serde_json::json!({
            "id":"source-off-completion","after":{"device":"source-load","on":false},
            "when":[{"device":"condition-load","on":true}],"variable":"linked-mode","value":3,
            "indicators":[{"device":"linked-led","on":true}],"max_age_ms":5_000,"review_evidence":"Synthetic conditional source"})).unwrap(),
        generation: generation.clone(),
        variable: ReadCheck { revision: generation.epochs.configuration, name: "linked-mode".into(), expected_version: 7 },
        manual: BTreeMap::from([(device("source-load"), 4), (device("condition-load"), 6)]),
        wires: vec![WireScope::from_persisted("bus".into(), 1).unwrap()],
        after_device: device("source-load"),
    })
}
fn command(
    generation: &TaskGeneration,
    id: u64,
    device_id: &str,
    command: Command,
) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: device(device_id),
        command,
        origin: Origin::Panel,
        epochs: generation.epochs,
        deadline_ms: 900,
        run_id: None,
    }
}
fn explicit(ids: &[u64], runs: &[&str]) -> Execution {
    Execution::Explicit {
        commands: ids.iter().copied().collect(),
        runs: runs.iter().map(|s| (*s).into()).collect(),
    }
}
fn evidence(pending: &Pending, now: u64) -> Option<Evidence> {
    pending.evidence(
        now,
        |_| Some(Status::Written),
        |_| Some(SceneStatus::CommandsWritten),
        None,
    )
}

#[tokio::test]
async fn diagnostic_retirement_keeps_the_first_failure_without_rearming_execution() {
    let g = generation().await;
    let mut p = pending(&g);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Switch(false))],
        1000,
        100,
        &BTreeMap::new(),
    );
    p.seal(explicit(&[1], &[]));
    p.written(1, false, 120);
    assert_eq!(p.retired, Some(End::WriteUncertain));
    p.retire(End::Cancelled);
    p.written(1, true, 2000);
    p.accepted(
        &[command(&g, 2, "source-load", Command::Switch(false))],
        4000,
        2000,
        &BTreeMap::new(),
    );
    p.seal(explicit(&[2], &[]));
    assert_eq!(
        p.retired,
        Some(End::WriteUncertain),
        "later cleanup must not replace the original causal diagnosis"
    );
    assert!(evidence(&p, 2001).is_none());
    assert_eq!(
        p.tickets.len(),
        1,
        "diagnostic bookkeeping cannot admit a replay"
    );
}

#[tokio::test]
async fn diagnostic_scope_reasons_distinguish_version_manual_wire_generation_and_original_timeout()
{
    let g = generation().await;
    let mut p = pending(&g);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Switch(false))],
        1000,
        100,
        &BTreeMap::new(),
    );
    let manual = BTreeMap::from([(device("source-load"), 4), (device("condition-load"), 6)]);
    assert_eq!(p.invalid_reason(&g, 7, &manual, |_| true, 200), None);
    assert_eq!(
        p.invalid_reason(&g, 8, &manual, |_| true, 200),
        Some(End::VariableChanged)
    );
    let mut changed = manual.clone();
    changed.insert(device("condition-load"), 7);
    assert_eq!(
        p.invalid_reason(&g, 7, &changed, |_| true, 200),
        Some(End::ManualChanged)
    );
    assert_eq!(
        p.invalid_reason(&g, 7, &manual, |_| false, 200),
        Some(End::GatewayChanged)
    );
    let mut next = g.clone();
    next.epochs.control += 1;
    assert_eq!(
        p.invalid_reason(&next, 7, &manual, |_| true, 200),
        Some(End::GenerationChanged)
    );
    assert_eq!(
        p.invalid_reason(&g, 7, &manual, |_| true, 1000),
        Some(End::Deadline)
    );
}

#[tokio::test]
async fn an_accepted_or_journal_written_command_is_not_an_actual_writer_receipt() {
    let g = generation().await;
    let mut p = pending(&g);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Switch(false))],
        1_000,
        100,
        &BTreeMap::new(),
    );
    // A durable status alone cannot invent the physical writer callback.
    assert!(evidence(&p, 200).is_none());
    p.seal(explicit(&[1], &[]));
    assert!(evidence(&p, 200).is_none());
    p.written(99, true, 110); // Another execution's callback grants nothing.
    assert!(evidence(&p, 200).is_none());
    p.written(1, true, 120);
    assert!(
        evidence(&p, 119).is_none(),
        "cannot use a future callback timestamp"
    );
    let proof = evidence(&p, 200).unwrap();
    assert_eq!(proof.generation, g);
    assert_eq!(proof.after_device, device("source-load"));
    assert_eq!(proof.after_written, 120);
    assert_eq!(proof.written, BTreeMap::from([(1, 120)]));
    assert_eq!(proof.variable.expected_version, 7);
    assert_eq!(proof.deadline, 1_000);
    // A duplicate callback cannot move the post-write observation boundary.
    p.written(1, true, 160);
    assert_eq!(evidence(&p, 200).unwrap().after_written, 120);
}

#[tokio::test]
async fn source_receipt_must_cover_owned_ids_and_all_timeline_runs_must_succeed() {
    let g = generation().await;
    for final_status in [
        None,
        Some(SceneStatus::Pending),
        Some(SceneStatus::Running),
        Some(SceneStatus::Partial),
        Some(SceneStatus::Cancelled),
        Some(SceneStatus::Failed),
        Some(SceneStatus::Interrupted),
        Some(SceneStatus::CommandsWritten),
    ] {
        let mut p = pending(&g);
        p.accepted(
            &[command(&g, 1, "source-load", Command::Switch(false))],
            1_000,
            100,
            &BTreeMap::new(),
        );
        p.written(1, true, 120);
        p.seal(explicit(&[1], &["first", "second"]));
        let proof = p.evidence(
            200,
            |_| Some(Status::Written),
            |run| {
                if run == "first" {
                    Some(SceneStatus::CommandsWritten)
                } else {
                    final_status
                }
            },
            None,
        );
        assert_eq!(
            proof.is_some(),
            final_status == Some(SceneStatus::CommandsWritten),
            "{final_status:?}"
        );
    }
    let mut p = pending(&g);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Switch(false))],
        1_000,
        100,
        &BTreeMap::new(),
    );
    p.written(1, true, 120);
    p.seal(explicit(&[1, 99], &[]));
    assert!(
        evidence(&p, 200).is_none(),
        "cannot import a foreign command receipt"
    );
    p.seal(explicit(&[1], &[]));
    assert!(
        evidence(&p, 200).is_none(),
        "a different retry cannot reseal the execution"
    );
}

#[tokio::test]
async fn uncertain_cancelled_partial_late_and_preaccept_writes_cannot_complete() {
    let g = generation().await;
    for status in [
        None,
        Some(Status::Accepted),
        Some(Status::Writing),
        Some(Status::Cancelled),
        Some(Status::Failed),
        Some(Status::Uncertain),
    ] {
        let mut p = pending(&g);
        p.accepted(
            &[command(&g, 1, "source-load", Command::Switch(false))],
            1_000,
            100,
            &BTreeMap::new(),
        );
        p.written(1, true, 120);
        p.seal(explicit(&[1], &[]));
        assert!(
            p.evidence(200, |_| status, |_| None, None).is_none(),
            "{status:?}"
        );
    }
    for (complete, at) in [(false, 120), (true, 99), (true, 900), (true, 1_000)] {
        let mut p = pending(&g);
        p.accepted(
            &[command(&g, 1, "source-load", Command::Switch(false))],
            1_000,
            100,
            &BTreeMap::new(),
        );
        p.written(1, complete, at);
        p.written(1, true, 130); // No retry after uncertain/late evidence.
        p.seal(explicit(&[1], &[]));
        assert!(evidence(&p, 200).is_none(), "{complete} at {at}");
    }
}

#[tokio::test]
async fn query_led_and_unrelated_actuator_writes_do_not_prove_the_source_actuator() {
    let g = generation().await;
    for (id, action) in [
        ("source-load", Command::Query),
        ("source-load", Command::PanelLed(false)),
        ("condition-load", Command::Switch(false)),
    ] {
        let mut p = pending(&g);
        p.accepted(&[command(&g, 1, id, action)], 1_000, 100, &BTreeMap::new());
        p.written(1, true, 120);
        p.seal(explicit(&[1], &[]));
        assert!(evidence(&p, 200).is_none());
    }
    for action in [Command::Query, Command::PanelLed(true)] {
        let mut p = pending(&g);
        p.accepted(
            &[
                command(&g, 1, "source-load", Command::Switch(false)),
                command(&g, 2, "other", action),
            ],
            1_000,
            100,
            &BTreeMap::new(),
        );
        p.written(1, true, 120);
        p.written(2, true, 130);
        p.seal(explicit(&[1, 2], &[]));
        assert!(
            p.evidence(200, |_| Some(Status::Observed), |_| None, None)
                .is_none(),
            "nonactuator Observed is not Written"
        );
    }
}

#[tokio::test]
async fn variable_manual_gateway_and_generation_fences_keep_the_original_snapshot() {
    let g = generation().await;
    let mut p = pending(&g);
    let source_manual = BTreeMap::from([(device("source-load"), 8)]);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Switch(false))],
        1_000,
        100,
        &source_manual,
    );
    let manual = BTreeMap::from([(device("source-load"), 8), (device("condition-load"), 6)]);
    assert!(p.current(
        &g,
        7,
        &manual,
        |wire| wire.gateway() == "bus" && wire.epoch() == 1,
        200
    ));
    assert!(
        !p.current(&g, 8, &manual, |_| true, 200),
        "newer target variable wins"
    );
    for (device_id, value) in [("source-load", 9), ("condition-load", 10)] {
        let mut changed = manual.clone();
        changed.insert(device(device_id), value);
        assert!(
            !p.current(&g, 7, &changed, |_| true, 200),
            "newer physical operation wins"
        );
    }
    let mut epoch = g.clone();
    epoch.epochs.transport += 1;
    assert!(!p.current(&epoch, 7, &manual, |_| true, 200));
    assert!(
        !p.current(&generation().await, 7, &manual, |_| true, 200),
        "another boot cannot adopt pending work"
    );
    assert!(!p.current(&g, 7, &manual, |_| false, 200));
    assert!(!p.current(&g, 7, &manual, |_| true, 1_000));
    p.retire(End::Cancelled);
    assert!(!p.current(&g, 7, &manual, |_| true, 200));
}

#[tokio::test]
async fn native_phases_share_the_original_deadline_and_require_exact_final_completion() {
    let g = generation().await;
    let request = homesense_storage::native_scene::Request {
        controller: Uuid::new_v4().to_string(),
        key: "gesture".into(),
        scene: "source".into(),
        group: "source-group".into(),
        variable: "source-mode".into(),
        variable_version: 3,
        boot: g.boot_id().into(),
        epochs: g.epochs,
        deadline_ms: 1_000,
    };
    let mut p = pending(&g);
    p.accepted(
        &[command(&g, 1, "source-load", Command::Toggle)],
        1_000,
        100,
        &BTreeMap::new(),
    );
    p.written(1, true, 120);
    p.seal(Execution::Native {
        request: request.clone(),
    });
    p.accepted(
        &[command(&g, 2, "source-load", Command::Query)],
        5_000,
        200,
        &BTreeMap::new(),
    );
    p.written(2, true, 210);
    let mut row = homesense_storage::native_scene::Record {
        request,
        phase: 2,
        fence: 1,
        status: homesense_storage::native_scene::Status::Pending,
        commands: vec![2],
        initial_commands: vec![1],
        selected: None,
        updated_ms: 220,
    };
    assert!(
        p.evidence(250, |_| Some(Status::Written), |_| None, Some(&row))
            .is_none()
    );
    row.status = homesense_storage::native_scene::Status::Observed;
    for selected in [Some(0), Some(1)] {
        row.selected = selected;
        let proof = p
            .evidence(250, |_| Some(Status::Written), |_| None, Some(&row))
            .unwrap();
        assert_eq!(
            proof.after_written, 120,
            "the later query cannot replace the actuator write boundary"
        );
        assert_eq!(proof.deadline, 1_000);
        // Execution completion by itself does not imply the Off-specific
        // condition; final On must still be rejected by the feedback decision.
    }
    assert!(
        p.evidence(1_000, |_| Some(Status::Written), |_| None, Some(&row))
            .is_none()
    );
    row.request.deadline_ms = 5_000;
    assert!(
        p.evidence(250, |_| Some(Status::Written), |_| None, Some(&row))
            .is_none()
    );
    row.request.deadline_ms = 1_000;
    row.initial_commands = vec![99];
    assert!(
        p.evidence(250, |_| Some(Status::Written), |_| None, Some(&row))
            .is_none()
    );
}

#[tokio::test]
async fn capture_admission_window_does_not_shorten_an_already_accepted_native_execution() {
    let g = generation().await;
    let mut p = pending(&g);
    let request = homesense_storage::native_scene::Request {
        controller: Uuid::new_v4().to_string(),
        key: "long-native".into(),
        scene: "source".into(),
        group: "source-group".into(),
        variable: "source-mode".into(),
        variable_version: 1,
        boot: g.boot_id().into(),
        epochs: g.epochs,
        deadline_ms: 5_000,
    };
    p.accepted(
        &[command(&g, 1, "source-load", Command::Toggle)],
        5_000,
        100,
        &BTreeMap::new(),
    );
    p.written(1, true, 120);
    p.seal(Execution::Native {
        request: request.clone(),
    });
    let mut query = command(&g, 2, "source-load", Command::Query);
    query.deadline_ms = 4_000;
    p.accepted(&[query], 5_000, 2_000, &BTreeMap::new());
    p.written(2, true, 2_100);
    let row = homesense_storage::native_scene::Record {
        request,
        phase: 2,
        fence: 1,
        status: homesense_storage::native_scene::Status::Observed,
        commands: vec![2],
        initial_commands: vec![1],
        selected: Some(0),
        updated_ms: 2_200,
    };
    assert_eq!(
        p.evidence(2_300, |_| Some(Status::Written), |_| None, Some(&row))
            .unwrap()
            .deadline,
        5_000
    );
}

#[tokio::test]
async fn duplicate_acceptance_never_rewrites_a_ticket_or_extends_its_budget() {
    let g = generation().await;
    let first = command(&g, 1, "source-load", Command::Switch(false));
    let mut p = pending(&g);
    p.accepted(std::slice::from_ref(&first), 500, 100, &BTreeMap::new());
    p.written(1, true, 120);
    p.accepted(std::slice::from_ref(&first), 5_000, 200, &BTreeMap::new());
    p.seal(explicit(&[1], &[]));
    p.seal(explicit(&[1], &[]));
    assert_eq!(evidence(&p, 250).unwrap().deadline, 500);
    assert!(evidence(&p, 500).is_none());
    let mut changed = first;
    changed.command = Command::Switch(true);
    p.accepted(&[changed], 5_000, 250, &BTreeMap::new());
    assert!(
        evidence(&p, 300).is_none(),
        "a new command cannot reuse an old ticket identity"
    );
}

#[tokio::test]
async fn a_phase_cannot_import_foreign_manual_changes_or_mixed_generations() {
    let g = generation().await;
    for case in 0..4 {
        let mut p = pending(&g);
        let mut cmd = command(&g, 1, "source-load", Command::Switch(false));
        let mut own = BTreeMap::new();
        match case {
            0 => {
                own.insert(device("condition-load"), 100);
            }
            1 => cmd.epochs.control += 1,
            2 => cmd.deadline_ms = 100,
            _ => cmd.id = 0,
        }
        p.accepted(&[cmd], 1_000, 100, &own);
        p.written(1, true, 120);
        p.seal(explicit(&[1], &[]));
        assert!(evidence(&p, 200).is_none(), "case {case}");
    }
    let mut p = pending(&g);
    let cmd = command(&g, 1, "source-load", Command::Switch(false));
    p.accepted(
        std::slice::from_ref(&cmd),
        1_000,
        100,
        &BTreeMap::from([(device("source-load"), 8)]),
    );
    p.written(1, true, 120);
    p.seal(explicit(&[1], &[]));
    p.accepted(
        &[cmd],
        1_000,
        150,
        &BTreeMap::from([(device("source-load"), 999)]),
    );
    assert!(
        evidence(&p, 200).is_none(),
        "an idempotent receipt cannot adopt a newer manual fence"
    );
}

fn dimmer_evidence(g: &TaskGeneration) -> Evidence {
    let mut p = pending(g);
    p.capture.after_device = device("fixture-light");
    p.capture.effect.after.device = "fixture-light".into();
    p.capture.effect.when[0].device = "fixture-downlight".into();
    p.accepted(
        &[command(
            g,
            1,
            "fixture-light",
            Command::SetLevel {
                level: homesense_core::Percent::new(0).unwrap(),
                transition: homesense_core::LevelTransition::Immediate,
            },
        )],
        1_000,
        100,
        &BTreeMap::new(),
    );
    p.written(1, true, 120);
    p.seal(explicit(&[1], &[]));
    evidence(&p, 200).unwrap()
}
fn config() -> homesense_config::Configuration {
    homesense_config::Configuration::parse(
        include_bytes!("../../../config/test/native-app-keypad/devices.json"),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn feedback(
    g: &TaskGeneration,
    config: &homesense_config::Configuration,
) -> crate::feedback::Feedback {
    crate::feedback::Feedback::new(
        config,
        ExecutionContext::Simulation,
        Uuid::parse_str(g.boot_id()).unwrap().as_u128(),
        g.epochs.transport,
    )
    .unwrap()
}
fn observations(
    feedback: &mut crate::feedback::Feedback,
    at: u64,
    source: Option<u8>,
    condition: Option<u8>,
) {
    let frames = [(0xd1, source), (0xd2, condition)]
        .into_iter()
        .filter_map(|(command, value)| {
            value.map(|value| hb_protocol::RawFrame {
                address: 0x15,
                command,
                value,
            })
        })
        .collect();
    feedback
        .apply(&crate::transport_owner::GatewayRead {
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

#[tokio::test]
async fn genuine_postwrite_feedback_decides_the_source_specific_condition_without_any_query() {
    let g = generation().await;
    let config = config();
    for (source, condition, expected) in [
        (None, Some(100), Decision::Waiting),
        (Some(0), None, Decision::Waiting),
        (Some(100), None, Decision::NoEffect),
        (Some(100), Some(100), Decision::NoEffect),
        (Some(0), Some(0), Decision::NoEffect),
        (Some(0), Some(100), Decision::Apply),
    ] {
        let mut feedback = feedback(&g, &config);
        observations(&mut feedback, 150, source, condition);
        let proof = FeedbackProof::prepare(dimmer_evidence(&g), &config, &feedback, 200).unwrap();
        assert_eq!(
            proof.decision, expected,
            "source {source:?}, condition {condition:?}"
        );
        assert_eq!(
            proof.send_before, 1_000,
            "the source deadline wins over a new two-second send window"
        );
        assert_eq!(proof.evidence.variable.expected_version, 7);
        drop(
            proof
                .lease
                .lock(
                    Uuid::parse_str(g.boot_id()).unwrap().as_u128(),
                    g.epochs.configuration,
                    g.epochs.transport,
                    200,
                )
                .unwrap(),
        );
    }
}

#[tokio::test]
async fn prewrite_equal_boundary_and_stale_observations_never_complete_an_effect() {
    let g = generation().await;
    let config = config();
    for at in [110, 120] {
        let mut feedback = feedback(&g, &config);
        observations(&mut feedback, at, Some(0), Some(100));
        assert_eq!(
            FeedbackProof::prepare(dimmer_evidence(&g), &config, &feedback, 200)
                .unwrap()
                .decision,
            Decision::Waiting
        );
    }
    let mut feedback = feedback(&g, &config);
    observations(&mut feedback, 150, Some(0), Some(100));
    let mut receipt = dimmer_evidence(&g);
    receipt.effect.max_age_ms = 100;
    assert_eq!(
        FeedbackProof::prepare(receipt, &config, &feedback, 300)
            .unwrap()
            .decision,
        Decision::Waiting
    );
    assert!(FeedbackProof::prepare(dimmer_evidence(&g), &config, &feedback, 1_000).is_err());
}

#[tokio::test]
async fn changed_feedback_owner_or_newer_feedback_invalidates_a_prepared_decision() {
    let g = generation().await;
    let config = config();
    let mut feedback = feedback(&g, &config);
    observations(&mut feedback, 150, Some(0), Some(100));
    let proof = FeedbackProof::prepare(dimmer_evidence(&g), &config, &feedback, 200).unwrap();
    assert_eq!(proof.decision, Decision::Apply);
    observations(&mut feedback, 250, Some(0), Some(0));
    assert!(
        proof
            .lease
            .lock(
                Uuid::parse_str(g.boot_id()).unwrap().as_u128(),
                g.epochs.configuration,
                g.epochs.transport,
                250
            )
            .is_err()
    );
    let mut receipt = dimmer_evidence(&g);
    receipt.generation = generation().await;
    assert!(
        FeedbackProof::prepare(receipt, &config, &feedback, 300).is_err(),
        "another boot's snapshot cannot complete this source"
    );
}

#[tokio::test]
async fn a_known_on_branch_cannot_acquire_an_off_effect_from_independent_later_feedback() {
    let g = generation().await;
    let config = config();
    let mut feedback = feedback(&g, &config);
    observations(&mut feedback, 150, Some(0), Some(100));
    let mut receipt = dimmer_evidence(&g);
    receipt.after_command = Command::SetLevel {
        level: homesense_core::Percent::new(100).unwrap(),
        transition: homesense_core::LevelTransition::Immediate,
    };
    assert_eq!(
        FeedbackProof::prepare(receipt, &config, &feedback, 200)
            .unwrap()
            .decision,
        Decision::NoEffect
    );
    let mut receipt = dimmer_evidence(&g);
    receipt.after_command = Command::Toggle;
    assert_eq!(
        FeedbackProof::prepare(receipt, &config, &feedback, 200)
            .unwrap()
            .decision,
        Decision::Apply
    );
    observations(&mut feedback, 250, Some(100), Some(100));
    let mut receipt = dimmer_evidence(&g);
    receipt.after_command = Command::Toggle;
    assert_eq!(
        FeedbackProof::prepare(receipt, &config, &feedback, 300)
            .unwrap()
            .decision,
        Decision::NoEffect
    );
}

#[tokio::test]
async fn combined_off_uses_postwrite_supply_truth_and_ignores_residual_dimmer_brightness() {
    let g = generation().await;
    let mut input: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/devices.json"
    ))
    .unwrap();
    input["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({
        "id":"power-module","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    input["devices"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
        "id":"power","name":"Power","module_id":"power-module","channel":1,"enabled":true}));
    input["devices"][0]["default_on_level"] = serde_json::json!(100);
    input["devices"][0]["supply"] = serde_json::json!({"module_id":"power-module","channel":1});
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&input).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    for (power, expected) in [(0xf2, Decision::Apply), (0xf1, Decision::NoEffect)] {
        let mut feedback = feedback(&g, &config);
        // Residual AD8 100% is deliberately not a reliable Off observation.
        observations(&mut feedback, 130, Some(100), Some(100));
        feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: 150,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 3,
                        command: 0xd1,
                        value: power,
                    }],
                },
            })
            .unwrap();
        let mut p = pending(&g);
        p.capture.after_device = device("power");
        p.capture.effect.after.device = "fixture-light".into();
        p.capture.effect.when[0].device = "fixture-downlight".into();
        p.accepted(
            &[command(&g, 1, "power", Command::Switch(false))],
            1_000,
            100,
            &BTreeMap::new(),
        );
        p.written(1, true, 120);
        p.seal(explicit(&[1], &[]));
        let proof =
            FeedbackProof::prepare(evidence(&p, 200).unwrap(), &config, &feedback, 200).unwrap();
        assert_eq!(proof.decision, expected);
        assert!(
            !proof
                .lease
                .observations
                .contains_key(&device("fixture-light")),
            "Off must not depend on residual AD8"
        );
    }
}

#[tokio::test]
async fn sending_budget_is_bounded_by_both_original_execution_and_actual_feedback_freshness() {
    let g = generation().await;
    let config = config();
    let mut feedback = feedback(&g, &config);
    observations(&mut feedback, 150, Some(0), Some(100));
    let mut receipt = dimmer_evidence(&g);
    receipt.deadline = 5_000;
    receipt.effect.max_age_ms = 1_000;
    let proof = FeedbackProof::prepare(receipt, &config, &feedback, 200).unwrap();
    assert_eq!(proof.decision, Decision::Apply);
    assert_eq!(
        proof.send_before, 1_150,
        "freshness can expire before the ordinary two-second LED window"
    );
}
