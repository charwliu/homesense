//! Actual HAP action/readback over the ordinary admission and causal owner.
use super::*;

fn room_rules() -> Json {
    let mut r = rules();
    r["rules"] = json!([]);
    r["immediate_scenes"] = json!([
        {"id":"all","enabled":true,"review_evidence":"reviewed relative room action",
         "activate":[command("relative/unchanged","marker",json!({"kind":"switch","on":false}))]},
        {"id":"all-on","enabled":true,"review_evidence":"reviewed original full-on recipe",
         "activate":[command("on/first","relay",json!({"kind":"switch","on":true})),command("on/second","marker",json!({"kind":"switch","on":true}))]},
        {"id":"all-off","enabled":true,"review_evidence":"reviewed complete room shutdown",
         "activate":[command("off/first","relay",json!({"kind":"switch","on":false})),command("off/second","marker",json!({"kind":"switch","on":false}))]}
    ]);
    r["scene_metadata"] = json!([
        {"id":"all","name":"全部"},
        {"id":"all-on","name":"internal full on","catalog_hidden":true},
        {"id":"all-off","name":"internal shutdown","catalog_hidden":true}
    ]);
    r["scene_tracking"] = json!([{"scene":"all","enabled":true,"hap_activity":false,
    "hap_action":{"on_scene":"all-on","off_scene":"all-off"},
    "aggregation":"any","unknown_policy":"require_all_known","pending_policy":"block_scene",
    "control_policy":"separate_action_and_read_only_activity","activity_name":"any actual room lamp",
    "max_age_ms":5000,"review_evidence":"actual reviewed room outputs",
    "members":[
        {"id":"first","device":"relay","predicate":{"kind":"power","on":true},"review_evidence":"first light"},
        {"id":"second","device":"marker","predicate":{"kind":"power","on":true},"review_evidence":"second light"}
    ]}]);
    r
}
fn pattern_rules() -> Json {
    let mut r = room_rules();
    r["scene_tracking"][0]["aggregation"] = json!("all");
    r["scene_tracking"][0]["hap_action"]["readback"] = json!("all_conditions");
    // The App/wall source is a relative program; its HAP On must use the
    // independently reviewed absolute recipe, even if the root would turn off.
    r["immediate_scenes"][0]["activate"] = r["immediate_scenes"][2]["activate"].clone();
    r
}
fn samples(h: &mut Harness, first: bool, second: bool) {
    h.samples(first);
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd2,
                    value: if second { 0xf1 } else { 0xf2 },
                }],
            },
        })
        .unwrap();
}
async fn install(h: &mut Harness) -> (IdentityStore, crate::runtime_scene_hap::RuntimeSceneHap) {
    use homesense_hap::storage::IdentityStore;
    let mut store = IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut store, &hap::Config::default())
        .await
        .unwrap();
    let identity = store.clone();
    let doc = h.document.clone();
    let control = h.control.clone();
    let actor = h.actor.clone();
    let controller = h.controller;
    let member = h.member;
    let bridge = h
        .drive(tokio::spawn(async move {
            crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
                &identity,
                &doc,
                vec![homesense_hap::scene_graph::SceneSpec {
                    id: "all".into(),
                    name: "unrelated fallback name".into(),
                }],
                control,
                actor,
                controller,
                move |who, id| who == member && id == "all",
            )
            .await
        }))
        .await
        .unwrap();
    let bridge = h
        .drive(tokio::spawn(async move {
            let mut bridge = bridge;
            bridge.flush_resets().await.unwrap();
            bridge
        }))
        .await;
    (store, bridge)
}
use homesense_hap::storage::IdentityStore;
async fn read(bridge: &crate::runtime_scene_hap::RuntimeSceneHap) -> hap::Result<Json> {
    let (binding, handle) = &bridge.accessories[0];
    let mut accessory = handle.lock().await;
    accessory
        .get_mut_services()
        .into_iter()
        .flat_map(|s| s.get_mut_characteristics())
        .find(|c| c.get_id() == binding.on_iid)
        .unwrap()
        .get_value()
        .await
}
#[tokio::test]
async fn hap_room_all_reports_any_actual_lamp_without_an_extra_accessory_or_commands() {
    for (first, second) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut h = Harness::new(room_rules()).await;
        samples(&mut h, first, second);
        let (store, bridge) = install(&mut h).await;
        assert_eq!(bridge.accessories.len(), 1);
        assert!(bridge.activities.is_empty());
        assert_eq!(read(&bridge).await.unwrap(), json!(first || second));
        assert_eq!(
            h.count("command_journal"),
            0,
            "readback must not query or control the bus"
        );
        drop(bridge);
        drop(store);
        h.finish().await;
    }
}

