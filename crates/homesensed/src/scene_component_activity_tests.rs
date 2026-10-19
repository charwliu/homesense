//! Real Admission/feedback tests: component activity adds no control authority.
use super::*;
use crate::scene_activity::SceneActivity as A;

fn with_activity(mut document: Json, variable: &str, value: Json) -> Json {
    document["scene_component_activity"] = json!([{
        "scene":"act", "enabled":true, "variable":variable, "active_value":value,
        "activity_name":"Synthetic owned activity", "review_evidence":"Synthetic typed physical owner"
    }]);
    document
}
async fn activity(h: &Harness) -> crate::scene_tracking::Projection {
    crate::scene_tracking::read(
        &h.document,
        "act",
        &h.actor,
        h.controller,
        &h.generation,
        &h.feedback,
    )
    .await
    .unwrap()
    .unwrap()
}
async fn execute(h: &mut Harness) {
    h.request(Action::SceneAction {
        controller: h.controller,
        revision: 1,
        scene: "act".into(),
        execute: true,
    })
    .await
    .unwrap();
}

#[test]
fn component_activity_schema_requires_reviewed_typed_physical_provider() {
    let (cfg, document) = component_cycle_inputs();
    let c = Configuration::parse(&cfg, ExecutionContext::Simulation).unwrap();
    let document = with_activity(document, "v", json!(1));
    assert!(Document::parse(&serde_json::to_vec(&document).unwrap(), &c).is_ok());
    for variant in 0..8 {
        let mut d = document.clone();
        match variant {
            0 => d["scene_component_activity"][0]["active_value"] = json!(true),
            1 => d["scene_component_activity"][0]["variable"] = json!("missing"),
            2 => d["scene_component_activity"][0]["active_value"] = json!(99),
            3 => d["scene_component_activity"][0]["review_evidence"] = json!(""),
            4 => d["scene_component_activity"][0]["activity_name"] = json!("bad\nname"),
            5 => {
                let p = d["scene_component_activity"][0].clone();
                d["scene_component_activity"]
                    .as_array_mut()
                    .unwrap()
                    .push(p);
            }
            6 => d["scene_component_activity"][0]["scene"] = json!("absent"),
            _ => d["scene_component_activity"][0]["writable"] = json!(true),
        }
        assert!(
            Document::parse(&serde_json::to_vec(&d).unwrap(), &c).is_err(),
            "variant {variant}"
        );
    }
    let (_, logical) = logical_mode_inputs(false);
    assert!(
        Document::parse(
            &serde_json::to_vec(&with_activity(logical, "tag", json!(true))).unwrap(),
            &c
        )
        .is_err(),
        "logical source labels cannot register as physical providers"
    );
}

#[tokio::test]
async fn component_activity_requires_written_postwrite_confirmation_and_does_not_query() {
    let (cfg, document) = component_cycle_inputs();
    let mut h = Harness::from_inputs(cfg, with_activity(document, "v", json!(1))).await;
    assert_eq!(activity(&h).await.activity, A::Unknown);
    execute(&mut h).await;
    assert_eq!(activity(&h).await.activity, A::Pending);
    mode_frame(&mut h, 3, 1, 0xf1);
    assert_eq!(
        activity(&h).await.activity,
        A::Pending,
        "prewrite matching feedback cannot confirm"
    );
    assert_eq!(h.wire().await, vec![0xae, 3, 1, 0xf1, 0xee]);
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    assert_eq!(
        activity(&h).await.activity,
        A::Pending,
        "reading a card never commits the pending inference"
    );
    assert_eq!(mode_confirm(&h).await, 1);
    let projected = activity(&h).await;
    assert_eq!(projected.activity, A::Active);
    assert_eq!(projected.activity_source, "confirmed_component");
    assert!(projected.valid_until_ms.is_some());
    assert!(!projected.activity_writable);
    assert!(projected.action_available);
    let Reply::SceneCatalog(catalog) = h
        .request(Action::SceneCatalog {
            controller: h.controller,
            revision: 1,
            scenes: vec!["act".into()],
        })
        .await
        .unwrap()
    else {
        panic!("expected real scene catalog");
    };
    assert_eq!(catalog["act"].execution_kind, SceneExecutionKind::Immediate);
    assert_eq!(catalog["act"].activity, A::Active);
    assert_eq!(
        catalog["act"].tracking.as_ref().unwrap().activity_source,
        "confirmed_component"
    );
    let writes = h.count("command_journal");
    for _ in 0..5 {
        assert_eq!(activity(&h).await.activity, A::Active);
    }
    assert_eq!(
        h.count("command_journal"),
        writes,
        "activity reads do not create queries or writes"
    );
    execute(&mut h).await;
    assert_eq!(activity(&h).await.activity, A::Pending);
    assert_eq!(h.wire().await, vec![0xae, 3, 1, 0xf2, 0xee]);
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf2);
    assert_eq!(mode_confirm(&h).await, 1);
    assert_eq!(activity(&h).await.activity, A::Inactive);
    assert_eq!(
        h.count("command_journal"),
        4,
        "two original lights and two original LEDs only"
    );
    h.finish().await;
}

