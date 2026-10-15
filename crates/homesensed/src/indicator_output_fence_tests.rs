//! Admission-fenced software writes and transient restore target guards.
use super::*;
use super::cross_writer::drain_all;
use homesense_core::{
    DeviceId,
    rule::{Action, Budget, Condition, Expr, Node, Value},
};
fn reading_nodes() -> Vec<Node> {
    vec![Node {
        source: "synthetic fence".into(),
        action: Action::If {
            condition: Condition::Equal(
                Expr::DeviceOn(DeviceId::new("light").unwrap()),
                Expr::Literal(Value::Bool(true)),
            ),
            then_actions: vec![Node {
                source: "synthetic LED".into(),
                action: Action::Command {
                    device: DeviceId::new("key1").unwrap(),
                    command: Command::PanelLed(true),
                },
            }],
            else_actions: vec![],
        },
    }]
}
async fn prepared(h: &Harness) -> crate::rule_runtime::PreparedRule {
    let mut lease = None;
    let mut p = crate::rule_runtime::PreparedRule::prepare_indicator(
        &h.actor,
        h.controller,
        h.generation.clone(),
        vec!["selection".into()],
        |variables, _, policy| {
            let (plan, read) = h.feedback.plan_indicator_rule(
                &reading_nodes(),
                variables,
                Budget::default(),
                h.actor.monotonic_ms(),
                5000,
                &BTreeMap::new(),
                policy,
            )?;
            lease = Some(read);
            Ok(plan)
        },
    )
    .await
    .map_err(|_| "preparation failed")
    .unwrap();
    p.attach_feedback(lease);
    let output = h
        .actor
        .indicator_outputs(
            h.controller,
            h.generation.clone(),
            vec![DeviceId::new("key1").unwrap()],
            vec![],
        )
        .await
        .unwrap();
    p.attach_indicator_outputs(output.fence);
    p
}
#[tokio::test]
async fn newer_complete_write_between_capture_and_commit_rejects_old_output_fence() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let plan = prepared(&h).await;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: DeviceId::new("key1").unwrap(),
                    command: Command::PanelLed(false),
                }],
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    drain_leds(&h).await;
    assert_eq!(
        plan.commit_status(&h.actor).await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    assert!(drain_leds(&h).await.is_empty());
    sync(&h, &mut s).await;
    assert_eq!(drain_leds(&h).await.get(&(0x41, 1)), Some(&true));
    h.close().await;
}
#[tokio::test]
async fn output_fence_does_not_weaken_global_feedback_lease() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    let mut indicators = Synchronizer::default();
    sync(&h, &mut indicators).await;
    drain_leds(&h).await;
    let before = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    let (_, strict) = h
        .feedback
        .plan_rule(
            &reading_nodes(),
            &BTreeMap::new(),
            Budget::default(),
            h.actor.monotonic_ms(),
            5000,
        )
        .unwrap();
    let plan = prepared(&h).await;
    h.feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd8,
                    value: 0xf1,
                }],
            },
        })
        .unwrap();
    assert_eq!(
        h.actor
            .feedback_settled(h.controller, h.generation.clone(), strict)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy),
        "strict global proof remains invalid after an unrelated update"
    );
    assert!(
        matches!(
            plan.commit_status(&h.actor).await,
            Err(hap::server::WriteStatus::ResourceBusy
                | hap::server::WriteStatus::InvalidValueInRequest)
        ),
        "no stale plan may be accepted; selection CAS may reject before the global proof"
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    h.close().await;
}
#[tokio::test]
async fn previous_generation_output_is_not_current_or_replayed() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let old = h.generation.clone();
    h.actor.transport_failed().await.unwrap();
    h.actor.recover_synchronized(2, 1).await.unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    assert_eq!(
        h.actor
            .indicator_outputs(
                h.controller,
                old,
                vec![DeviceId::new("key1").unwrap()],
                vec![]
            )
            .await
            .err(),
        Some(hap::server::WriteStatus::ResourceBusy)
    );
    let output = h
        .actor
        .indicator_outputs(
            h.controller,
            h.generation.clone(),
            vec![DeviceId::new("key1").unwrap()],
            vec![],
        )
        .await
        .unwrap();
    assert!(
        output
            .fence
            .values()
            .all(|output| output.written.is_none() && output.requested.is_none())
    );
    h.feedback.invalidate().unwrap();
    h.feedback.resume(2, h.actor.monotonic_ms()).unwrap();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    assert!(drain_all(&h).await.contains(&[0xae, 0x41, 1, 0xf1, 0xee]));
    sync(&h, &mut s).await;
    assert!(drain_all(&h).await.is_empty());
    h.close().await;
}
#[tokio::test]
async fn transient_owned_dimmer_off_target_is_deferred_until_normal_feedback_settlement() {
    let (config, mut rules) = linked();
    rules["timed_scenes"][3]["steps"].as_array_mut().unwrap().push(json!({"device":"key1","offset_ms":0,"source":"synthetic timed off","command":{"kind":"panel_led","on":false}}));
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let mut scenes =
        SceneScheduler::new(h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
    scenes
        .start_document(&h.document, "off", h.actor.monotonic_ms())
        .await
        .unwrap();
    for _ in 0..8 {
        scenes.tick(h.actor.monotonic_ms()).await.unwrap();
        drain_all(&h).await;
        if scenes.publication_impact().is_empty() {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(0), Some(0));
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    let pending = h
        .actor
        .projection_levels(1, h.feedback.snapshot().cursor.boot_id)
        .await
        .unwrap();
    assert!(
        pending.contains_key(&DeviceId::new("light").unwrap()),
        "diagnose the actual early gate, not assume feedback_settled value matching"
    );
    eprintln!("Existing transient restore targets defer LED plan: {pending:?}");
    sync(&h, &mut s).await;
    assert!(drain_leds(&h).await.is_empty());
    levels(&mut h, Some(0), Some(0));
    h.actor
        .remember_levels(1, h.feedback.snapshot(), 5000)
        .await
        .unwrap();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    assert_eq!(drain_leds(&h).await.get(&(0x41, 1)), Some(&true));
    h.close().await;
}

#[tokio::test]
async fn accepted_other_writer_invalidates_stale_output_plan_without_claiming_written() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let plan = prepared(&h).await;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: DeviceId::new("key1").unwrap(),
                    command: Command::PanelLed(false),
                }],
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let count = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(
        plan.commit_status(&h.actor).await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    assert_eq!(drain_leds(&h).await.get(&(0x41, 1)), Some(&false));
    sync(&h, &mut s).await;
    assert_eq!(drain_leds(&h).await.get(&(0x41, 1)), Some(&true));
    h.close().await;
}
#[tokio::test]
async fn authorization_change_does_not_expand_scope_or_replay_cached_output() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (config, rules) = linked();
    let mut h = harness(config.clone(), rules).await;
    h.actor.shutdown().await.unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let authorization = allowed.clone();
    let controller = h.controller;
    h.actor = Admission::open(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller && authorization.load(Ordering::Acquire),
    )
    .await
    .unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    let config =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    h.feedback = crate::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        h.generation.epochs.transport,
    )
    .unwrap();
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let before = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    allowed.store(false, Ordering::Release);
    assert_eq!(
        h.actor
            .indicator_outputs(
                controller,
                h.generation.clone(),
                vec![DeviceId::new("key1").unwrap()],
                vec![]
            )
            .await
            .err(),
        Some(hap::server::WriteStatus::InsufficientPrivileges)
    );
    let scenes =
        SceneScheduler::new(h.actor.clone(), controller, 8, h.actor.monotonic_ms()).unwrap();
    assert!(
        s.tick(
            &h.actor,
            controller,
            &h.document,
            &scenes,
            &h.feedback,
            5000
        )
        .await
        .is_err()
    );
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    allowed.store(true, Ordering::Release);
    sync(&h, &mut s).await;
    assert!(drain_leds(&h).await.is_empty());
    h.close().await;
}

#[tokio::test]
async fn positive_level_change_does_not_rewrite_unique_power_scene_leds() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    for level in [80, 60, 30, 17, 100] {
        levels(&mut h, Some(level), Some(0));
        reconcile(&h).await;
        sync(&h, &mut s).await;
        assert!(
            drain_leds(&h).await.is_empty(),
            "positive dimmer level changes preserve unique power meaning"
        );
    }
    h.close().await;
}
