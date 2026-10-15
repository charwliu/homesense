use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use serde_json::json;
fn config() -> Configuration {
    Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn input() -> serde_json::Value {
    json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"caller","enabled":true,"review_evidence":"synthetic","source":"caller","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"callsite","action":{"kind":"scene","id":"draft","activate":true}}]}],"immediate_scenes":[{"id":"draft","enabled":false,"review_evidence":"synthetic","activate":[{"source":"member","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":100,"ramp":false}}}]}]})
}
#[test]
fn enabling_caller_or_passing_external_map_cannot_bypass_disabled_definition() {
    let cfg = config();
    let v = input();
    let d = Document::parse(&serde_json::to_vec(&v).unwrap(), &cfg).unwrap();
    assert!(d.disabled_immediate_scenes.contains("draft"));
    assert!(d.linked_rules_with_deferred().is_err());
    assert!(
        d.linked_rules(&d.immediate_scenes.clone(), &Default::default())
            .is_err()
    );
}
#[test]
fn disabled_scene_result_trigger_is_also_rejected() {
    let cfg = config();
    let mut v = input();
    v["rules"][0]["actions"] = json!([]);
    v["rules"][0]["trigger"] = json!({"kind":"scene_result","scene":"draft"});
    let d = Document::parse(&serde_json::to_vec(&v).unwrap(), &cfg).unwrap();
    assert!(d.linked_rules_with_deferred().is_err());
}
#[test]
fn legacy_definitions_remain_compatible_and_explicit_disabled_syntax_still_validated() {
    let cfg = config();
    let mut v = input();
    v["immediate_scenes"][0]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    let d = Document::parse(&serde_json::to_vec(&v).unwrap(), &cfg).unwrap();
    assert!(d.disabled_immediate_scenes.is_empty());
    assert_eq!(d.linked_rules_with_deferred().unwrap()[0].actions.len(), 1);
    v["immediate_scenes"][0]["enabled"] = false.into();
    v["immediate_scenes"][0]["activate"][0]["action"]["unknown"] = true.into();
    assert!(Document::parse(&serde_json::to_vec(&v).unwrap(), &cfg).is_err());
}
