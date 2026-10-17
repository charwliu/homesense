use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use serde_json::{Value, json};
fn cfg() -> Configuration {
    Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn rules() -> Value {
    json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":[{"id":"scene","enabled":true,"review_evidence":"synthetic","activate":[{"source":"synthetic","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":100,"ramp":false}}}]}],"scene_metadata":[{"id":"scene","name":"Friendly name"}],"scene_tracking":[{"scene":"scene","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic activity","max_age_ms":5000,"review_evidence":"synthetic","members":[{"id":"member","device":"simulation:light","predicate":{"kind":"level_range","minimum":20,"maximum":60},"review_evidence":"synthetic explicit powered predicate"}]}]})
}
fn parse(v: &Value) -> Result<Document, homesense_config::Error> {
    Document::parse(&serde_json::to_vec(v).unwrap(), &cfg())
}
#[test]
fn strict_policy_keeps_display_identity_and_has_no_implicit_unknown_or_pending_strategy() {
    let v = rules();
    let d = parse(&v).unwrap();
    assert_eq!(d.scene_metadata["scene"], "Friendly name");
    assert!(d.scene_tracking["scene"].enabled);
    for key in [
        "aggregation",
        "unknown_policy",
        "pending_policy",
        "control_policy",
        "activity_name",
        "enabled",
        "review_evidence",
    ] {
        let mut v = v.clone();
        v["scene_tracking"][0].as_object_mut().unwrap().remove(key);
        assert!(parse(&v).is_err(), "{key}");
    }
    let mut v = v;
    v["scene_tracking"][0]["unknown_policy"] = "guess_off".into();
    assert!(parse(&v).is_err());
}
#[test]
fn disabled_definition_and_unreviewed_missing_ignored_or_invalid_members_cannot_supply_activity() {
    let base = rules();
    for mode in 0..8 {
        let mut v = base.clone();
        match mode {
            0 => v["immediate_scenes"][0]["enabled"] = false.into(),
            1 => v["scene_tracking"][0]["max_age_ms"] = 0.into(),
            2 => v["scene_tracking"][0]["members"][0]["device"] = "missing".into(),
            3 => {
                v["scene_tracking"][0]["members"][0]["predicate"] =
                    json!({"kind":"ignore","reason":""})
            }
            4 => {
                v["scene_tracking"][0]["members"][0]["predicate"] =
                    json!({"kind":"ignore","reason":"all members cannot be ignored"})
            }
            5 => v["scene_tracking"][0]["members"][0]["predicate"]["minimum"] = 0.into(),
            6 => v["scene_tracking"][0]["members"][0]["review_evidence"] = "".into(),
            _ => v["scene_tracking"][0]["members"][0]["predicate"]["maximum"] = 101.into(),
        };
        assert!(parse(&v).is_err(), "mode {mode}");
    }
}
#[test]
fn tracking_provider_cannot_override_existing_latching_selection_or_native_toggle_provider() {
    let mut v = rules();
    v["immediate_scenes"] = json!([]);
    v["timed_scenes"] = json!([{"id":"scene","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","device":"simulation:light","offset_ms":0,"command":{"kind":"level","value":100,"ramp":false}}]},{"id":"off","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","device":"simulation:light","offset_ms":0,"command":{"kind":"level","value":0,"ramp":false}}]}]);
    let mut other = v["timed_scenes"][0].clone();
    other["id"] = "other".into();
    v["timed_scenes"].as_array_mut().unwrap().push(other);
    v["variables"] = json!([{"id":"selection","kind":"integer","lifetime":"persistent"}]);
    v["scene_switch_groups"] = json!([{"id":"group","variable":"selection","review_evidence":"synthetic existing native provider","members":[{"scene":"scene","value":1,"off_scene":"off"},{"scene":"other","value":2,"off_scene":"off"}]}]);
    let error = parse(&v).err().unwrap().to_string();
    assert!(error.contains("conflicts"), "{error}");
    v["scene_tracking"][0]["enabled"] = false.into();
    assert!(parse(&v).is_ok());
}
#[test]
fn metadata_and_tracking_are_strict_and_duplicate_identifiers_are_rejected() {
    let v = rules();
    for field in ["scene_metadata", "scene_tracking"] {
        let mut v = v.clone();
        let value = v[field][0].clone();
        v[field].as_array_mut().unwrap().push(value);
        assert!(parse(&v).is_err());
    }
    let mut v = v;
    v["scene_metadata"][0]["id"] = "missing".into();
    assert!(parse(&v).is_err());
}

#[test]
fn scene_room_metadata_is_optional_and_resolves_only_same_revision_rooms() {
    let mut configuration: Value = serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    configuration["rooms"] = json!([
        {"id":"alpha","name":"Alpha","floor":"Upper"},
        {"id":"beta","name":"Beta"}
    ]);
    let config = Configuration::parse(&serde_json::to_vec(&configuration).unwrap(), ExecutionContext::Simulation).unwrap();
    let mut input = rules();
    for room in [None, Some(Value::Null)] {
        if let Some(room) = room { input["scene_metadata"][0]["room"] = room; }
        let document = Document::parse(&serde_json::to_vec(&input).unwrap(), &config).unwrap();
        assert!(document.scene_rooms.is_empty());
        assert_eq!(document.scene_catalog()["scene"], "Friendly name");
    }
    for id in ["alpha", "beta"] {
        input["scene_metadata"][0]["room"] = json!(id);
        let document = Document::parse(&serde_json::to_vec(&input).unwrap(), &config).unwrap();
        assert_eq!(document.scene_rooms["scene"], config.rooms()[id]);
        assert_eq!(document.scene_metadata["scene"], "Friendly name");
    }
    input["immediate_scenes"][0]["enabled"] = json!(false);
    assert!(Document::parse(&serde_json::to_vec(&input).unwrap(), &config).is_err());
    input["scene_tracking"][0]["enabled"] = json!(false);
    let disabled = Document::parse(&serde_json::to_vec(&input).unwrap(), &config).unwrap();
    assert_eq!(disabled.scene_rooms["scene"], config.rooms()["beta"]);
    assert!(!disabled.scene_catalog().contains_key("scene"));
    input["immediate_scenes"][0]["enabled"] = json!(true);
    input["scene_tracking"][0]["enabled"] = json!(true);
    for invalid in [json!("absent"), json!(""), json!(7), json!({"id":"alpha"})] {
        input["scene_metadata"][0]["room"] = invalid;
        assert!(Document::parse(&serde_json::to_vec(&input).unwrap(), &config).is_err());
    }
    input["scene_metadata"][0]["room"] = json!("alpha");
    input["scene_metadata"][0]["id"] = json!("missing_scene");
    assert!(Document::parse(&serde_json::to_vec(&input).unwrap(), &config).is_err());
    input["scene_metadata"][0]["id"] = json!("scene");
    input["configuration_revision"] = json!(2);
    assert!(Document::parse(&serde_json::to_vec(&input).unwrap(), &config).is_err());
}
