//! Reviewed conditional actions keep physical state separate from program intent.
use super::*;

fn reviewed_rules() -> Json {
    let mut r = room_rules();
    r["scene_tracking"][0]["aggregation"] = json!("all");
    r["scene_tracking"][0]["hap_action"]["readback"] = json!("reviewed_conditions");
    r["scene_tracking"][0]["members"][1]["predicate"]["on"] = json!(false);
    let on = json!([
        {"source":"intent/on","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":1}}},
        command("on/own", "relay", json!({"kind":"switch","on":true})),
        {"source":"on/exclude-other","action":{"kind":"if","condition":{"kind":"any","conditions":[
            {"kind":"unknown","value":{"kind":"device_on","device":"marker"}},
            {"kind":"equal","left":{"kind":"device_on","device":"marker"},"right":{"kind":"bool","value":true}}
        ]},"then_actions":[command("on/other", "marker", json!({"kind":"switch","on":false}))],"else_actions":[]}}
    ]);
    let off = json!([
        {"source":"intent/off","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":0}}},
        command("off/own", "relay", json!({"kind":"switch","on":false}))
    ]);
    let matched = json!({"kind":"all","conditions":[
        {"kind":"equal","left":{"kind":"device_on","device":"relay"},"right":{"kind":"bool","value":true}},
        {"kind":"equal","left":{"kind":"device_on","device":"marker"},"right":{"kind":"bool","value":false}}
    ]});
    r["immediate_scenes"][0]["activate"] = json!([{"source":"original/relative","action":{
        "kind":"if","condition":{"kind":"equal","left":{"kind":"device_on","device":"relay"},"right":{"kind":"bool","value":false}},
        "then_actions":on,"else_actions":off
    }}]);
    r["immediate_scenes"][1]["activate"] = on;
    r["immediate_scenes"][2]["activate"] = json!([{"source":"off/only-matching-pattern","action":{
        "kind":"if","condition":matched,"then_actions":off,"else_actions":[]
    }}]);
    r
}

