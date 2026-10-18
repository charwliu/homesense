use super::*;
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