#[tokio::test]
async fn component_activity_current_contradiction_is_unknown_without_mutating_the_source() {
    let (cfg, document) = component_cycle_inputs();
    let mut h = Harness::from_inputs(cfg, with_activity(document, "v", json!(1))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    assert_eq!(mode_confirm(&h).await, 1);
    assert_eq!(activity(&h).await.activity, A::Active);
    let view = h
        .actor
        .component_mode_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let stale = crate::component_modes::Proof::prepare(
        &view,
        &h.configuration,
        &h.feedback,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf2);
    assert_eq!(
        h.actor
            .component_activity(
                h.controller,
                h.generation.clone(),
                "act".into(),
                Some(stale)
            )
            .await
            .unwrap()
            .0,
        A::Unknown,
        "a stale lease does not publish Active or make the whole scene catalog unavailable"
    );
    assert_eq!(
        activity(&h).await.activity,
        A::Unknown,
        "latest actual Off defeats the retained On vector before the consumer ticks"
    );
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1)),
        "read-only projection does not rewrite variables"
    );
    assert_eq!(h.count("command_journal"), 2);
    h.finish().await;
}

#[tokio::test]
async fn component_activity_boolean_owner_does_not_upgrade_logical_canonicalization() {
    let (cfg, document) = logical_mode_inputs(true);
    let mut h = Harness::from_inputs(cfg, with_activity(document, "tag", json!(true))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_frame(&mut h, 3, 2, 0xf1);
    assert_eq!(mode_confirm(&h).await, 1);
    assert_eq!(
        activity(&h).await.activity,
        A::Inactive,
        "physically owned false is an actual inactive result"
    );
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf2);
    mode_frame(&mut h, 3, 2, 0xf2);
    assert_eq!(
        mode_value(&h, "tag"),
        Some(homesense_core::rule::Value::Bool(false))
    );
    assert_eq!(
        activity(&h).await.activity,
        A::Unknown,
        "eager logical-only canonicalization is not a retained physical owner"
    );
    h.finish().await;
}

#[tokio::test]
async fn component_activity_stale_version_and_restart_cannot_reuse_current_light_shape() {
    let (cfg, document) = component_cycle_inputs();
    let mut h = Harness::from_inputs(cfg, with_activity(document, "v", json!(1))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_confirm(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    seed_mode(&h, "v", 1);
    assert_eq!(
        activity(&h).await.activity,
        A::Unknown,
        "same value with a new version is a different owner"
    );
    h.actor.clone().shutdown().await.unwrap();
    let controller = h.controller;
    let actor = Admission::open(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let generation = actor.task_generation().await.unwrap();
    assert_eq!(
        actor
            .component_activity(controller, generation, "act".into(), None)
            .await
            .unwrap()
            .0,
        A::Unknown
    );
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(h.directory).unwrap();
}
