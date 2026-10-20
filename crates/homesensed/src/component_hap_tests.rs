//! Absolute HAP intent must retain the component program's causal ownership.
use super::*;
use homesense_hap::storage::IdentityStore;

fn inputs() -> (Vec<u8>, Json) {
    let (cfg, mut d) = recorded_program_inputs();
    let root = &d["immediate_scenes"][0]["activate"][0]["action"];
    let on = root["then_actions"].clone();
    let off = root["else_actions"][0]["action"]["then_actions"].clone();
    for (id, nodes) in [("absolute-on", on), ("absolute-off", off)] {
        assert!(nodes.is_array());
        d["immediate_scenes"].as_array_mut().unwrap().push(json!({
            "id":id,"enabled":true,"review_evidence":"Synthetic absolute branch, no relative execution","activate":nodes
        }));
    }
    d["scene_metadata"] = json!([
        {"id":"act","name":"Synthetic component"},
        {"id":"absolute-on","name":"Internal on","catalog_hidden":true},
        {"id":"absolute-off","name":"Internal off","catalog_hidden":true}
    ]);
    d["scene_component_activity"][0]["hap_action"] = json!({
        "on_scene":"absolute-on","off_scene":"absolute-off","readback":"confirmed_component"
    });
    (cfg, d)
}
async fn install(h: &mut Harness) -> (IdentityStore, crate::runtime_scene_hap::RuntimeSceneHap) {
    let mut identity = IdentityStore::open(h.directory.join("hap-identity"))
        .await
        .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let store = identity.clone();
    let doc = h.document.clone();
    let control = h.control.clone();
    let actor = h.actor.clone();
    let who = h.controller;
    let member = h.member;
    let bridge = h
        .drive(tokio::spawn(async move {
            crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
                &store,
                &doc,
                vec![homesense_hap::scene_graph::SceneSpec {
                    id: "act".into(),
                    name: "Synthetic component".into(),
                }],
                control,
                actor,
                who,
                move |id, scene| id == member && scene == "act",
            )
            .await
        }))
        .await
        .unwrap();
    (identity, bridge)
}
async fn refresh(
    h: &mut Harness,
    bridge: crate::runtime_scene_hap::RuntimeSceneHap,
) -> crate::runtime_scene_hap::RuntimeSceneHap {
    // The projection owner samples at100ms. A completed physical proof must
    // not be tested by forcing a second flush inside the previous interval.
    tokio::time::sleep(Duration::from_millis(110)).await;
    h.drive(tokio::spawn(async move {
        let mut bridge = bridge;
        bridge.flush_resets().await.unwrap();
        bridge
    }))
    .await
}
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
async fn write(
    h: &mut Harness,
    bridge: &crate::runtime_scene_hap::RuntimeSceneHap,
    on: bool,
) -> Result<(), hap::server::WriteStatus> {
    let binding = &bridge.accessories[0].0;
    let body = serde_json::to_vec(
        &json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":on}]}),
    )
    .unwrap();
    let handler = bridge.write_handler.clone();
    let member = h.member;
    h.drive(tokio::spawn(async move {
        handler.write_async(member, &body).await
    }))
    .await
}

#[test]
fn component_hap_schema_rejects_wrong_provider_new_operations_and_recipe_scope() {
    let (cfg, d) = inputs();
    let config = Configuration::parse(&cfg, ExecutionContext::Simulation).unwrap();
    let doc = Document::parse(&serde_json::to_vec(&d).unwrap(), &config).unwrap();
    assert_eq!(doc.scene_confirmation_id("absolute-on"), "act");
    assert_eq!(doc.scene_confirmation_id("absolute-off"), "act");
    for case in 0..9 {
        let mut bad = d.clone();
        match case {
            0 => bad["scene_component_activity"][0]["hap_action"]["readback"] = json!("any_lamp"),
            1 => {
                bad["scene_component_activity"][0]["hap_action"]["off_scene"] = json!("absolute-on")
            }
            2 => bad["scene_metadata"][1]["catalog_hidden"] = json!(false),
            3 => bad["immediate_scenes"][2]["activate"][0]["action"]["device"] = json!("marker"),
            4 => bad["immediate_scenes"][2]["activate"][1]["action"]["value"]["value"] = json!(99),
            5 => bad["immediate_scenes"][2]["activate"][1]["action"]["variable"] = json!("w"),
            6 => {
                bad["immediate_scenes"][2]["activate"][0]["action"] =
                    json!({"kind":"scene","id":"other","activate":true})
            }
            7 => bad["scene_metadata"][1]["scope"] = json!("household"),
            _ => {
                bad["immediate_scenes"][2]["activate"][0]["action"]["command"] =
                    json!({"kind":"query"})
            }
        }
        assert!(
            Document::parse(&serde_json::to_vec(&bad).unwrap(), &config).is_err(),
            "case{case}"
        );
    }
}

