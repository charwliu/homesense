use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use serde_json::{Value, json};

fn configuration() -> Configuration {
    Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn rules() -> Value {
    let scene = |id, level| {
        json!({"id":id,"enabled":true,"review_evidence":"synthetic catalog regression","max_lateness_ms":1000,
        "steps":[{"source":"synthetic/light","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":level,"ramp":false}}]})
    };
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],
        "rules":[],"timed_scenes":[scene("reading",100),scene("off",0),scene("unrelated",0)],
        "scene_metadata":[{"id":"reading","name":"阅读"},{"id":"off","name":"阅读关闭","catalog_hidden":true},{"id":"unrelated","name":"阅读关闭"}],
        "scene_switch_groups":[{"id":"room","variable":"selection","review_evidence":"synthetic explicit pair",
            "members":[{"scene":"reading","value":1,"off_scene":"off"}],"reset_scenes":["off"]}]})
}
#[test]
fn internal_shutdown_keeps_execution_and_pairing_but_has_no_second_catalog_entry() {
    let config = configuration();
    let document = Document::parse(&serde_json::to_vec(&rules()).unwrap(), &config).unwrap();
    assert_eq!(
        document.scene_catalog(),
        [
            ("reading".into(), "阅读".into()),
            ("unrelated".into(), "阅读关闭".into())
        ]
        .into()
    );
    assert_eq!(document.scene_switch_groups[0].members[0].off_scene, "off");
    assert!(
        document.scene_action_available("off"),
        "internal recipe must remain executable by its paired scene"
    );
    assert!(document.linked_manual_scenes(&["off".into()]).is_ok());
}
#[test]
fn existing_catalogs_and_equal_names_are_not_implicitly_collapsed() {
    let mut input = rules();
    input["scene_metadata"][1]
        .as_object_mut()
        .unwrap()
        .remove("catalog_hidden");
    let document = Document::parse(&serde_json::to_vec(&input).unwrap(), &configuration()).unwrap();
    assert_eq!(document.scene_catalog().len(), 3);
    input["scene_metadata"][1]["catalog_hidden"] = json!("true");
    assert!(Document::parse(&serde_json::to_vec(&input).unwrap(), &configuration()).is_err());
}
