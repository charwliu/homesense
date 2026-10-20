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
#[test]
fn hap_room_all_rejects_missing_public_chained_and_conflicting_recipes() {
    let config = Configuration::parse(&data(), ExecutionContext::Simulation).unwrap();
    Document::parse(&serde_json::to_vec(&room_rules()).unwrap(), &config).unwrap();
    for case in 0..6 {
        let mut r = room_rules();
        match case {
            0 => r["scene_tracking"][0]["hap_action"]["on_scene"] = json!("missing"),
            1 => r["scene_tracking"][0]["hap_action"]["on_scene"] = json!("all"),
            2 => r["scene_tracking"][0]["hap_action"]["off_scene"] = json!("all-on"),
            3 => r["scene_metadata"][1]["catalog_hidden"] = json!(false),
            4 => r["scene_tracking"][0]["hap_activity"] = json!(true),
            _ => r["scene_tracking"][0]["aggregation"] = json!("all"),
        }
        assert!(
            Document::parse(&serde_json::to_vec(&r).unwrap(), &config).is_err(),
            "case {case}"
        );
    }
}