#[tokio::test]
async fn component_hap_keeps_saved_mode_and_matching_lights_unknown_without_owned_execution() {
    let (cfg, d) = inputs();
    let mut h = Harness::from_inputs(cfg, d).await;
    seed_mode(&h, "v", 1);
    mode_frame(&mut h, 3, 1, 0xf1);
    let (store, bridge) = install(&mut h).await;
    let bridge = refresh(&mut h, bridge).await;
    assert!(
        read(&bridge).await.is_err(),
        "a saved mode and matching light cannot become HAP On"
    );
    assert_eq!(h.count("command_journal"), 0);
    drop(bridge);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn component_hap_confirmation_alias_does_not_authorize_recipe_or_retarget_source() {
    use crate::{
        source_auth::Source,
        web_auth::{Grant, Role, Tokens},
    };
    let (cfg, d) = inputs();
    let mut h = Harness::from_inputs(cfg, d).await;
    let tokens = Arc::new(Tokens::default());
    const SECRET: &str = "synthetic-component-hap-scope-0123456789";
    tokens
        .insert(
            Uuid::new_v4(),
            SECRET,
            Grant {
                controller: h.member,
                role: Role::Member,
                scenes: ["act".into()].into(),
                devices: h
                    .configuration
                    .devices()
                    .keys()
                    .map(|d| d.as_str().to_owned())
                    .collect(),
            },
        )
        .unwrap();
    let action = Action::SceneAction {
        controller: h.member,
        revision: 1,
        scene: "absolute-on".into(),
        execute: true,
    };
    assert!(
        Source::capture(
            tokens
                .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
                .unwrap(),
            &action,
            Some("absolute-on")
        )
        .is_none(),
        "canonical confirmation identity is not permission for a hidden recipe"
    );
    const EXACT: &str = "synthetic-component-hap-exact-0123456789";
    tokens
        .insert(
            Uuid::new_v4(),
            EXACT,
            Grant {
                controller: h.member,
                role: Role::Member,
                scenes: ["absolute-on".into()].into(),
                devices: ["execute-led".into()].into(),
            },
        )
        .unwrap();
    let source = Source::capture(
        tokens
            .module_off_lease(Some(&format!("Bearer {EXACT}")), None, None)
            .unwrap(),
        &action,
        Some("absolute-on"),
    )
    .unwrap();
    let retargeted = Action::SceneAction {
        controller: h.member,
        revision: 1,
        scene: "absolute-off".into(),
        execute: true,
    };
    let control = h.control.clone();
    assert!(
        h.drive(tokio::spawn(async move {
            control.request_authorized(retargeted, source).await
        }))
        .await
        .is_err(),
        "an exact On source cannot authorize another recipe or the canonical scene"
    );
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(mode_value(&h, "v"), None);
    h.finish().await;
}

#[tokio::test]
async fn component_hap_absolute_on_off_retains_canonical_postwrite_program_confirmation() {
    let (cfg, d) = inputs();
    let mut h = Harness::from_inputs(cfg, d).await;
    let (store, mut bridge) = install(&mut h).await;
    seed_mode(&h, "w", 4);
    mode_frame(&mut h, 3, 2, 0xf1);
    for on in [true, true, false, false, true] {
        write(&mut h, &bridge, on).await.unwrap();
        assert_eq!(
            activity(&h).await.activity,
            A::Pending,
            "acceptance alone is not a selected component"
        );
        assert_eq!(
            mode_value(&h, "w"),
            Some(homesense_core::rule::Value::Integer(4)),
            "absolute intent must preserve the independent component"
        );
        let wire = h.wire().await;
        assert!(
            wire.windows(5)
                .any(|f| f == [0xae, 3, 1, if on { 0xf1 } else { 0xf2 }, 0xee])
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        confirm_program(&h).await;
        drain_linked_led(&h).await;
        assert_eq!(
            activity(&h).await.activity,
            if on { A::Active } else { A::Inactive }
        );
        bridge = refresh(&mut h, bridge).await;
        assert_eq!(read(&bridge).await.unwrap(), json!(on));
    }
    h.allowed.store(false, Ordering::Release);
    bridge = refresh(&mut h, bridge).await;
    assert!(
        read(&bridge).await.is_err(),
        "matching light and revoked ownership cannot establish HAP On"
    );
    mode_frame(&mut h, 3, 1, 0xf2);
    bridge = refresh(&mut h, bridge).await;
    assert_eq!(
        read(&bridge).await.unwrap(),
        json!(false),
        "fresh negative feedback may still disprove activity without recreating a positive owner"
    );
    drop(bridge);
    drop(store);
    h.finish().await;
}
