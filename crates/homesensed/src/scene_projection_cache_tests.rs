//! Synthetic cache contracts. No private inputs, sockets or physical commands.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

async fn clocked(config: Vec<u8>, rules: Json) -> (Harness, &'static AtomicU64) {
    let mut h = harness(config.clone(), rules).await;
    h.actor.shutdown().await.unwrap();
    let time: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(100)));
    let controller = h.controller;
    h.actor = Admission::open_with_test_time(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
        time,
    )
    .await
    .unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    let config =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    h.feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        h.generation.epochs.transport,
    )
    .unwrap();
    (h, time)
}
fn frames(h: &mut Harness, input: &[(u8, u8, u8)]) {
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: input
                    .iter()
                    .map(|&(address, command, value)| hb_protocol::RawFrame {
                        address,
                        command,
                        value,
                    })
                    .collect(),
            },
        })
        .unwrap();
}
async fn selected(h: &Harness, expected: Option<i64>) {
    assert_eq!(
        h.selection().await.stored.value,
        expected.map(Value::Integer)
    );
}

#[tokio::test]
async fn unbound_poll_reply_preserves_current_scene_without_refreshing_cache() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    history(&h, 2);
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    let original = h.selection().await;
    selected(&h, Some(0)).await;
    let strict = projection(&h).await;
    time.store(101, Ordering::Release);
    // Port eight is registered for polling but not in any scene dependency.
    frames(&mut h, &[(3, 0xd8, 0xf1)]);
    selected(&h, Some(0)).await;
    assert_eq!(h.selection().await.physical, original.physical);
    assert!(
        source(&h)
            .await
            .sample()
            .await
            .unwrap()
            .values()
            .all(|v| *v == Some(false))
    );
    assert_eq!(
        h.actor
            .reconcile_scene_selection(h.controller, h.generation.clone(), strict)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy),
        "action admission must retain the global fence"
    );
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn equal_required_reply_preserves_projection_but_not_original_action_lease() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    let original = h.selection().await.physical.unwrap();
    let strict = projection(&h).await;
    time.store(101, Ordering::Release);
    levels(&mut h, Some(30), Some(0));
    selected(&h, Some(1)).await;
    assert_eq!(
        h.selection().await.physical.unwrap(),
        original,
        "equal refresh must not rewrite proof time or history"
    );
    assert_eq!(
        h.actor
            .reconcile_scene_selection(h.controller, h.generation.clone(), strict)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    time.store(5099, Ordering::Release);
    selected(&h, Some(1)).await;
    time.store(5100, Ordering::Release);
    selected(&h, None).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn managed_action_deadline_does_not_expire_readonly_scene_projection() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    h.feedback.set_managed_max_age_ms(5000).unwrap();
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    let strict = projection(&h).await;
    let original = h.selection().await.physical.unwrap();
    // The managed runtime limits actions to two seconds. A read-only scene
    // still has the original five-second physical observation, with no I/O.
    time.store(2100, Ordering::Release);
    selected(&h, Some(1)).await;
    assert_eq!(h.selection().await.physical.unwrap(), original);
    assert_eq!(h.actor.reconcile_scene_selection(h.controller, h.generation.clone(), strict).await,
        Err(hap::server::WriteStatus::ResourceBusy), "the strict action lease must still expire");
    time.store(5099, Ordering::Release);
    selected(&h, Some(1)).await;
    time.store(5100, Ordering::Release);
    selected(&h, None).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn dependent_value_change_and_same_batch_round_trip_revoke_projection() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    selected(&h, Some(0)).await;
    time.store(101, Ordering::Release);
    levels(&mut h, Some(30), Some(0));
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(1)).await;
    time.store(102, Ordering::Release);
    frames(&mut h, &[(0x15, 0xd1, 0), (0x15, 0xd1, 30)]);
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(1)).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn stale_read_requests_metadata_refresh_without_bypassing_feedback_proof() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    selected(&h, Some(1)).await;
    time.store(101, Ordering::Release);
    levels(&mut h, Some(0), Some(0));
    selected(&h, None).await;
    let mut cursor=0;
    let groups=h.actor.projection_refresh_groups(&h.document,&mut cursor);
    assert_eq!(groups.iter().map(|g|g.variable.as_str()).collect::<Vec<_>>(), ["selection"]);
    selected(&h, None).await; // taking priority is not evidence of completion
    let scenes=crate::scene_scheduler::SceneScheduler::new(h.actor.clone(),h.controller,8,h.actor.monotonic_ms()).unwrap();
    crate::scene_reconciliation::tick_groups(&h.actor,h.controller,&h.document,&scenes,&h.feedback,&groups).await.unwrap();
    selected(&h, Some(0)).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn invalid_dependency_and_recovery_require_a_new_projection() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    time.store(101, Ordering::Release);
    frames(&mut h, &[(0x15, 0xd1, 255)]);
    selected(&h, None).await;
    time.store(102, Ordering::Release);
    levels(&mut h, Some(0), Some(0));
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(0)).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn invalid_never_observed_dependency_revokes_relative_eligibility() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    reconcile(&h).await;
    assert!(h.selection().await.physical.unwrap().relative_eligible);
    time.store(101, Ordering::Release);
    frames(&mut h, &[(0x15, 0xd1, 255)]);
    assert!(
        !h.selection().await.physical.unwrap().relative_eligible,
        "invalid sample cannot retain a healthy never-observed relative proof"
    );
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn supply_relay_change_revokes_combined_light_cache() {
    let (config, rules) = reconciled();
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["devices"][0]["supply_relay"] = json!("switch");
    config["devices"][0]["default_on_level"] = json!(100);
    let (mut h, time) = clocked(serde_json::to_vec(&config).unwrap(), rules).await;
    levels(&mut h, Some(30), Some(0));
    frames(&mut h, &[(3, 0xd1, 0xf1)]);
    reconcile(&h).await;
    selected(&h, Some(1)).await;
    time.store(101, Ordering::Release);
    frames(&mut h, &[(3, 0xd1, 0xf2)]);
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(0)).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn collision_discriminator_level_change_revokes_read_cache() {
    let (config, mut rules) = reconciled();
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["gateways"][0]["modules"][0]["profile"] = json!("ad8_din_rs485_v2_2_1");
    rules["timed_scenes"][1]["steps"][0]["command"]["value"] = json!(30);
    rules["timed_scenes"][1]["steps"][1]["command"]["value"] = json!(0);
    rules["scene_switch_groups"][0]["reconciliation"]["policy"] =
        json!("power_pattern_then_profile_level");
    for scene in rules["timed_scenes"].as_array_mut().unwrap() {
        for step in scene["steps"].as_array_mut().unwrap() {
            step["command"]["ramp"] = json!(true);
        }
    }
    let (mut h, time) = clocked(serde_json::to_vec(&config).unwrap(), rules).await;
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    selected(&h, Some(1)).await;
    time.store(101, Ordering::Release);
    levels(&mut h, Some(30), Some(0));
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(2)).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn future_equal_sample_and_backwards_clock_cannot_keep_cached_truth() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    time.store(101, Ordering::Release);
    levels(&mut h, Some(0), Some(0));
    time.store(100, Ordering::Release);
    selected(&h, None).await;
    time.store(101, Ordering::Release);
    selected(&h, Some(0)).await;
    time.store(99, Ordering::Release);
    selected(&h, None).await;
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn stale_quality_and_feedback_shutdown_revoke_projection_without_tick() {
    let (config, rules) = reconciled();
    let (mut h, time) = clocked(config, rules).await;
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    time.store(101, Ordering::Release);
    h.feedback.expire(101, 1).unwrap();
    selected(&h, None).await;
    time.store(102, Ordering::Release);
    levels(&mut h, Some(0), Some(0));
    selected(&h, None).await;
    reconcile(&h).await;
    selected(&h, Some(0)).await;
    h.feedback.invalidate().unwrap();
    selected(&h, None).await;
    h.no_output();
    h.close().await;
}
