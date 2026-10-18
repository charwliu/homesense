use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{DeviceId, ExecutionContext};
use serde_json::{Value, json};

fn configuration() -> Configuration {
    Configuration::parse(&serde_json::to_vec(&json!({
        "schema_version": 1, "revision": 1,
        "gateways": [{"id":"bus", "endpoint":"127.0.0.1:50007", "modules":[
            {"address":"11", "profile":"ad8_din_rs485_v2_2_1", "verification":{"status":"simulated"}}
        ]}],
        "devices":[
            {"id":"courtyard-light", "name":"Courtyard", "gateway":"bus", "address":"11", "channel":1, "enabled":true},
            {"id":"passage-light", "name":"Passage", "gateway":"bus", "address":"11", "channel":2, "enabled":true}
        ]
    })).unwrap(), ExecutionContext::Simulation).unwrap()
}
fn fixture() -> Value {
    let mut scenes = vec![];
    let mut groups = vec![];
    let mut variables = vec![];
    for (name, light) in [
        ("courtyard", "courtyard-light"),
        ("coming-home", "passage-light"),
    ] {
        let off = format!("{name}-off");
        let variable = format!("{name}-selection");
        variables.push(json!({"id":variable,"kind":"integer","lifetime":"persistent"}));
        for (id, level) in [(name.to_owned(), 100), (off.clone(), 0)] {
            scenes.push(json!({"id":id,"enabled":true,"review_evidence":"independent manual scene fixture",
                "max_lateness_ms":5000,"steps":[{"source":format!("fixture/{id}"),"device":light,"offset_ms":0,
                    "command":{"kind":"level","value":level,"ramp":false}}]}));
        }
        groups.push(json!({"id":name,"variable":variable,"review_evidence":"one independently toggled scene",
            "members":[{"scene":name,"value":1,"off_scene":off}],"reset_scenes":[off],
            "reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,
                "review_evidence":"one light power pattern in this independent scope"}}));
    }
    json!({"schema_version":1,"configuration_revision":1,"variables":variables,"rules":[],
        "timed_scenes":scenes,"scene_switch_groups":groups})
}
fn parse(value: &Value) -> Result<Document, homesense_config::Error> {
    Document::parse(&serde_json::to_vec(value).unwrap(), &configuration())
}
#[test]
fn independent_single_member_groups_keep_disjoint_targets_and_unknown_initial_selection() {
    let document = parse(&fixture()).unwrap();
    assert_eq!(document.scene_switch_groups.len(), 2);
    assert!(
        document
            .variables
            .values()
            .all(|variable| variable.value.is_none())
    );
    for (group, light) in document
        .scene_switch_groups
        .iter()
        .zip(["courtyard-light", "passage-light"])
    {
        assert_eq!(group.members.len(), 1);
        let targets = group
            .absolute_light_targets(&document.timed_scenes)
            .unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[&1].keys().cloned().collect::<Vec<_>>(),
            vec![DeviceId::new(light).unwrap()]
        );
        assert_eq!(
            group.light_on_patterns(&document.timed_scenes).unwrap()[&1].len(),
            1
        );
    }
}
#[test]
fn singleton_support_still_rejects_empty_groups_and_shared_selection_variables() {
    let mut value = fixture();
    value["scene_switch_groups"][0]["members"] = json!([]);
    assert!(parse(&value).is_err());
    let mut value = fixture();
    value["scene_switch_groups"][1]["variable"] =
        value["scene_switch_groups"][0]["variable"].clone();
    assert!(parse(&value).is_err());
}

#[test]
fn preserving_scene_omissions_is_explicit_reviewed_and_keeps_the_old_default() {
    let mut value = fixture();
    let mut second = value["scene_switch_groups"][1]["members"][0].clone();
    second["value"] = json!(2);
    value["scene_switch_groups"][0]["members"].as_array_mut().unwrap().push(second);
    value["scene_switch_groups"].as_array_mut().unwrap().truncate(1);
    let default = parse(&value).unwrap();
    let targets = default.scene_switch_groups[0].absolute_light_targets(&default.timed_scenes).unwrap();
    assert!(targets.values().all(|v| v.len() == 2), "unmodified configs retain the union-Off behavior");
    value["scene_switch_groups"][0]["reconciliation"]["omitted_targets"] = json!("preserve");
    let preserved = parse(&value).unwrap();
    let group = &preserved.scene_switch_groups[0];
    let targets = group.absolute_light_targets(&preserved.timed_scenes).unwrap();
    assert_eq!(targets[&1].keys().map(DeviceId::as_str).collect::<Vec<_>>(), ["courtyard-light"]);
    assert_eq!(targets[&2].keys().map(DeviceId::as_str).collect::<Vec<_>>(), ["passage-light"]);
    let powers = group.light_on_patterns(&preserved.timed_scenes).unwrap();
    assert!(powers.values().all(|v| v.len() == 1));
    let levels = group.profile_level_patterns(&preserved.timed_scenes).unwrap();
    assert!(levels.values().all(|v| v.len() == 1));
    let mut profiled = value.clone();
    profiled["scene_switch_groups"][0]["reconciliation"]["policy"] = json!("power_pattern_then_profile_level");
    assert!(parse(&profiled).is_ok());
    let config = Configuration::parse(&serde_json::to_vec(&json!({
        "schema_version":1,"revision":1,
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"11","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]}],
        "devices":[{"id":"courtyard-light","name":"Courtyard","gateway":"bus","address":"11","channel":1,"enabled":true},
                   {"id":"passage-light","name":"Passage","gateway":"bus","address":"11","channel":2,"enabled":true}]
    })).unwrap(), ExecutionContext::Simulation).unwrap();
    assert!(Document::parse(&serde_json::to_vec(&profiled).unwrap(), &config).is_err(), "sparse brightness refinement must not bypass established profile-domain validation");
    let mut disabled = value.clone();
    disabled["scene_switch_groups"][0]["reconciliation"]["enabled"] = json!(false);
    let disabled = parse(&disabled).unwrap();
    assert!(disabled.scene_switch_groups[0].absolute_light_targets(&disabled.timed_scenes).unwrap().values().all(|v| v.len() == 2));
    for (field, setting) in [("omitted_targets", json!("guess")), ("review_evidence", json!(""))] {
        let mut invalid = value.clone();
        invalid["scene_switch_groups"][0]["reconciliation"][field] = setting;
        assert!(parse(&invalid).is_err());
    }
}
