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