#[test]
fn reviewed_hap_rejects_new_scope_operations_and_state_providers() {
    let config = Configuration::parse(&data(), ExecutionContext::Simulation).unwrap();
    Document::parse(&serde_json::to_vec(&reviewed_rules()).unwrap(), &config).unwrap();
    for case in 0..12 {
        let mut r = reviewed_rules();
        match case {
            0 => r["scene_tracking"][0]["hap_action"]["readback"] = json!("all_conditions"),
            1 => r["scene_tracking"][0]["aggregation"] = json!("any"),
            2 => r["scene_metadata"][1]["catalog_hidden"] = json!(false),
            3 => r["scene_tracking"][0]["hap_action"]["off_scene"] = json!("all-on"),
            4 => r["immediate_scenes"][1]["activate"][0]["action"]["value"]["value"] = json!(7),
            5 => r["immediate_scenes"][1]["activate"][1]["action"]["device"] = json!("light"),
            6 => {
                r["immediate_scenes"][1]["activate"][2]["action"]["condition"] = json!({
                    "kind":"unknown","value":{"kind":"variable","id":"relay_on"}
                })
            }
            7 => {
                r["scene_tracking"][0]["members"][1]["predicate"] =
                    json!({"kind":"ignore","reason":"skip the other condition"})
            }
            8 => r["scene_tracking"][0]["members"][0]["predicate"]["on"] = json!(false),
            9 => {
                r["immediate_scenes"][1]["activate"][1]["action"]["command"] =
                    json!({"kind":"query"})
            }
            10 => {
                r["immediate_scenes"][1]["activate"][1]["action"] =
                    json!({"kind":"scene","id":"all-off","activate":true})
            }
            _ => r["scene_tracking"][0]["members"][1]["device"] = json!("light"),
        }
        assert!(
            Document::parse(&serde_json::to_vec(&r).unwrap(), &config).is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn reviewed_hap_physical_readback_ignores_the_program_variable_and_does_no_io() {
    for (one, other) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut h = Harness::new(reviewed_rules()).await;
        samples(&mut h, one, other);
        let (store, bridge) = install(&mut h).await;
        assert_eq!(read(&bridge).await.unwrap(), json!(one && !other));
        assert_eq!(bridge.accessories.len(), 1);
        assert!(bridge.activities.is_empty());
        assert_eq!(h.count("command_journal"), 0);
        drop(bridge);
        drop(store);
        h.finish().await;
    }
}

async fn write(h: &mut Harness, bridge: &crate::runtime_scene_hap::RuntimeSceneHap, on: bool) {
    let b = &bridge.accessories[0].0;
    let body =
        serde_json::to_vec(&json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":on}]}))
            .unwrap();
    let handler = bridge.write_handler.clone();
    let member = h.member;
    h.drive(tokio::spawn(async move {
        handler.write_async(member, &body).await
    }))
    .await
    .unwrap();
}
async fn refresh(
    h: &mut Harness,
    bridge: crate::runtime_scene_hap::RuntimeSceneHap,
) -> crate::runtime_scene_hap::RuntimeSceneHap {
    tokio::time::sleep(Duration::from_millis(110)).await;
    h.drive(tokio::spawn(async move {
        let mut bridge = bridge;
        bridge.flush_resets().await.unwrap();
        bridge
    }))
    .await
}

#[tokio::test]
async fn reviewed_hap_repeat_is_absolute_and_inactive_off_preserves_other_lights() {
    let mut h = Harness::new(reviewed_rules()).await;
    samples(&mut h, false, false);
    let (store, mut bridge) = install(&mut h).await;
    for on in [true, true, false, false] {
        let already_off = !on && read(&bridge).await.unwrap() == json!(false);
        write(&mut h, &bridge, on).await;
        let expected = if already_off {
            vec![]
        } else {
            vec![0xae, 3, 1, if on { 0xf1 } else { 0xf2 }, 0xee]
        };
        assert_eq!(h.wire().await, expected);
        tokio::time::sleep(Duration::from_millis(2)).await;
        samples(&mut h, on, false);
        bridge = refresh(&mut h, bridge).await;
        assert_eq!(read(&bridge).await.unwrap(), json!(on));
    }
    // Both lamps externally On is a different pattern. Off must not turn
    // either off simply because the same source program can address them.
    samples(&mut h, true, true);
    bridge = refresh(&mut h, bridge).await;
    assert_eq!(read(&bridge).await.unwrap(), json!(false));
    let before = h.count("command_journal");
    write(&mut h, &bridge, false).await;
    assert!(h.wire().await.is_empty());
    assert_eq!(h.count("command_journal"), before);
    write(&mut h, &bridge, true).await;
    assert_eq!(
        h.wire().await,
        [0xae, 3, 1, 0xf1, 0xee, 0xae, 3, 2, 0xf2, 0xee]
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    samples(&mut h, true, false);
    bridge = refresh(&mut h, bridge).await;
    assert_eq!(read(&bridge).await.unwrap(), json!(true));
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn reviewed_hap_unknown_pending_revoked_writes_and_expired_owner_never_fake_state() {
    let mut h = Harness::new(reviewed_rules()).await;
    let (store, mut bridge) = install(&mut h).await;
    assert!(read(&bridge).await.is_err());
    write(&mut h, &bridge, true).await;
    assert!(
        read(&bridge).await.is_err(),
        "accepted intent is not physical confirmation"
    );
    assert_eq!(
        h.wire().await,
        [0xae, 3, 1, 0xf1, 0xee, 0xae, 3, 2, 0xf2, 0xee]
    );
    let snapshot = h
        .actor
        .variable_snapshot(h.member, h.generation.clone(), vec!["v".into()])
        .await
        .unwrap();
    assert_eq!(
        snapshot["v"].value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    assert!(
        read(&bridge).await.is_err(),
        "stored intent1 cannot replace actual feedback"
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    samples(&mut h, true, false);
    bridge = refresh(&mut h, bridge).await;
    assert_eq!(read(&bridge).await.unwrap(), json!(true));
    h.allowed.store(false, Ordering::Release);
    let before = h.count("command_journal");
    let b = &bridge.accessories[0].0;
    let body = serde_json::to_vec(
        &json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":false}]}),
    )
    .unwrap();
    let handler = bridge.write_handler.clone();
    let member = h.member;
    assert!(
        h.drive(tokio::spawn(async move {
            handler.write_async(member, &body).await
        }))
        .await
        .is_err()
    );
    assert_eq!(h.count("command_journal"), before);
    assert!(
        h.wire().await.is_empty(),
        "revoked write cannot send a prefix"
    );
    // Publication samples with the bridge controller, independently of this
    // member's write grant. Expire that actual task owner to test read denial.
    h.actor.transport_failed().await.unwrap();
    h.actor.recover_synchronized(2, 1).await.unwrap();
    bridge = refresh(&mut h, bridge).await;
    assert!(
        read(&bridge).await.is_err(),
        "expired publisher generation cannot reuse confirmed state"
    );
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn feedback_retention_is_enabled_only_for_actual_feedback_trigger_bindings() {
    let cases = [
        (json!({"kind":"startup_synchronized"}), true, false),
        (
            json!({"kind":"variable_changed","variable":"v"}),
            true,
            false,
        ),
        (
            json!({"kind":"variable_changed","variable":"relay_on"}),
            true,
            true,
        ),
        (
            json!({"kind":"device_changed","device":"relay"}),
            true,
            true,
        ),
        (
            json!({"kind":"device_changed","device":"relay"}),
            false,
            false,
        ),
    ];
    for (trigger, enabled, required) in cases {
        let mut doc = reviewed_rules();
        doc["rules"] = json!([{"id":"feedback-policy","source":"synthetic/retention","review_evidence":"actual trigger index","enabled":enabled,"trigger":trigger,
            "actions":[command("synthetic/trigger-output","marker",json!({"kind":"switch","on":true}))]}]);
        let h = Harness::new(doc).await;
        let configured = crate::configured_rules::ConfiguredRules::load_causal_document(
            h.actor.clone(),
            h.controller,
            &h.document,
            homesense_core::rule::Budget::default(),
        )
        .await
        .unwrap();
        let calendar = crate::event_rules::ScheduledRules::new(
            configured.events,
            vec![],
            1_700_000_000_000,
            1000,
        )
        .await
        .unwrap();
        assert_eq!(calendar.needs_feedback_candidates(), required);
        assert_eq!(
            h.count("command_journal"),
            0,
            "index inspection must not execute a trigger"
        );
        drop(calendar);
        h.finish().await;
    }
}
