//! Real Admission/feedback tests: component activity adds no control authority.
use super::*;
use crate::scene_activity::SceneActivity as A;

fn recorded_program_inputs() -> (Vec<u8>, Json) {
    let (cfg, doc) = component_cycle_inputs();
    let mut doc = with_program(doc, "v", json!(1));
    doc["component_mode_confirmations"] = json!([]);
    doc["scene_component_activity"][0]["program_confirmation"]["recorded_selection"] = json!({
        "source_scenes":["act"], "reset_variables":["v"],
        "review_evidence":"Synthetic explicit selection; physical ownership still required"
    });
    (cfg, doc)
}

#[test]
fn recorded_program_schema_rejects_unowned_writers_resets_and_nonphysical_conditions() {
    let (cfg, doc) = recorded_program_inputs();
    let config = Configuration::parse(&cfg, ExecutionContext::Simulation).unwrap();
    assert!(Document::parse(&serde_json::to_vec(&doc).unwrap(), &config).is_ok());
    for variant in 0..7 {
        let mut bad = doc.clone();
        let p = &mut bad["scene_component_activity"][0]["program_confirmation"];
        match variant {
            0 => p["recorded_selection"]["source_scenes"] = json!(["other"]),
            1 => p["recorded_selection"]["source_scenes"] = json!(["act", "other"]),
            2 => p["recorded_selection"]["reset_variables"] = json!(["w"]),
            3 => p["recorded_selection"]["reset_variables"] = json!(["v", "missing"]),
            4 => p["active_conditions"][0]["on"] = json!(false),
            5 => p["active_conditions"][0]["device"] = json!("execute-led"),
            _ => p["recorded_selection"]["review_evidence"] = json!(""),
        }
        assert!(Document::parse(&serde_json::to_vec(&bad).unwrap(), &config).is_err(), "variant{variant}");
    }
}

#[tokio::test]
async fn recorded_program_boot_reset_prevents_reusing_a_persistent_selected_intent() {
    let (cfg, doc) = recorded_program_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h,3,1,0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity,A::Active);
    h.actor.clone().shutdown().await.unwrap();
    let controller=h.controller;
    let actor=Admission::open(h.directory.join("runtime.sqlite"),Options {
        context:ExecutionContext::Simulation,..Default::default()
    },move |id|id==controller).await.unwrap();
    actor.synchronized(1,1).await.unwrap();
    assert_eq!(mode_value(&h,"v"),None);
    assert!(actor.program_activity_views(controller,actor.task_generation().await.unwrap()).await.unwrap().is_empty());
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(h.directory).unwrap();
}

#[tokio::test]
async fn recorded_program_gateway_fault_resets_only_registered_scope_without_io() {
    let (cfg, doc) = recorded_program_inputs();
    let h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h,"v",1);
    seed_mode(&h,"w",1);
    let mut owner=crate::program_activity::Owner::default();
    owner.configure(&h.document,&h.configuration);
    let mut store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
    owner.reset_recorded_for_gateway(&mut store,&h.configuration,&h.generation,"unrelated",h.actor.monotonic_ms()).unwrap();
    assert_eq!(mode_value(&h,"v"),Some(homesense_core::rule::Value::Integer(1)));
    owner.reset_recorded_for_gateway(&mut store,&h.configuration,&h.generation,"bus",h.actor.monotonic_ms()).unwrap();
    assert_eq!(mode_value(&h,"v"),None);
    assert_eq!(mode_value(&h,"w"),Some(homesense_core::rule::Value::Integer(1)));
    assert_eq!(h.count("command_journal"),0);
    drop(store);
    h.finish().await;
}

#[tokio::test]
async fn recorded_program_saved_intent_matching_shape_cannot_promote_but_negative_is_read_only() {
    let (cfg, doc) = recorded_program_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h, "v", 1);
    mode_frame(&mut h, 3, 1, 0xf1);
    assert_eq!(activity(&h).await.activity, A::Unknown, "saved label and shape are not owned execution");
    mode_frame(&mut h, 3, 1, 0xf2);
    assert_eq!(activity(&h).await.activity, A::Inactive);
    assert_eq!(mode_value(&h, "v"), Some(homesense_core::rule::Value::Integer(1)));
    assert_eq!(h.count("command_journal"), 0);
    h.finish().await;
}

