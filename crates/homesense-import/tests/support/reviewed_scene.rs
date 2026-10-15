#![allow(dead_code)]
use homesense_core::ExecutionContext;
use homesense_import::reviewed_scenes::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn scene(id: u32, toggle: &str, members: &[(u32, u8, u64, u64)]) -> String {
    format!("<AdvScene><name>Same synthetic display</name><scene_id>{id}</scene_id><track_mode>synthetic-no-tracking</track_mode><hold_rate_up>0</hold_rate_up><hold_rate_down>0</hold_rate_down><toggle_id>{toggle}</toggle_id><is_active>True</is_active><full_on>False</full_on><full_off>False</full_off><ramp_capable>False</ramp_capable><all_members>{}</all_members></AdvScene>",members.iter().map(|(proxy,level,delay,rate)|format!("<AdvSceneMember><device_id>{proxy}</device_id><track_mode>synthetic-no-tracking</track_mode><track_lvl>100</track_lvl><flash>False</flash><ignore_ramp>False</ignore_ramp><all_elements><element><delay>{delay}</delay><rate>{rate}</rate><level>{level}</level></element></all_elements></AdvSceneMember>")).collect::<String>())
}
pub fn call(node: u32, agent: u32, id: u32, activate: bool, enabled: bool) -> String {
    format!(
        "<codeitem><id>{node}</id><device>{agent}</device><type>1</type><enabled>{}</enabled><cmdcond><devicecommand owneridtype=\"agent\" owneriditem=\"{id}\"><command>{}</command><params><param><name>SCENE_ID</name><value><static>{id}</static></value></param></params></devicecommand></cmdcond><subitems/></codeitem>",
        if enabled { "True" } else { "False" },
        if activate {
            "ACTIVATE_SCENE"
        } else {
            "DEACTIVATE_SCENE"
        }
    )
}
pub fn event(inside: &str) -> String {
    format!("<event><deviceid>900</deviceid><eventid>2</eventid>{inside}</event>")
}
pub fn guarded_event(inside: &str) -> String {
    event(&format!(
        "<codeitem><id>1</id><device>100001</device><type>1</type><enabled>True</enabled><cmdcond><devicecommand owneridtype=\"variable\" owneriditem=\"7\" name=\"=\"><param name=\"value\" type=\"int\">1</param></devicecommand></cmdcond><subitems/></codeitem><codeitem><id>2</id><device>100001</device><type>2</type><enabled>True</enabled><cmdcond><deviceconditional owneridtype=\"variable\" owneriditem=\"7\" name=\"==\"><param name=\"value\" type=\"int\">1</param></deviceconditional></cmdcond><subitems>{inside}</subitems></codeitem>"
    ))
}
pub fn project(first: &str, second: &str, event: &str, dimmer: bool) -> String {
    let controls=(0..40).map(|i|format!("<item><id>{}</id><name>Synthetic load</name><itemdata><config_data_file>GJ-TECH.CN_{}_V2.c4z</config_data_file></itemdata><state><![CDATA[<state><property><name>Device ID</name><value>{:02X}</value></property><property><name>Channel</name><value>{}</value></property></state>]]></state><subitems><item><id>{}</id><type>7</type><name>Proxy</name></item></subitems></item>",1000+i,if dimmer && i==0{"Dimmer"}else{"Switch"},16+i/8,i%8+1,2000+i)).collect::<String>();
    format!(
        "<project><systemitems><item><id>7</id><name>Synthetic agent A</name><state><![CDATA[<state>{first}</state>]]></state></item><item><id>8</id><name>Synthetic agent B</name><state><![CDATA[<state>{second}</state>]]></state></item>{controls}</systemitems><event_mgr>{event}</event_mgr></project>"
    )
}
pub fn config_bytes(dimmer: bool) -> Vec<u8> {
    let mut modules = Vec::new();
    let mut devices = Vec::new();
    // First channel dimmer, remaining relays on a separate module when requested.
    for i in 0..5 {
        modules.push(json!({"id":format!("r{i}"),"address":format!("{:02X}",16+i),"profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    }
    if dimmer {
        modules.push(json!({"id":"d","address":"20","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}));
    }
    for i in 0..40 {
        devices.push(json!({"id":format!("control4:{}",1000+i),"name":"Synthetic load","module_id":if dimmer&&i==0{"d".to_string()}else{format!("r{}",i/8)},"channel":i%8+1,"enabled":true}));
    }
    serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap()
}
pub fn config(dimmer: bool) -> homesense_config::Configuration {
    homesense_config::Configuration::parse(&config_bytes(dimmer), ExecutionContext::Simulation)
        .unwrap()
}
pub fn review(xml: &str) -> Review {
    let report = homesense_import::import_bytes(xml.as_bytes()).unwrap();
    let sources = inventory(xml).unwrap();
    let fields = |s: &SourceScene| {
        s.fields
            .iter()
            .map(|(path, value)| {
                let name = path.rsplit('/').next().unwrap().split('[').next().unwrap();
                let disposition = match name {
                    "name" => Disposition::Display,
                    "scene_id" => Disposition::Identity,
                    "device_id" => Disposition::Binding,
                    "level" => Disposition::Level,
                    "delay" => Disposition::Delay,
                    "rate" => Disposition::Rate,
                    "toggle_id" => Disposition::Toggle,
                    "is_active" => Disposition::SourceHistory,
                    _ => Disposition::Nonoperative,
                };
                (
                    path.clone(),
                    FieldReview {
                        expected: value.clone(),
                        disposition,
                        review_evidence: format!("synthetic explicit {path}"),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    Review {
        project_xml_sha256: report.source.unwrap().project_xml_sha256,
        configuration_revision: 1,
        bindings: (0..40)
            .map(|i| BindingReview {
                behavior: BindingBehavior::Standalone,
                source_device: format!("{}", 2000 + i),
                logical_device: format!("control4:{}", 1000 + i),
                review_evidence: "synthetic reviewed module identity".into(),
            })
            .collect(),
        scenes: sources
            .iter()
            .map(|s| {
                let raw = s.fields["AdvScene/toggle_id[0]"].clone();
                let target = format!("control4:scene:{}:{raw}", s.agent);
                Contract {
                    deactivation_runtime:None,
                    tracking: None,
                    scene: s.id.clone(),
                    source_sha256: s.source_sha256.clone(),
                    review_evidence: "synthetic contract; no original household semantics approved"
                        .into(),
                    fields: fields(s),
                    delay_unit: Unit::Milliseconds,
                    delay_anchor: Anchor::SceneStart,
                    rate_duration_unit: Unit::Milliseconds,
                    rate_backend: RateBackend::ZeroOnly,
                    zero_rate: ZeroRate::Immediate,
                    nonoperative_tracking_tokens: ["synthetic-no-tracking".into()].into(),
                    deactivation: if sources.iter().any(|s| s.id == target) {
                        Deactivation::Target { scene_id: raw }
                    } else {
                        Deactivation::Unavailable { raw_value: raw }
                    },
                    max_lateness_ms: 1000,
                }
            })
            .collect(),
    }
}
pub fn document_rule(actions: Value, enabled: bool) -> Value {
    json!({"id":"synthetic-rule","enabled":enabled,"review_evidence":"synthetic lab only","source":"synthetic event source","trigger":{"kind":"startup_synchronized"},"actions":actions})
}
pub fn topology() -> (String, String) {
    let source = (2000..2004).map(|id| (id, 100, 0, 0)).collect::<Vec<_>>();
    let target = (2004..2040).map(|id| (id, 0, 0, 0)).collect::<Vec<_>>();
    (
        format!(
            "{}{}",
            scene(50, "11", &source),
            scene(11, "synthetic-unavailable", &target)
        ),
        scene(50, "synthetic-unavailable", &[(2000, 0, 0, 0)]),
    )
}

pub fn combined_config_bytes() -> Vec<u8> {
    let mut cfg: Value = serde_json::from_slice(&config_bytes(true)).unwrap();
    cfg["gateways"][0]["modules"][5]["profile"] = "ad8_din_rs485_v2_2_1".into();
    cfg["devices"][0]["default_on_level"] = 100.into();
    cfg["devices"][0]["supply"] = json!({"module_id":"r0","channel":2});
    serde_json::to_vec(&cfg).unwrap()
}
pub fn combined_config() -> homesense_config::Configuration {
    homesense_config::Configuration::parse(&combined_config_bytes(), ExecutionContext::Simulation)
        .unwrap()
}

pub fn combined_review(xml: &str) -> Review {
    let mut r = review(xml);
    let b = r
        .bindings
        .iter_mut()
        .find(|b| b.source_device == "2000")
        .unwrap();
    b.behavior = BindingBehavior::ReviewedCombinedLightNormalizedLevel;
    b.review_evidence =
        "synthetic explicit normalized combined-light level review including owned supply planning"
            .into();
    r
}
