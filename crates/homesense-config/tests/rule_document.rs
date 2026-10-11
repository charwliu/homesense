use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{
    ExecutionContext,
    rule::{self, Budget, Value},
};
use serde_json::{Value as Json, json};
fn config() -> Configuration {
    Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn fixture() -> Json {
    json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],
        "rules":[{"id":"startup","source":"fixture:root","trigger":{"kind":"startup_synchronized"},"actions":[
            {"source":"fixture:condition","action":{"kind":"if","condition":{"kind":"unknown","value":{"kind":"variable","id":"mode"}},"then_actions":[
                {"source":"fixture:set","action":{"kind":"set","variable":"mode","value":{"kind":"add","left":{"kind":"integer","value":2},"right":{"kind":"integer","value":3}}}}
            ],"else_actions":[{"source":"fixture:else","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":9}}}]}}
        ]}]
    })
}
fn parse(v: &Json) -> Result<Document, homesense_config::Error> {
    Document::parse(&serde_json::to_vec(v).unwrap(), &config())
}
#[test]
fn preserves_nested_source_order_and_never_enables_by_default() {
    let d = parse(&fixture()).unwrap();
    assert!(!d.rules[0].enabled);
    let p = rule::evaluate(&d.rules[0].actions, &d.variables, Budget::default()).unwrap();
    assert_eq!(p.variables["mode"].value, Some(Value::Integer(5)));
    assert_eq!(p.actions[0].source, "fixture:set");
    let p = rule::evaluate(&d.rules[0].actions, &p.variables, Budget::default()).unwrap();
    assert_eq!(p.variables["mode"].value, Some(Value::Integer(9)));
    assert_eq!(p.actions[0].source, "fixture:else");
}
#[test]
fn requires_matching_revision_review_and_strict_fields() {
    let mut v = fixture();
    v["configuration_revision"] = 2.into();
    assert!(parse(&v).is_err());
    let mut v = fixture();
    v["rules"][0]["enabled"] = true.into();
    assert!(parse(&v).is_err());
    v["rules"][0]["review_evidence"] = "offline fixture review".into();
    assert!(parse(&v).unwrap().rules[0].enabled);
    v["rules"][0]["enable"] = true.into();
    assert!(parse(&v).is_err());
    let mut v = fixture();
    let duplicate = v["rules"][0].clone();
    v["rules"].as_array_mut().unwrap().push(duplicate);
    assert!(parse(&v).is_err());
}
#[test]
fn validates_unreachable_branch_types_and_read_only_cache() {
    let mut v = fixture();
    v["rules"][0]["actions"][0]["action"]["else_actions"][0]["action"]["value"] =
        json!({"kind":"bool","value":true});
    assert!(parse(&v).is_err());
    let mut v = fixture();
    v["variables"][0]["lifetime"] = "feedback_cache".into();
    assert!(parse(&v).is_err());
    let mut v = fixture();
    v["rules"][0]["actions"][0]["action"]["condition"]["value"]["id"] = "missing".into();
    assert!(parse(&v).is_err());
}
#[test]
fn device_commands_and_trigger_kinds_cannot_cross_capabilities() {
    let mut v = fixture();
    v["rules"][0]["actions"] = json!([{"source":"fixture:light","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":40,"ramp":false}}}]);
    assert!(parse(&v).is_ok());
    v["rules"][0]["actions"][0]["action"]["command"] = json!({"kind":"panel_led","on":true});
    assert!(parse(&v).is_err());
    v["rules"][0]["actions"] = json!([]);
    v["rules"][0]["trigger"] =
        json!({"kind":"panel","device":"simulation:light","event":"pressed"});
    assert!(parse(&v).is_err());
    v["rules"][0]["trigger"] = json!({"kind":"device_changed","device":"missing"});
    assert!(parse(&v).is_err());
}

