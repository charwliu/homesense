use super::*;

async fn presentation_hap_read(
    bridge: &crate::runtime_scene_hap::RuntimeSceneHap,
    scene: &str,
) -> hap::Result<Json> {
    let (binding, handle) = bridge.accessories.iter()
        .find(|(binding, _)| binding.scene == scene).unwrap();
    let mut accessory = handle.lock().await;
    accessory.get_mut_services().into_iter()
        .flat_map(|s| s.get_mut_characteristics())
        .find(|c| c.get_id() == binding.on_iid).unwrap().get_value().await
}

#[tokio::test]
async fn presentation_hap_subscriptions_never_execute_scenes_including_mixed_requests() {
    use homesense_hap::storage::IdentityStore;
    let (cfg, doc) = inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    let mut store = IdentityStore::open(h.directory.join("subscription-hap")).await.unwrap();
    hap::storage::Storage::save_config(&mut store, &hap::Config::default()).await.unwrap();
    let identity = store.clone();
    let document = h.document.clone();
    let control = h.control.clone();
    let actor = h.actor.clone();
    let controller = h.controller;
    let member = h.member;
    let bridge = h.drive(tokio::spawn(async move {
        crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
            &identity, &document,
            ["act", "other"].into_iter().map(|id| homesense_hap::scene_graph::SceneSpec {
                id: id.into(), name: id.into(),
            }).collect(), control, actor, controller,
            move |who, scene| who == member && matches!(scene, "act" | "other"),
        ).await
    })).await.unwrap();
    let characteristic = |scene: &str, value: Option<bool>, ev: bool| {
        let binding = &bridge.accessories.iter().find(|(b, _)| b.scene == scene).unwrap().0;
        let mut item = json!({"aid":binding.aid,"iid":binding.on_iid,"ev":ev});
        if let Some(value) = value { item["value"] = json!(value); }
        item
    };
    // Home subscribes again when reconnecting or opening a room. These are
    // notifications-only requests even if the physical selection is unknown.
    for (ev, both) in [(true, false), (false, false), (true, true)] {
        let mut items = vec![characteristic("act", None, ev)];
        if both { items.push(characteristic("other", None, ev)); }
        let body = serde_json::to_vec(&json!({"characteristics":items})).unwrap();
        let handler = bridge.write_handler.clone();
        h.drive(tokio::spawn(async move { handler.write_async(member, &body).await })).await.unwrap();
        assert_eq!(h.count("command_journal"), 0, "subscription must not execute either scene");
    }
    // A real value and an unrelated subscription can share one HAP request.
    let body = serde_json::to_vec(&json!({"characteristics":[
        characteristic("act", Some(true), true), characteristic("other", None, true)
    ]})).unwrap();
    let handler = bridge.write_handler.clone();
    h.drive(tokio::spawn(async move { handler.write_async(member, &body).await })).await.unwrap();
    settle(&mut h, true).await;
    assert_eq!(details(&mut h).await["act"].selected, Some(true));
    assert_eq!(details(&mut h).await["other"].selected, Some(false));
    let before = h.count("command_journal");
    let body = serde_json::to_vec(&json!({"characteristics":[characteristic("act", None, true)]})).unwrap();
    let handler = bridge.write_handler.clone();
    h.drive(tokio::spawn(async move { handler.write_async(member, &body).await })).await.unwrap();
    assert_eq!(h.count("command_journal"), before, "resubscribe must not toggle an active scene off");
    assert_eq!(details(&mut h).await["act"].selected, Some(true));
    let body = serde_json::to_vec(&json!({"characteristics":[
        characteristic("act", Some(false), true), characteristic("other", None, false)
    ]})).unwrap();
    let handler = bridge.write_handler.clone();
    h.drive(tokio::spawn(async move { handler.write_async(member, &body).await })).await.unwrap();
    settle(&mut h, false).await;
    assert_eq!(details(&mut h).await["act"].selected, Some(false));
    assert_eq!(details(&mut h).await["other"].selected, Some(false));
    let before = h.count("command_journal");
    let body = serde_json::to_vec(&json!({"characteristics":[characteristic("act", None, true)]})).unwrap();
    assert_eq!(bridge.write_handler.write_async(Uuid::new_v4(), &body).await,
        Err(hap::server::WriteStatus::InsufficientPrivileges));
    assert_eq!(h.count("command_journal"), before);
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn public_presentation_hap_uses_confirmed_provider_without_aliasing_actions_or_permissions() {
    use homesense_hap::storage::IdentityStore;
    let (cfg, doc) = inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    let mut store = IdentityStore::open(h.directory.join("presentation-hap")).await.unwrap();
    hap::storage::Storage::save_config(&mut store, &hap::Config::default()).await.unwrap();
    let identity = store.clone();
    let document = h.document.clone();
    let control = h.control.clone();
    let actor = h.actor.clone();
    let controller = h.controller;
    let member = h.member;
    let mut bridge = h.drive(tokio::spawn(async move {
        crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
            &identity, &document,
            ["act", "other"].into_iter().map(|id| homesense_hap::scene_graph::SceneSpec {
                id: id.into(), name: id.into(),
            }).collect(), control, actor, controller,
            move |who, scene| who == member && matches!(scene, "act" | "other"),
        ).await
    })).await.unwrap();
    assert_eq!(bridge.accessories.len(), 2);
    assert!(bridge.activities.is_empty());
    assert!(bridge.accessories.iter().all(|(b, _)| matches!(b.scene.as_str(), "act" | "other")));
    assert!(presentation_hap_read(&bridge, "act").await.is_err(), "unknown cannot become false");
    assert_eq!(h.count("command_journal"), 0, "HAP reads must not query or control the bus");
    // Explicit On is idempotent, and distinct source modes with identical
    // physical targets still have distinct confirmed readback.
    for (scene, on) in [("act", false), ("act", true), ("act", true), ("other", true), ("other", true), ("other", false)] {
        let binding = &bridge.accessories.iter().find(|(b, _)| b.scene == scene).unwrap().0;
        let body = serde_json::to_vec(&json!({"characteristics":[{
            "aid":binding.aid, "iid":binding.on_iid, "value":on
        }]})).unwrap();
        let handler = bridge.write_handler.clone();
        h.drive(tokio::spawn(async move { handler.write_async(member, &body).await })).await.unwrap();
        assert!(presentation_hap_read(&bridge, scene).await.is_err(), "pending write cannot become confirmed selection");
        settle(&mut h, on).await;
        // Match the production projection cadence; a second flush inside the
        // 100 ms sampling interval deliberately retains the pending cache.
        tokio::time::sleep(Duration::from_millis(110)).await;
        bridge = h.drive(tokio::spawn(async move {
            bridge.flush_resets().await.unwrap();
            bridge
        })).await;
        for id in ["act", "other"] {
            assert_eq!(presentation_hap_read(&bridge, id).await
                .unwrap_or_else(|e| panic!("{scene}={on}, read {id}: {e:?}")), json!(on && id == scene));
        }
    }
    let before = h.count("command_journal");
    let binding = &bridge.accessories[0].0;
    let body = serde_json::to_vec(&json!({"characteristics":[{
        "aid":binding.aid, "iid":binding.on_iid, "value":true
    }]})).unwrap();
    assert_eq!(bridge.write_handler.write_async(Uuid::new_v4(), &body).await,
        Err(hap::server::WriteStatus::InsufficientPrivileges));
    assert_eq!(h.count("command_journal"), before);
    drop(bridge);
    drop(store);
    h.finish().await;
}