#[tokio::test]
async fn hap_load_pattern_requires_every_condition_without_changing_room_any_semantics() {
    for (first, second) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut h = Harness::new(pattern_rules()).await;
        samples(&mut h, first, second);
        let (store, bridge) = install(&mut h).await;
        assert_eq!(read(&bridge).await.unwrap(), json!(first && second));
        assert_eq!(bridge.accessories.len(), 1);
        assert!(bridge.activities.is_empty());
        assert_eq!(h.count("command_journal"), 0);
        drop(bridge);
        drop(store);
        h.finish().await;
    }
}

#[tokio::test]
async fn hap_load_pattern_repeated_writes_are_absolute_and_do_not_run_the_relative_root() {
    let mut h = Harness::new(pattern_rules()).await;
    samples(&mut h, false, false);
    let (store, bridge) = install(&mut h).await;
    let binding = &bridge.accessories[0].0;
    for on in [true, true, false, false] {
        let body = serde_json::to_vec(&json!({"characteristics":[{
            "aid":binding.aid,"iid":binding.on_iid,"value":on
        }]}))
        .unwrap();
        let handler = bridge.write_handler.clone();
        let member = h.member;
        h.drive(tokio::spawn(async move {
            handler.write_async(member, &body).await
        }))
        .await
        .unwrap();
        let code = if on { 0xf1 } else { 0xf2 };
        assert_eq!(
            h.wire().await,
            vec![0xae, 3, 1, code, 0xee, 0xae, 3, 2, code, 0xee]
        );
    }
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[test]
fn hap_load_pattern_rejects_incomplete_relative_or_out_of_scope_recipes() {
    let config = Configuration::parse(&data(), ExecutionContext::Simulation).unwrap();
    Document::parse(&serde_json::to_vec(&pattern_rules()).unwrap(), &config).unwrap();
    for case in 0..10 {
        let mut r = pattern_rules();
        match case {
            0 => r["scene_tracking"][0]["aggregation"] = json!("any"),
            1 => r["immediate_scenes"][1]["activate"]
                .as_array_mut()
                .unwrap()
                .truncate(1),
            2 => r["immediate_scenes"][2]["activate"][0]["action"]["command"]["on"] = json!(true),
            3 => r["immediate_scenes"][1]["activate"][0]["action"]["command"]["on"] = json!(false),
            4 => r["scene_tracking"][0]["members"]
                .as_array_mut()
                .unwrap()
                .truncate(1),
            5 => {
                r["immediate_scenes"][1]["activate"][0]["action"] =
                    json!({"kind":"scene","id":"all-off","activate":true})
            }
            6 => r["immediate_scenes"][0]["activate"]
                .as_array_mut()
                .unwrap()
                .truncate(1),
            7 => {
                r["scene_tracking"][0]["members"][0]["predicate"] =
                    json!({"kind":"ignore","reason":"not a pattern condition"})
            }
            8 => {
                for member in r["scene_tracking"][0]["members"].as_array_mut().unwrap() {
                    member["predicate"]["on"] = json!(false);
                }
                r["immediate_scenes"][1]["activate"] = r["immediate_scenes"][2]["activate"].clone();
            }
            _ => r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().push(json!({
                "source":"unsupported/source/indirection", "action":{"kind":"scene","id":"all-off","activate":true}
            })),
        }
        assert!(
            Document::parse(&serde_json::to_vec(&r).unwrap(), &config).is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn hap_room_all_batched_load_projection_preserves_partial_pending_and_authority() {
    let mut h = Harness::new(room_rules()).await;
    samples(&mut h, false, true);
    let policies = (0..34)
        .map(|n| {
            let mut policy = h.document.scene_tracking["all"].clone();
            policy.scene = format!("load-{n}");
            if n % 2 == 1 {
                policy.aggregation = homesense_config::scene_tracking::Aggregation::All;
            }
            policy
        })
        .collect::<Vec<_>>();
    let refs = policies.iter().collect::<Vec<_>>();
    let read =
        crate::scene_tracking::project_many(&refs, &h.actor, h.member, &h.generation, &h.feedback)
            .await;
    for (n, value) in read.iter().enumerate() {
        assert_eq!(value.active(), Some(n % 2 == 0));
    }
    assert_eq!(
        h.count("command_journal"),
        0,
        "load projection must never query or control the bus"
    );
    h.actor
        .submit(homesense_hap::requests::Batch {
            controller: h.member,
            subscriptions: vec![],
            intents: vec![homesense_hap::requests::Intent {
                device: homesense_core::DeviceId::new("relay").unwrap(),
                command: homesense_core::Command::Switch(true),
            }],
        })
        .await
        .unwrap();
    let read =
        crate::scene_tracking::project_many(&refs, &h.actor, h.member, &h.generation, &h.feedback)
            .await;
    assert!(
        read.iter()
            .all(|p| p.activity == crate::scene_activity::SceneActivity::Pending)
    );
    assert_eq!(h.wire().await, [0xae, 3, 1, 0xf1, 0xee]);
    tokio::time::sleep(Duration::from_millis(2)).await;
    samples(&mut h, true, true);
    let read =
        crate::scene_tracking::project_many(&refs, &h.actor, h.member, &h.generation, &h.feedback)
            .await;
    assert!(read.iter().all(|p| p.active() == Some(true)));
    h.allowed.store(false, Ordering::Release);
    let read =
        crate::scene_tracking::project_many(&refs, &h.actor, h.member, &h.generation, &h.feedback)
            .await;
    assert!(
        read.iter()
            .all(|p| p.active().is_none() && p.valid_until_ms.is_none())
    );
    assert_eq!(h.count("command_journal"), 1);
    h.finish().await;
}
#[tokio::test]
async fn hap_room_all_uses_absolute_recipes_for_repeated_on_and_off_not_the_relative_root() {
    let mut h = Harness::new(room_rules()).await;
    samples(&mut h, false, false);
    let (store, bridge) = install(&mut h).await;
    let b = &bridge.accessories[0].0;
    for on in [true, true, false, false] {
        let body = serde_json::to_vec(
            &json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":on}]}),
        )
        .unwrap();
        let handler = bridge.write_handler.clone();
        let member = h.member;
        h.drive(tokio::spawn(async move {
            handler.write_async(member, &body).await
        }))
        .await
        .unwrap();
        assert_eq!(
            h.wire().await,
            vec![
                0xae,
                3,
                1,
                if on { 0xf1 } else { 0xf2 },
                0xee,
                0xae,
                3,
                2,
                if on { 0xf1 } else { 0xf2 },
                0xee
            ]
        );
    }
    drop(bridge);
    drop(store);
    h.finish().await;
}
#[tokio::test]
async fn hap_room_all_keeps_unknown_state_and_unauthorized_writes_unavailable() {
    let mut h = Harness::new(room_rules()).await;
    let (store, bridge) = install(&mut h).await;
    assert!(read(&bridge).await.is_err());
    let b = &bridge.accessories[0].0;
    let body =
        serde_json::to_vec(&json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":true}]}))
            .unwrap();
    assert_eq!(
        bridge
            .write_handler
            .write_async(Uuid::new_v4(), &body)
            .await,
        Err(hap::server::WriteStatus::InsufficientPrivileges)
    );
    assert_eq!(h.count("command_journal"), 0);
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn hap_room_all_accepted_write_survives_projection_timeout_without_replaying_controls() {
    let mut h = Harness::new(room_rules()).await;
    samples(&mut h, false, false);
    let (store, bridge) = install(&mut h).await;
    let binding = &bridge.accessories[0].0;
    let body = serde_json::to_vec(&json!({"characteristics":[{
        "aid":binding.aid,"iid":binding.on_iid,"value":true
    }]}))
    .unwrap();
    let handler = bridge.write_handler.clone();
    let member = h.member;
    let task = tokio::spawn(async move { handler.write_async(member, &body).await });
    while h.count("command_journal") == 0 {
        h.receiver
            .poll_causal(
                &h.actor,
                &h.document,
                &mut h.scheduler,
                &h.feedback,
                5000,
                None,
                Some(&mut h.router),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // Leave the read-only SceneLoadStates request unserviced past its 500 ms
    // budget, after the actual two-output write was durably accepted.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        read(&bridge).await.is_err(),
        "a timed-out read cannot invent state"
    );
    assert_eq!(
        h.count("command_journal"),
        2,
        "projection timeout must not retry controls"
    );
    assert_eq!(
        h.wire().await,
        [0xae, 3, 1, 0xf1, 0xee, 0xae, 3, 2, 0xf1, 0xee]
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    samples(&mut h, true, true);
    let bridge = h
        .drive(tokio::spawn(async move {
            let mut bridge = bridge;
            bridge.flush_resets().await.unwrap();
            bridge
        }))
        .await;
    assert_eq!(read(&bridge).await.unwrap(), json!(true));
    assert_eq!(h.count("command_journal"), 2);
    drop(bridge);
    drop(store);
    h.finish().await;
}
#[test]
fn hap_room_all_rejects_missing_public_chained_and_conflicting_recipes() {
    let config = Configuration::parse(&data(), ExecutionContext::Simulation).unwrap();
    let document = Document::parse(&serde_json::to_vec(&room_rules()).unwrap(), &config).unwrap();
    assert_eq!(document.scene_confirmation_id("all-on"), "all");
    assert_eq!(document.scene_confirmation_id("all-off"), "all");
    assert_eq!(document.scene_confirmation_id("unrelated"), "unrelated");
    for case in 0..7 {
        let mut r = room_rules();
        match case {
            0 => r["scene_tracking"][0]["hap_action"]["on_scene"] = json!("missing"),
            1 => r["scene_tracking"][0]["hap_action"]["on_scene"] = json!("all"),
            2 => r["scene_tracking"][0]["hap_action"]["off_scene"] = json!("all-on"),
            3 => r["scene_metadata"][1]["catalog_hidden"] = json!(false),
            4 => r["scene_tracking"][0]["hap_activity"] = json!(true),
            5 => r["scene_tracking"][0]["aggregation"] = json!("all"),
            _ => {
                let mut other = r["scene_tracking"][0].clone();
                other["scene"] = json!("other-all");
                r["scene_tracking"].as_array_mut().unwrap().push(other);
                r["immediate_scenes"].as_array_mut().unwrap().push(json!({
                    "id":"other-all", "enabled":true, "review_evidence":"another public action",
                    "activate":[command("other/output","marker",json!({"kind":"switch","on":false}))]
                }));
                r["scene_metadata"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"other-all","name":"Other all"}));
            }
        }
        assert!(
            Document::parse(&serde_json::to_vec(&r).unwrap(), &config).is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn hap_room_all_absolute_recipe_retains_canonical_feedback_confirmation() {
    for on in [true, false] {
        let value = i64::from(on);
        let mut r = room_rules();
        r["immediate_scenes"][0]["activate"] = json!([{
            "source":"relative/on", "action":{"kind":"scene","id":"all-on","activate":true}
        }]);
        r["immediate_scenes"][if on { 1 } else { 2 }]["activate"].as_array_mut().unwrap().push(json!({
        "source":"recipe/selection", "action":{"kind":"set","variable":"v","value":{"kind":"integer","value":value}}
    }));
        r["component_mode_confirmations"] = json!([{
            "id":"room-mode", "variable":"v", "scenes":["all"], "first_value":value,
            "confirm_when_known":true, "when_values":{"v":value}, "timeout_ms":5000,
            "max_age_ms":5000, "review_evidence":"selection requires written physical confirmation",
            "postconditions":[{"device":"relay","on":on},{"device":"marker","on":on}]
        }]);
        let mut h = Harness::new(r).await;
        samples(&mut h, !on, !on);
        let (store, bridge) = install(&mut h).await;
        let b = &bridge.accessories[0].0;
        let body = serde_json::to_vec(
            &json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":on}]}),
        )
        .unwrap();
        let handler = bridge.write_handler.clone();
        let member = h.member;
        h.drive(tokio::spawn(async move {
            handler.write_async(member, &body).await
        }))
        .await
        .unwrap();
        let snapshot = h
            .actor
            .variable_snapshot(h.member, h.generation.clone(), vec!["v".into()])
            .await
            .unwrap();
        assert_eq!(
            snapshot["v"].value, None,
            "HAP recipe must retain the canonical source's feedback gate before any wire write"
        );
        assert_eq!(h.wire().await.len(), 10);
        tokio::time::sleep(Duration::from_millis(2)).await;
        samples(&mut h, on, on);
        mode_confirm(&h).await;
        assert_eq!(
            mode_value(&h, "v"),
            Some(homesense_core::rule::Value::Integer(value)),
            "both absolute recipes must commit after their actual written outputs are confirmed"
        );
        drop(bridge);
        drop(store);
        h.finish().await;
    }
}