#[test]
fn links_only_enabled_rules_and_checks_scene_shutdown_and_schedule_references() {
    use homesense_core::scene_expand::Definition;
    let mut v = fixture();
    v["rules"][0]["actions"] = json!([{"source":"fixture:call","action":{"kind":"scene","id":"reading","activate":false}}]);
    let parsed = parse(&v).unwrap();
    assert!(
        parsed
            .linked_rules(&Default::default(), &Default::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(parsed.rules.len(), 1); // Disabled source is retained for review.
    v["rules"][0]["enabled"] = true.into();
    v["rules"][0]["review_evidence"] = "fixture".into();
    let parsed = parse(&v).unwrap();
    let scenes = [(
        "reading".into(),
        Definition {
            activate: vec![],
            deactivate: None,
        },
    )]
    .into();
    assert!(parsed.linked_rules(&scenes, &Default::default()).is_err());
    let scenes = [(
        "reading".into(),
        Definition {
            activate: vec![],
            deactivate: Some(vec![]),
        },
    )]
    .into();
    assert_eq!(
        parsed
            .linked_rules(&scenes, &Default::default())
            .unwrap()
            .len(),
        1
    );
    v["rules"][0]["trigger"] = json!({"kind":"schedule","schedule":"morning"});
    let parsed = parse(&v).unwrap();
    assert!(parsed.linked_rules(&scenes, &Default::default()).is_err());
    assert!(
        parsed
            .linked_rules(&scenes, &["morning".into()].into())
            .is_ok()
    );
}

#[test]
fn feedback_cache_reads_lower_to_fresh_actual_state_and_are_read_only() {
    use homesense_core::{
        DeviceId, Percent,
        rule::{Feedback, FeedbackSnapshot},
    };
    let mut v = fixture();
    v["variables"] = json!([{"id":"cached_level","kind":"integer","lifetime":"feedback_cache","feedback":{"kind":"level","device":"simulation:light"}}]);
    v["rules"][0]["enabled"] = true.into();
    v["rules"][0]["review_evidence"] = "fixture".into();
    v["rules"][0]["actions"] = json!([{"source":"cached-condition","action":{"kind":"if","condition":{"kind":"less","left":{"kind":"integer","value":30},"right":{"kind":"variable","id":"cached_level"}},"then_actions":[{"source":"light","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":50,"ramp":false}}}],"else_actions":[]}}]);
    let document = parse(&v).unwrap();
    let linked = document
        .linked_rules(&Default::default(), &Default::default())
        .unwrap();
    assert!(
        rule::variable_references(&linked[0].actions)
            .unwrap()
            .is_empty()
    );
    let device = DeviceId::new("simulation:light").unwrap();
    let states = [(
        device.clone(),
        Feedback {
            on: Some(true),
            level: Some(Percent::new(40).unwrap()),
            observed_ms: 0,
        },
    )]
    .into();
    for (now, count) in [(9, 1), (10, 0)] {
        let plan = rule::evaluate_with_feedback(
            &linked[0].actions,
            &Default::default(),
            Budget::default(),
            FeedbackSnapshot {
                devices: &states,
                now_ms: now,
                max_age_ms: 10,
            },
        )
        .unwrap();
        assert_eq!(plan.commands.len(), count);
        assert!(plan.feedback_reads.contains(&device));
        if count == 0 {
            assert_eq!(plan.unknown_branches, ["cached-condition"]);
        }
    }
    v["rules"][0]["actions"] = json!([{"source":"bad","action":{"kind":"set","variable":"cached_level","value":{"kind":"integer","value":0}}}]);
    assert!(parse(&v).is_err());
    v["rules"][0]["actions"] = json!([]);
    v["variables"][0]["kind"] = "bool".into();
    assert!(parse(&v).is_err());
}