fn inputs() -> (Vec<u8>, Json) {
    let mut cfg: Json = serde_json::from_slice(&data()).unwrap();
    cfg["gateways"][0]["modules"][0]["profile"] = json!("ad8_din_rs485_v2_2_1");
    let set = |v| json!({"source":"synthetic/select","action":{"kind":"set","variable":"selection","value":{"kind":"integer","value":v}}});
    let profile = |id, level| json!({"id":id,"enabled":true,"review_evidence":"synthetic target","max_lateness_ms":1000,"steps":[{"source":"synthetic/profile","offset_ms":0,"device":"light","command":{"kind":"level","value":level,"ramp":false}}]});
    let mut doc = json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"immediate_scenes":[],"timed_scenes":[profile("profile-act",100),profile("profile-other",100),profile("off",0)],"scene_switch_groups":[{"id":"group","variable":"selection","review_evidence":"synthetic duplicate physical profile","members":[{"scene":"profile-act","value":1,"off_scene":"off"},{"scene":"profile-other","value":2,"off_scene":"off"}]}],"scene_metadata":[],"scene_presentations":[],"component_mode_confirmations":[]});
    for (id, value) in [("act", 1), ("other", 2)] {
        doc["immediate_scenes"].as_array_mut().unwrap().push(json!({"id":id,"enabled":true,"review_evidence":"synthetic absolute source","activate":[command("source/on","light",level(100)),set(value)],"deactivate":[command("source/off","light",level(0)),set(0)]}));
        doc["scene_presentations"].as_array_mut().unwrap().push(json!({"scene":id,"selection_scene":format!("profile-{id}"),"review_evidence":"synthetic single public entry"}));
        for (target, on) in [(value, true), (0, false)] {
            doc["component_mode_confirmations"].as_array_mut().unwrap().push(json!({"id":format!("{id}-{target}"),"variable":"selection","scenes":[id],"first_value":target,"confirm_when_known":true,"shared_selection_group":"group","when_values":{"selection":target},"max_age_ms":5000,"timeout_ms":15000,"review_evidence":"synthetic real post-write confirmation","postconditions":[{"device":"light","on":on}]}));
        }
    }
    (serde_json::to_vec(&cfg).unwrap(), doc)
}
async fn settle(h: &mut Harness, on: bool) {
    let bytes = h.wire().await;
    assert_eq!(bytes, vec![0xae, 0x15, 1, if on { 100 } else { 0 }, 0xee]);
    let pending = h
        .actor
        .scene_selection_snapshot(h.member, h.generation.clone(), vec!["selection".into()])
        .await
        .unwrap();
    assert_eq!(
        pending["selection"].stored.value, None,
        "Written must not publish the new mode; prior audit value may remain stored"
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(h, 0x15, 1, if on { 100 } else { 0 });
    assert_eq!(mode_confirm(h).await, 1);
}
async fn details(
    h: &mut Harness,
) -> std::collections::BTreeMap<String, crate::scene_switches::Details> {
    let Reply::SceneSelectionDetails(d) = h
        .request(Action::SceneSelectionDetails {
            controller: h.member,
            revision: 1,
        })
        .await
        .unwrap()
    else {
        panic!("selection reply")
    };
    d
}
#[tokio::test]
async fn absolute_room_on_off_and_guarded_key_press_share_one_confirmed_member() {
    let (cfg, doc) = inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    assert_eq!(details(&mut h).await["act"].selected, None);
    // Explicit Off works even when the source mode is unknown.
    h.request(Action::SceneAction {
        controller: h.member,
        revision: 1,
        scene: "act".into(),
        execute: false,
    })
    .await
    .unwrap();
    settle(&mut h, false).await;
    assert_eq!(details(&mut h).await["act"].selected, Some(false));
    for _ in 0..2 {
        h.request(Action::SceneAction {
            controller: h.member,
            revision: 1,
            scene: "act".into(),
            execute: true,
        })
        .await
        .unwrap();
        settle(&mut h, true).await;
        assert_eq!(details(&mut h).await["act"].selected, Some(true));
    }
    h.request(Action::SceneWrites {
        controller: h.member,
        revision: 1,
        scenes: vec![("act".into(), None)],
    })
    .await
    .unwrap();
    settle(&mut h, false).await;
    h.request(Action::SceneWrites {
        controller: h.member,
        revision: 1,
        scenes: vec![("other".into(), None)],
    })
    .await
    .unwrap();
    settle(&mut h, true).await;
    let d = details(&mut h).await;
    assert_eq!(d["act"].selected, Some(false));
    assert_eq!(d["other"].selected, Some(true));
    assert_eq!(
        d["other"].selection_source,
        "local_written_feedback_inference"
    );
    assert!(!d["other"].relative_unknown_allowed);
    // Same physical profile, different source: select On, never toggle Off.
    h.request(Action::SceneWrites {
        controller: h.member,
        revision: 1,
        scenes: vec![("act".into(), None)],
    })
    .await
    .unwrap();
    settle(&mut h, true).await;
    assert_eq!(details(&mut h).await["act"].selected, Some(true));
    h.request(Action::SceneWrites {
        controller: h.member,
        revision: 1,
        scenes: vec![("act".into(), None)],
    })
    .await
    .unwrap();
    settle(&mut h, false).await;
    h.finish().await;
}
#[tokio::test]
async fn internal_profiles_are_absent_and_denied_even_with_an_explicit_profile_grant() {
    let (cfg, doc) = inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    let api = web(&h);
    let token = "synthetic-presentation-profile-abcdefghijklmnopqrstuvwxyz";
    api.tokens
        .insert(
            Uuid::new_v4(),
            token,
            crate::web_auth::Grant {
                controller: h.member,
                role: crate::web_auth::Role::Member,
                scenes: [
                    "act".into(),
                    "other".into(),
                    "profile-act".into(),
                    "profile-other".into(),
                    "off".into(),
                ]
                .into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    async fn request(
        h: &mut Harness,
        api: Arc<crate::web_api::Api>,
        token: &str,
        body: Option<Json>,
    ) -> (hyper::StatusCode, Json) {
        use http_body_util::BodyExt;
        let req = hyper::Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(if body.is_some() {
                "/api/v1/scene-actions"
            } else {
                "/api/v1/scenes"
            })
            .header("host", "localhost")
            .header("origin", "http://localhost")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(hyper::body::Bytes::from(
                body.map_or_else(String::new, |v| v.to_string()),
            ))
            .unwrap();
        let response = h
            .drive(tokio::spawn(async move { api.handle(req).await }))
            .await;
        let status = response.status();
        (
            status,
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap(),
        )
    }
    let (status, list) = request(&mut h, api.clone(), token, None).await;
    assert_eq!(status, hyper::StatusCode::OK);
    let ids = list["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["act", "off", "other"]);
    let action = list["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "act")
        .unwrap();
    assert_eq!(action["switch_kind"], "selection");
    assert_eq!(action["selected"], Json::Null);
    for id in ["profile-act", "profile-other"] {
        let (status, _) = request(
            &mut h,
            api.clone(),
            token,
            Some(json!({"scene":id,"on":true,"request_id":format!("blocked-{id}")})),
        )
        .await;
        assert_eq!(status, hyper::StatusCode::FORBIDDEN);
    }
    assert_eq!(h.count("command_journal"), 0);
    let (status, _) = request(
        &mut h,
        api.clone(),
        token,
        Some(json!({"scene":"act","on":false,"request_id":"explicit-off"})),
    )
    .await;
    assert_eq!(status, hyper::StatusCode::ACCEPTED);
    settle(&mut h, false).await;
    let (status, list) = request(&mut h, api.clone(), token, None).await;
    assert_eq!(status, hyper::StatusCode::OK);
    assert_eq!(list["scenes"][0]["selected"], false);
    let (status, _) = web_request(
        &mut h,
        api,
        true,
        Some(json!({"scene":"act","on":true,"request_id":"viewer-denied"})),
    )
    .await;
    assert_eq!(status, hyper::StatusCode::FORBIDDEN);
    h.finish().await;
}

#[tokio::test]
async fn conflicting_public_members_and_internal_profile_batch_reject_before_output() {
    let (cfg, doc) = inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    for batch in [
        vec![("act".into(), None), ("other".into(), None)],
        vec![
            ("act".into(), Some(true)),
            ("profile-other".into(), Some(true)),
        ],
    ] {
        assert!(
            h.request(Action::SceneWrites {
                controller: h.member,
                revision: 1,
                scenes: batch
            })
            .await
            .is_err()
        );
        assert_eq!(h.count("command_journal"), 0);
        assert_eq!(mode_value(&h, "selection"), None);
    }
    h.finish().await;
}

fn unique_physical_selection_inputs() -> (Vec<u8>, Json) {
    let (cfg, mut doc) = inputs();
    doc["immediate_scenes"].as_array_mut().unwrap().retain(|s| s["id"] == "act");
    doc["timed_scenes"].as_array_mut().unwrap().retain(|s| s["id"] != "profile-other");
    doc["scene_presentations"].as_array_mut().unwrap().retain(|s| s["scene"] == "act");
    doc["component_mode_confirmations"].as_array_mut().unwrap().retain(|p| p["scenes"][0] == "act");
    doc["scene_switch_groups"][0]["members"].as_array_mut().unwrap().retain(|m| m["scene"] == "profile-act");
    doc["scene_switch_groups"][0]["reconciliation"] = json!({"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"synthetic unique actual profile"});
    (cfg, doc)
}

#[tokio::test]
async fn keypad_presentation_toggle_uses_same_current_physical_selection_as_catalog() {
    let (cfg, doc) = unique_physical_selection_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    mode_frame(&mut h, 0x15, 1, 100);
    crate::scene_reconciliation::tick(&h.actor, h.member, &h.document, &h.scheduler, &h.feedback).await.unwrap();
    assert_eq!(details(&mut h).await["act"].selected, Some(true));
    let stored = Store::open(h.directory.join("runtime.sqlite")).unwrap().logical_variable(1, "selection").unwrap().unwrap();
    assert_eq!(stored.value, None, "physical selection must not rewrite historical source intent");
    h.request(Action::SceneWrites { controller: h.member, revision: 1, scenes: vec![("act".into(), None)] }).await.unwrap();
    assert_eq!(h.wire().await, [0xae, 0x15, 1, 0, 0xee], "a currently selected presentation must close, not reopen from stale stored intent");
    h.finish().await;
}

#[tokio::test]
async fn keypad_presentation_off_truth_supersedes_historical_on_intent() {
    let (cfg, doc) = unique_physical_selection_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    set_value(&h, "selection", 1).await;
    mode_frame(&mut h, 0x15, 1, 0);
    crate::scene_reconciliation::tick(&h.actor, h.member, &h.document, &h.scheduler, &h.feedback).await.unwrap();
    assert_eq!(details(&mut h).await["act"].selected, Some(false));
    assert_eq!(mode_value(&h, "selection"), Some(homesense_core::rule::Value::Integer(1)));
    h.request(Action::SceneWrites { controller: h.member, revision: 1, scenes: vec![("act".into(), None)] }).await.unwrap();
    assert_eq!(h.wire().await, [0xae, 0x15, 1, 100, 0xee]);
    h.finish().await;
}

#[tokio::test]
async fn presentation_selection_read_is_rechecked_before_any_output() {
    for changed in [false, true] {
        let (cfg, doc) = unique_physical_selection_inputs();
        let mut h = Harness::from_inputs(cfg, doc).await;
        mode_frame(&mut h, 0x15, 1, 100);
        crate::scene_reconciliation::tick(&h.actor, h.member, &h.document, &h.scheduler, &h.feedback).await.unwrap();
        let captured = crate::manual_scene_plan::capture(&h.actor, h.member, &h.generation, &h.document, &[("act".into(), None)]).await.ok().unwrap();
        assert_eq!(captured.selection_reads["selection"].stored.value, Some(homesense_core::rule::Value::Integer(1)));
        if changed {
            mode_frame(&mut h, 0x15, 1, 0);
            crate::scene_reconciliation::tick(&h.actor, h.member, &h.document, &h.scheduler, &h.feedback).await.unwrap();
        }
        let mut payload = guard_payload(&h);
        payload.variables.clear();
        payload.batch.intents[0].command = homesense_core::Command::SetLevel { level: homesense_core::Percent::new(0).unwrap(), transition: homesense_core::LevelTransition::Immediate };
        let result = h.actor.submit_deferred_rule_for_entry(h.generation.clone(), payload, None, Some(vec!["synthetic/presentation-read".into()]), crate::admission::RuleEntry {selection_reads:Some(captured.selection_reads), ..Default::default()}).await;
        if changed {
            assert!(result.is_err(), "a newer physical selection cannot inherit an old decision");
            assert_eq!(h.count("command_journal"), 0);
        } else {
            result.unwrap();
            assert_eq!(h.wire().await, [0xae, 0x15, 1, 0, 0xee]);
        }
        h.finish().await;
    }
}