#[tokio::test]
async fn recorded_program_needs_postwrite_confirmation_and_preserves_unrelated_manual_outputs() {
    let (cfg, doc) = recorded_program_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    execute(&mut h).await;
    assert_eq!(activity(&h).await.activity, A::Pending);
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Pending);
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    mode_frame(&mut h, 3, 2, 0xf1);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active, "unrelated component cannot revoke the owned primary");
    mode_frame(&mut h, 3, 1, 0xf2);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Inactive);
    assert_eq!(mode_value(&h, "v"), Some(homesense_core::rule::Value::Integer(1)), "negative display is not a control mutation");
    h.finish().await;
}

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

fn with_program(document: Json, variable: &str, value: Json) -> Json {
    let mut document = with_activity(document, variable, value.clone());
    let policies = document["component_mode_confirmations"].as_array().unwrap();
    let candidates = policies
        .iter()
        .filter(|p| {
            p["variable"] == variable
                && p["first_value"] == value
                && p.get("basis").is_none_or(|b| b == "physical")
                && p["scenes"].as_array().unwrap().contains(&json!("act"))
        })
        .collect::<Vec<_>>();
    let mut conditions = candidates[0]["postconditions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["on"] == true)
        .cloned()
        .collect::<Vec<_>>();
    conditions.retain(|c| {
        candidates
            .iter()
            .all(|p| p["postconditions"].as_array().unwrap().contains(c))
    });
    document["scene_component_activity"][0]["program_confirmation"] = json!({
        "active_conditions":conditions,"known_values":if value.is_boolean(){json!([false,true])}else{json!([0,1,4])},
        "timeout_ms":15000,"max_age_ms":5000,"review_evidence":"Synthetic complete reviewed common component; exact source execution required"
    });
    document
}

async fn confirm_program(h: &Harness) {
    crate::program_activity::Consumer::default()
        .tick(&h.actor, h.controller, &h.document, &h.feedback)
        .await
        .unwrap();
}

fn independent_program_inputs(metadata_only: bool) -> (Vec<u8>, Json) {
    let (cfg, mut doc) = logical_mode_inputs(true);
    let mut actions = vec![
        json!({"source":"synthetic/other-source","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":5}}}),
        json!({"source":"synthetic/keep-tag","action":{"kind":"set","variable":"tag","value":{"kind":"bool","value":true}}}),
    ];
    if !metadata_only {
        actions.push(command(
            "synthetic/other-component",
            "marker",
            json!({"kind":"switch","on":true}),
        ));
    }
    doc["immediate_scenes"][1]["activate"] = json!(actions);
    doc["component_mode_confirmations"].as_array_mut().unwrap().push(json!({"id":"other-canonical-tag","variable":"tag","scenes":["other"],"first_value":true,"basis":"logical_source","confirm_when_known":true,"when_values":{"v":5},"timeout_ms":15000,"max_age_ms":5000,"review_evidence":"Synthetic independent output retains owned component","postconditions":[]}));
    (cfg, with_program(doc, "tag", json!(true)))
}