#[test]
fn embedded_scene_calendar_example_links_and_rejects_invalid_definitions() {
    let mut v: Json =
        serde_json::from_slice(include_bytes!("../../../config/automation.simulation.json"))
            .unwrap();
    let d = parse(&v).unwrap();
    assert_eq!(d.immediate_scenes.len(), 1);
    assert_eq!(d.schedules.len(), 1);
    assert!(!d.rules[0].enabled);
    v["rules"][0]["enabled"] = true.into();
    v["rules"][0]["review_evidence"] = "test fixture".into();
    let d = parse(&v).unwrap();
    let ids = d.schedules.iter().map(|s| s.id.clone()).collect();
    let linked = d.linked_rules(&d.immediate_scenes, &ids).unwrap();
    assert_eq!(linked[0].actions.len(), 2);
    let plan = rule::evaluate(&linked[0].actions, &d.variables, Budget::default()).unwrap();
    assert_eq!(plan.commands.len(), 1);
    assert_eq!(plan.variables["mode"].value, Some(Value::Integer(1)));
    let mut bad = v.clone();
    bad["schedules"][0]["timezone"] = "UTC".into();
    assert!(parse(&bad).is_err());
    let mut bad = v.clone();
    bad["schedules"][0]["hour"] = 24.into();
    assert!(parse(&bad).is_err());
    let mut bad = v.clone();
    bad["immediate_scenes"][0]["activate"] = json!([{"source":"cycle","action":{"kind":"scene","id":"simulation-evening","activate":true}}]);
    assert!(parse(&bad).is_err());
    let mut bad = v;
    bad["immediate_scenes"][0]["delay_ms"] = 1000.into();
    assert!(parse(&bad).is_err()); // Never discard unsupported timing.
}

