use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use serde_json::{Value, json};
fn config() -> Configuration {
    let mut v: Value = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/devices.json"
    ))
    .unwrap();
    for panel in v["keypads"].as_array_mut().unwrap() {
        for key in panel["keys"].as_array_mut().unwrap() {
            key.as_object_mut().unwrap().remove("action");
        }
    }
    Configuration::parse(
        &serde_json::to_vec(&v).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn rules() -> Value {
    let command = |level| json!({"source":"synthetic/light","action":{"kind":"command","device":"fixture-light","command":{"kind":"level","value":level,"ramp":false}}});
    let set = |value| json!({"source":"synthetic/selection","action":{"kind":"set","variable":"selection","value":{"kind":"integer","value":value}}});
    let timed = |id, level| json!({"id":id,"enabled":true,"review_evidence":"synthetic profile","max_lateness_ms":1000,"steps":[{"source":"synthetic/profile","offset_ms":0,"device":"fixture-light","command":{"kind":"level","value":level,"ramp":false}}]});
    let policy = |value, on| json!({"id":format!("confirm-{value}"),"variable":"selection","scenes":["action"],"first_value":value,"confirm_when_known":true,"shared_selection_group":"group","when_values":{"selection":value},"max_age_ms":1000,"timeout_ms":2000,"review_evidence":"synthetic source confirmation","postconditions":[{"device":"fixture-light","on":on}]});
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"immediate_scenes":[{"id":"action","enabled":true,"review_evidence":"synthetic explicit On and Off","activate":[command(100),set(1)],"deactivate":[command(0),set(0)]}],"timed_scenes":[timed("profile",100),timed("off",0)],"scene_switch_groups":[{"id":"group","variable":"selection","review_evidence":"synthetic group","members":[{"scene":"profile","value":1,"off_scene":"off"}]}],"component_mode_confirmations":[policy(1,true),policy(0,false)],"scene_metadata":[{"id":"action","name":"阅读","room":"study"},{"id":"profile","name":"阅读","room":"study"}],"scene_presentations":[{"scene":"action","selection_scene":"profile","review_evidence":"explicit source action and its confirmed member"}]})
}
fn parse(v: &Value) -> Result<Document, homesense_config::Error> {
    Document::parse(&serde_json::to_vec(v).unwrap(), &config())
}
#[test]
fn one_public_action_keeps_its_profile_internal_without_name_based_deduplication() {
    let d = parse(&rules()).unwrap();
    let catalog = d.scene_catalog();
    assert_eq!(catalog.get("action").map(String::as_str), Some("阅读"));
    assert!(
        !catalog.contains_key("profile"),
        "the selection profile is internal, not a second room action"
    );
    assert!(catalog.contains_key("off"));
    let mut ordinary = rules();
    ordinary["scene_presentations"] = json!([]);
    assert!(
        parse(&ordinary)
            .unwrap()
            .scene_catalog()
            .contains_key("profile"),
        "equal names alone never hide a scene"
    );
}
#[test]
fn presentation_requires_unique_enabled_source_exact_scope_same_room_and_confirmed_outcomes() {
    for case in 0..12 {
        let mut v = rules();
        match case {
            0=>v["scene_presentations"][0]["selection_scene"]=json!("missing"),
            1=>v["scene_presentations"][0]["scene"]=json!("profile"),
            2=>v["scene_presentations"][0]["review_evidence"]=json!(""),
            3=>{let duplicate=v["scene_presentations"][0].clone();v["scene_presentations"].as_array_mut().unwrap().push(duplicate);},
            4=>v["immediate_scenes"][0]["enabled"]=json!(false),
            5=>v["immediate_scenes"][0].as_object_mut().unwrap().remove("deactivate").map(|_|()).unwrap(),
            6=>v["immediate_scenes"][0]["activate"][0]["action"]["device"]=json!("fixture-downlight"),
            7=>v["component_mode_confirmations"]=json!([]),
            8=>v["component_mode_confirmations"].as_array_mut().unwrap().truncate(1),
            9=>v["scene_metadata"][0]["room"]=json!(null),
            10=>v["scene_presentations"][0]["unknown_option"]=json!(true),
            _=>v["immediate_scenes"][0]["deactivate"].as_array_mut().unwrap().push(json!({"source":"extra/off/LED","action":{"kind":"command","device":"legacy-key-1","command":{"kind":"panel_led","on":false}}})),
        }
        assert!(parse(&v).is_err(), "case{case}");
    }
}

#[test]
fn public_action_and_internal_profile_share_household_or_unassigned_scope() {
    let mut v=rules();
    for row in v["scene_metadata"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("room");
        row["scope"]=json!("household");
    }
    assert!(parse(&v).is_ok());
    v["scene_metadata"][1]["scope"]=json!("unassigned");
    assert!(parse(&v).is_err(),"public action and profile must move together");
    v["scene_metadata"][0]["scope"]=json!("unassigned");
    assert!(parse(&v).is_ok());
}