#[tokio::test]
async fn known_program_activity_independent_component_preserves_owned_scope_and_real_old_written_stamp()
 {
    let (cfg, doc) = independent_program_inputs(false);
    let mut h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_frame(&mut h, 3, 2, 0xf2);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    // Model the existing observer's terminal acknowledgement. updated_ms is
    // an observation time; inheritance must retain the genuine write stamp.
    let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let id=sql.query_row("SELECT command_id FROM command_journal WHERE device_id='relay' ORDER BY command_id DESC LIMIT 1",[],|r|r.get::<_,i64>(0)).unwrap();
    drop(sql);
    Store::open(h.directory.join("runtime.sqlite"))
        .unwrap()
        .transition_command(
            h.actor.boot_id(),
            u64::try_from(id).unwrap(),
            homesense_storage::journal::Status::Written,
            homesense_storage::journal::Status::Observed,
            h.actor.monotonic_ms(),
        )
        .unwrap();
    h.request(Action::SceneAction {
        controller: h.controller,
        revision: 1,
        scene: "other".into(),
        execute: true,
    })
    .await
    .unwrap();
    assert_eq!(h.wire().await, vec![0xae, 3, 2, 0xf1, 0xee]);
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 2, 0xf1);
    confirm_program(&h).await;
    assert_eq!(
        activity(&h).await.activity,
        A::Active,
        "unchanged scope uses its owned prior fact, without querying or rewriting that relay"
    );
    assert_eq!(h.count("command_journal"), 3);
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_unrelated_success_cannot_promote_unproven_old_mode_even_with_matching_shape()
 {
    let (cfg, doc) = independent_program_inputs(false);
    let mut h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h, "v", 4);
    seed_mode(&h, "tag", 1);
    mode_frame(&mut h, 3, 1, 0xf1);
    h.request(Action::SceneAction {
        controller: h.controller,
        revision: 1,
        scene: "other".into(),
        execute: true,
    })
    .await
    .unwrap();
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 2, 0xf1);
    confirm_program(&h).await;
    assert_eq!(
        mode_value(&h, "tag"),
        Some(homesense_core::rule::Value::Bool(true))
    );
    assert_eq!(
        activity(&h).await.activity,
        A::Unknown,
        "an unrelated completed output is not proof of a stale/failed component selection"
    );
    assert_eq!(h.count("command_journal"), 1);
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_metadata_noop_can_transfer_only_a_live_owned_unchanged_component() {
    let (cfg, doc) = independent_program_inputs(true);
    let mut h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_frame(&mut h, 3, 2, 0xf2);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    h.request(Action::SceneAction {
        controller: h.controller,
        revision: 1,
        scene: "other".into(),
        execute: true,
    })
    .await
    .unwrap();
    assert_eq!(activity(&h).await.activity, A::Pending);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    assert_eq!(h.count("command_journal"), 2);
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_changed_marker_version_cannot_reuse_a_prepared_confirmation() {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    let view = h
        .actor
        .program_activity_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let proof = crate::program_activity::Proof::prepare(
        &view,
        &h.configuration,
        &h.feedback,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    seed_mode(&h, "v", 1);
    assert!(
        !h.actor
            .program_activity_proof(h.controller, proof)
            .await
            .unwrap()
    );
    assert_eq!(activity(&h).await.activity, A::Unknown);
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1))
    );
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_revoked_request_source_retires_fact_without_rewriting_mode_or_recreating_it_from_feedback()
 {
    use crate::{
        source_auth::Source,
        web_auth::{Grant, Role, Tokens},
    };
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    let tokens = Arc::new(Tokens::default());
    let credential = Uuid::new_v4();
    const SECRET: &str = "synthetic-program-fact-scope-0123456789";
    tokens
        .insert(
            credential,
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
        scene: "act".into(),
        execute: true,
    };
    let source = Source::capture(
        tokens
            .module_off_lease(Some(&format!("Bearer {SECRET}")), None, None)
            .unwrap(),
        &action,
        Some("act"),
    )
    .unwrap();
    let control = h.control.clone();
    h.drive(tokio::spawn(async move {
        control.request_authorized(action, source).await
    }))
    .await
    .unwrap();
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    let view = h
        .actor
        .program_activity_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let stale = crate::program_activity::Proof::prepare(
        &view,
        &h.configuration,
        &h.feedback,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    tokens.revoke(credential).unwrap();
    assert!(
        !h.actor
            .program_activity_proof(h.controller, stale)
            .await
            .unwrap()
    );
    assert_eq!(activity(&h).await.activity, A::Unknown);
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1))
    );
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert_eq!(activity(&h).await.activity, A::Unknown);
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_uncertain_write_and_late_matching_frame_never_confirm_a_fact() {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    h.actor
        .finish_write(ticket.command.id, false)
        .await
        .unwrap();
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert!(
        h.actor
            .program_activity_views(h.controller, h.generation.clone())
            .await
            .is_err()
    );
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1)),
        "read-only failure cannot rewrite the eager original program value"
    );
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_restart_drops_receipts_and_owned_mode_even_if_persistent_value_and_physical_shape_match()
 {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    let view = h
        .actor
        .program_activity_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let stale = crate::program_activity::Proof::prepare(
        &view,
        &h.configuration,
        &h.feedback,
        h.actor.monotonic_ms(),
    )
    .unwrap();
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
    assert!(
        actor
            .program_activity_views(controller, generation.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        actor
            .program_activity_proof(controller, stale)
            .await
            .is_err()
    );
    assert!(
        actor
            .program_activity(controller, generation, "act".into(), None)
            .await
            .unwrap()
            .is_none()
    );
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(h.directory).unwrap();
}

async fn drain_linked_led(h: &Harness) {
    if !h
        .actor
        .control_batch_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .is_empty()
    {
        crate::control_batch::tick(&h.actor, h.controller, &h.feedback)
            .await
            .unwrap();
        h.wire().await;
    }
}

#[test]
fn program_activity_schema_rejects_weakened_component_and_unbounded_or_untyped_outcomes() {
    let (cfg, doc) = component_inputs();
    let config = Configuration::parse(&cfg, ExecutionContext::Simulation).unwrap();
    let doc = with_program(doc, "v", json!(1));
    assert!(Document::parse(&serde_json::to_vec(&doc).unwrap(), &config).is_ok());
    for variant in 0..8 {
        let mut d = doc.clone();
        let p = &mut d["scene_component_activity"][0]["program_confirmation"];
        match variant {
            0 => p["active_conditions"] = json!([]),
            1 => p["active_conditions"][0]["on"] = json!(false),
            2 => p["active_conditions"][0]["device"] = json!("marker"),
            3 => p["known_values"] = json!([false, true]),
            4 => p["known_values"] = json!([0]),
            5 => p["timeout_ms"] = json!(15001),
            6 => p["max_age_ms"] = json!(u64::MAX),
            _ => p["review_evidence"] = json!(""),
        }
        assert!(
            Document::parse(&serde_json::to_vec(&d).unwrap(), &config).is_err(),
            "variant {variant}"
        );
    }
}

#[tokio::test]
async fn known_program_activity_direct_source_off_and_reopen_keep_original_eager_set_and_wait_for_actual_write()
 {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_confirm(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    for on in [false, true, false, true] {
        execute(&mut h).await;
        assert_eq!(
            mode_value(&h, "v"),
            Some(homesense_core::rule::Value::Integer(i64::from(on))),
            "known Set must still commit at original admission"
        );
        assert_eq!(activity(&h).await.activity, A::Pending);
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        confirm_program(&h).await;
        assert_eq!(
            activity(&h).await.activity,
            A::Pending,
            "a prewrite matching frame cannot confirm"
        );
        assert_eq!(
            h.wire().await,
            vec![0xae, 3, 1, if on { 0xf1 } else { 0xf2 }, 0xee]
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        assert_eq!(
            activity(&h).await.activity,
            A::Pending,
            "GET does not confirm an operation"
        );
        confirm_program(&h).await;
        let row = activity(&h).await;
        assert_eq!(row.activity, if on { A::Active } else { A::Inactive });
        assert_eq!(row.activity_source, "confirmed_program");
        assert!(!row.activity_writable);
        drain_linked_led(&h).await;
    }
    assert_eq!(
        h.count("command_journal"),
        10,
        "only original five light writes and five LEDs"
    );
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_canonical_boolean_uses_fresh_full_cohort_and_never_changes_source_values()
 {
    let (cfg, doc) = logical_mode_inputs(true);
    let mut h = Harness::from_inputs(cfg, with_program(doc, "tag", json!(true))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    mode_frame(&mut h, 3, 2, 0xf1);
    mode_confirm(&h).await;
    assert_eq!(activity(&h).await.activity, A::Inactive);
    for on in [false, true] {
        execute(&mut h).await;
        let source = mode_value(&h, "v");
        let tag = mode_value(&h, "tag");
        assert_eq!(tag, Some(homesense_core::rule::Value::Bool(on)));
        assert_eq!(activity(&h).await.activity, A::Pending);
        h.wire().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        mode_frame(&mut h, 3, 1, if on { 0xf1 } else { 0xf2 });
        confirm_program(&h).await;
        assert_eq!(
            activity(&h).await.activity,
            A::Pending,
            "own relay matches but the cohort peer has no postwrite feedback"
        );
        mode_frame(&mut h, 3, 2, 0xf2);
        confirm_program(&h).await;
        assert_eq!(
            activity(&h).await.activity,
            if on { A::Active } else { A::Inactive }
        );
        assert_eq!(mode_value(&h, "v"), source);
        assert_eq!(mode_value(&h, "tag"), tag);
        drain_linked_led(&h).await;
    }
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_changed_physical_signature_retires_without_rewriting_marker_or_resurrecting_from_shape()
 {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity, A::Active);
    let view = h
        .actor
        .program_activity_views(h.controller, h.generation.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let stale = crate::program_activity::Proof::prepare(
        &view,
        &h.configuration,
        &h.feedback,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf2);
    assert!(
        !h.actor
            .program_activity_proof(h.controller, stale)
            .await
            .unwrap(),
        "old lease cannot refresh fact"
    );
    assert_eq!(activity(&h).await.activity, A::Unknown);
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1))
    );
    confirm_program(&h).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert_eq!(
        activity(&h).await.activity,
        A::Unknown,
        "matching the old shape cannot recreate a retired program lineage"
    );
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_deadline_and_late_feedback_do_not_cancel_lighting_or_rewrite_the_source()
 {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut doc = with_program(doc, "v", json!(1));
    doc["scene_component_activity"][0]["program_confirmation"]["timeout_ms"] = json!(1);
    let mut h = Harness::from_inputs(cfg, doc).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        h.wire().await,
        vec![0xae, 3, 1, 0xf1, 0xee],
        "read-only timeout does not cancel the original lighting write"
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    drain_linked_led(&h).await;
    assert_eq!(activity(&h).await.activity, A::Unknown);
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1))
    );
    h.finish().await;
}

#[tokio::test]
async fn known_program_activity_read_marker_and_current_shape_without_real_outputs_cannot_establish_fact()
 {
    let (cfg, mut doc) = component_inputs();
    doc["immediate_scenes"][0]["activate"] = json!([{"source":"synthetic/metadata-only","action":{"kind":"set","variable":"v","value":{"kind":"integer","value":1}}}]);
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert_eq!(
        mode_value(&h, "v"),
        Some(homesense_core::rule::Value::Integer(1))
    );
    assert_eq!(h.count("command_journal"), 0);
    assert_eq!(activity(&h).await.activity, A::Unknown);
    h.finish().await;
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

#[tokio::test]
async fn known_program_activity_feedback_budget_is_not_the_already_completed_send_ttl() {
    let (cfg, mut doc) = component_cycle_inputs();
    doc["component_mode_confirmations"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != "known-off-only");
    let mut h = Harness::from_inputs(cfg, with_program(doc, "v", json!(1))).await;
    seed_mode(&h, "v", 0);
    execute(&mut h).await;
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    let sent_by = ticket.command.deadline_ms;
    h.actor.finish_write(ticket.command.id, true).await.unwrap();
    let wait_ms = sent_by
        .saturating_sub(h.actor.monotonic_ms())
        .saturating_add(20);
    assert!(
        wait_ms < 4000,
        "synthetic ordinary send budget must remain short"
    );
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    confirm_program(&h).await;
    assert_eq!(
        activity(&h).await.activity,
        A::Active,
        "actual write completed before its TTL; feedback still fits the original total budget"
    );
    drain_linked_led(&h).await;
    h.finish().await;
}

#[tokio::test]
async fn catalog_shared_component_views_recheck_actual_feedback_without_creating_io() {
    let (cfg, document) = component_cycle_inputs();
    let mut h = Harness::from_inputs(cfg, with_activity(document, "v", json!(1))).await;
    execute(&mut h).await;
    h.wire().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf1);
    assert_eq!(mode_confirm(&h).await, 1);
    let writes = h.count("command_journal");
    let config = h.configuration.clone();
    let mut context = crate::scene_tracking::CatalogReadContext::new(&config);
    for _ in 0..2 {
        let value = crate::scene_tracking::read_catalog(&h.document, "act", &h.actor,
            h.controller, &h.generation, &h.feedback, &mut context).await.unwrap().unwrap();
        assert_eq!(value.activity, A::Active);
    }
    tokio::time::sleep(Duration::from_millis(2)).await;
    mode_frame(&mut h, 3, 1, 0xf2);
    let value = crate::scene_tracking::read_catalog(&h.document, "act", &h.actor,
        h.controller, &h.generation, &h.feedback, &mut context).await.unwrap().unwrap();
    assert_eq!(value.activity, A::Unknown, "a reused catalog view is never cached actual activity");
    assert_eq!(h.actor.component_activity(h.controller, h.generation.clone(),
        "not-configured".into(), None).await, Err(hap::server::WriteStatus::InvalidValueInRequest));
    let mut other_generation = h.generation.clone();
    other_generation.epochs.configuration += 1;
    assert_eq!(h.actor.component_activity(h.controller, other_generation,
        "act".into(), None).await, Err(hap::server::WriteStatus::ResourceBusy));
    assert_eq!(h.count("command_journal"), writes, "catalog reads never enqueue queries or controls");
    drop(context);
    h.finish().await;
}