#[test]
fn delayed_linking_is_explicit_and_keeps_calls_and_variable_references() {
    let mut v=fixture();
    v["rules"][0]["enabled"]=true.into();v["rules"][0]["review_evidence"]="test".into();
    v["rules"][0]["actions"].as_array_mut().unwrap().push(json!({"source":"call","action":{"kind":"scene","id":"later","activate":true}}));
    v["timed_scenes"]=json!([{"id":"later","enabled":true,"review_evidence":"test","max_lateness_ms":1000,"steps":[{"source":"step","offset_ms":500,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]);
    let d=parse(&v).unwrap();
    assert!(d.linked_rules(&d.immediate_scenes,&Default::default()).is_err());
    let rules=d.linked_rules_with_deferred().unwrap();
    let refs=rule::variable_references_with_deferred(&rules[0].actions,&["later".into()].into()).unwrap();
    assert_eq!(refs,["mode".into()].into());
    let plan=rule::evaluate(&rules[0].actions,&d.variables,Budget::default()).unwrap();
    assert_eq!(plan.variables["mode"].value,Some(Value::Integer(5)));
    assert!(matches!(&plan.actions[1].action,rule::Action::Scene{id,activate:true} if id=="later"));
    assert!(plan.commands.is_empty());
    v["timed_scenes"][0]["enabled"]=false.into();
    assert!(parse(&v).unwrap().linked_rules_with_deferred().is_err());
    v["timed_scenes"][0]["enabled"]=true.into();
    v["rules"][0]["actions"][1]["action"]["activate"]=false.into();
    assert!(parse(&v).unwrap().linked_rules_with_deferred().is_err());
}

#[test]
fn variable_migration_declarations_require_review_and_persistent_typed_targets() {
    let devices=String::from_utf8(include_bytes!("../../../config/simulation.json").to_vec()).unwrap().replace("\"revision\": 1","\"revision\": 2");
    let config=Configuration::parse(devices.as_bytes(),ExecutionContext::Simulation).unwrap();
    let mut doc=json!({"schema_version":1,"configuration_revision":2,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"variable_migrations":[{"variable":"mode","source_revision":1,"source_type":"bool","conversion":"bool_to_integer","review_evidence":"fixture"}]});
    let parse=|v:&Json| Document::parse(&serde_json::to_vec(v).unwrap(),&config);
    let parsed=parse(&doc).unwrap();
    assert_eq!(parsed.published_variable_value(Some(1),"mode",Some((rule::ValueType::Bool,Some(&Value::Bool(true))))).unwrap(),Some(Value::Integer(1)));
    assert!(parsed.published_variable_value(Some(1),"mode",None).is_err());
    for (field,value) in [("review_evidence",json!("")),("source_revision",json!(2)),("source_type",json!("integer")),("variable",json!("missing"))] {
        let mut bad=doc.clone();bad["variable_migrations"][0][field]=value;assert!(parse(&bad).is_err());
    }
    let duplicate=doc["variable_migrations"][0].clone();
    doc["variable_migrations"].as_array_mut().unwrap().push(duplicate);
    assert!(parse(&doc).is_err());
    doc["variable_migrations"].as_array_mut().unwrap().pop();
    doc["variables"][0]["lifetime"]="temporary".into();
    assert!(parse(&doc).is_err());
}

#[test]
fn timed_fade_metadata_requires_bounded_timing_and_immediate_level() {
    let mut v=fixture();
    v["timed_scenes"]=json!([{"id":"fade","enabled":false,"max_lateness_ms":1000,"steps":[{
        "source":"source","offset_ms":0,"device":"simulation:light",
        "command":{"kind":"level","value":0,"ramp":false},
        "fade":{"duration_ms":1000,"interval_ms":100,"lateness_ms":100}
    }]}]);
    let document=parse(&v).unwrap();
    assert_eq!(document.timed_scenes["fade"].fades[&0].samples().unwrap(),10);
    assert_eq!(document.timed_scenes["fade"].steps.len(),1);
    for (field,value) in [("duration_ms",0),("duration_ms",60001),("interval_ms",49),("lateness_ms",5001)] {
        let mut bad=v.clone();bad["timed_scenes"][0]["steps"][0]["fade"][field]=value.into();
        assert!(parse(&bad).is_err());
    }
    let mut bad=v.clone();
    bad["timed_scenes"][0]["steps"][0]["command"]["ramp"]=true.into();
    assert!(parse(&bad).is_err());
    v["timed_scenes"][0]["steps"][0]["fade"]["unknown"]=true.into();
    assert!(parse(&v).is_err());
}

#[test]
fn ad8_fade_draft_cannot_be_enabled_but_module_ramp_remains_valid() {
    let mut devices:Json=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["modules"][0]["profile"]="ad8_din_rs485_v2_2_1".into();
    let config=Configuration::parse(&serde_json::to_vec(&devices).unwrap(),ExecutionContext::Simulation).unwrap();
    let mut document=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":false,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":0,"ramp":false},"fade":{"duration_ms":1000,"interval_ms":100,"lateness_ms":100}}]}]});
    assert!(Document::parse(&serde_json::to_vec(&document).unwrap(),&config).is_ok());
    document["timed_scenes"][0]["enabled"]=true.into();
    assert!(matches!(Document::parse(&serde_json::to_vec(&document).unwrap(),&config),Err(e) if e.to_string().contains("AD8")));
    document["timed_scenes"][0]["steps"][0].as_object_mut().unwrap().remove("fade");
    document["timed_scenes"][0]["steps"][0]["command"]=json!({"kind":"level","value":50,"ramp":true});
    assert!(Document::parse(&serde_json::to_vec(&document).unwrap(),&config).is_ok());
}

#[test]
fn scene_failure_policy_is_explicit_and_rejects_unimplemented_semantics() {
    use homesense_config::rule_document::SceneFailurePolicy;
    let mut document=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"scene","enabled":false,"max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":50,"ramp":false}}]}]});
    assert_eq!(parse(&document).unwrap().timed_scenes["scene"].failure_policy,SceneFailurePolicy::CancelRemaining);
    document["timed_scenes"][0]["failure_policy"]="cancel_remaining".into();
    assert!(parse(&document).is_ok());
    for policy in ["continue","retry","rollback","compensate",""] {
        document["timed_scenes"][0]["failure_policy"]=policy.into();
        assert!(parse(&document).is_err(),"unsupported policy {policy}");
    }
}
