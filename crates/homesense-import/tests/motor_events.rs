use homesense_core::{
    DeviceId, ExecutionContext, ModuleKind, MotorAction,
    rule::{Action, Condition},
};
use homesense_import::semantic::{
    Binding, MotorOptions, MotorReview, compile_event, compile_event_with_motors,
};
use std::collections::BTreeMap;
fn condition(name: &str) -> String {
    format!(
        r#"<codeitem><id>1</id><device>17</device><type>2</type><enabled>True</enabled><cmdcond><deviceconditional owneridtype="" owneriditem="-1"><name>{name}</name></deviceconditional></cmdcond><subitems>{}</subitems></codeitem>"#,
        command("SET_LEVEL_TARGET", "Open", "2")
    )
}
fn command(command: &str, value: &str, id: &str) -> String {
    let params = if command == "STOP" {
        "<params/>".into()
    } else {
        format!(
            r#"<params><param><name>LEVEL_TARGET_NAME</name><value type="STRING"><static>{value}</static></value></param></params>"#
        )
    };
    format!(
        r#"<codeitem><id>{id}</id><device>17</device><type>1</type><enabled>True</enabled><cmdcond><devicecommand owneridtype="" owneriditem="-1"><command>{command}</command>{params}</devicecommand></cmdcond><subitems/></codeitem>"#
    )
}
fn event(nodes: &str) -> String {
    format!("<event><deviceid>17</deviceid><eventid>1</eventid>{nodes}</event>")
}
fn project(event: &str) -> String {
    format!(
        r#"<project><systemitems><item><id>16</id><type>6</type><name>Synthetic controller</name><itemdata><config_data_file>GJ-TECH.CN_Blinds.c4z</config_data_file></itemdata><state><![CDATA[<state><property><name>Device ID</name><value>37</value></property><property><name>Channel</name><value>3</value></property></state>]]></state><subitems><item><id>17</id><type>7</type><name>Synthetic proxy</name><itemdata><config_data_file>blind.c4i</config_data_file></itemdata></item></subitems></item></systemitems><event_mgr>{event}</event_mgr></project>"#
    )
}
fn cfg(calibrated: bool) -> homesense_config::Configuration {
    let mut d = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"37","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"control4:16","name":"Synthetic","module_id":"motor-module","channel":3,"enabled":true}]});
    if calibrated {
        d["devices"][0]["motor_calibration"] = serde_json::json!({"open_direction":"down","stop_verified":true,"evidence":"synthetic calibration","verified_at_unix_ms":1});
    }
    homesense_config::Configuration::parse(
        &serde_json::to_vec(&d).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn bindings() -> BTreeMap<String, Binding> {
    [(
        "17".into(),
        Binding {
            device: DeviceId::new("control4:16").unwrap(),
            kind: ModuleKind::Motor,
        },
    )]
    .into()
}
fn review(event: &str) -> MotorReview {
    use sha2::{Digest, Sha256};
    MotorReview {
        source_key: "17:1".into(),
        source_xml_sha256: Sha256::digest(event.as_bytes())
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect(),
        source_target: "17".into(),
        device: DeviceId::new("control4:16").unwrap(),
        mapping_review: "synthetic source mapping, not semantic approval".into(),
    }
}
#[test]
fn opt_in_preserves_guards_source_tags_order_and_disabled_output() {
    for name in ["IF_STOPPED", "IF_MOVING"] {
        let xml = event(
            &(condition(name)
                + &command("SET_LEVEL_TARGET", "Closed", "3")
                + &command("STOP", "", "4")),
        );
        assert!(compile_event(&xml, &bindings()).is_err());
        let project = project(&xml);
        let config = cfg(true);
        let reviews = vec![review(&xml)];
        let options = MotorOptions::reviewed_project(&config, &project, &reviews).unwrap();
        let compiled = compile_event_with_motors(&xml, &bindings(), options).unwrap();
        let Action::If {
            condition,
            then_actions,
            ..
        } = &compiled.actions[0].action
        else {
            panic!()
        };
        assert!(
            matches!(condition,Condition::MotorState(p) if p.device.as_str()=="control4:16" && p.evidence==homesense_core::rule_motor::Evidence::ActualOnly)
        );
        assert!(matches!(
            then_actions[0].action,
            Action::Motor {
                action: MotorAction::Open,
                ..
            }
        ));
        assert!(matches!(
            compiled.actions[1].action,
            Action::Motor {
                action: MotorAction::Close,
                ..
            }
        ));
        assert!(matches!(
            compiled.actions[2].action,
            Action::Motor {
                action: MotorAction::Stop,
                ..
            }
        ));
        assert_eq!(compiled.actions[0].source, "device:17/event:1/code:1");
        assert_eq!(then_actions[0].source, "device:17/event:1/code:1/code:2");
        assert_eq!(compiled.actions[2].source, "device:17/event:1/code:4");
        let draft = homesense_import::event_export::draft(&compiled).unwrap();
        assert_eq!(draft["enabled"], false);
        assert_eq!(draft["automatic_execution"], false);
        assert_eq!(draft["actions"][1]["action"]["action"], "close");
    }
}
#[test]
fn targets_are_exact_and_dynamic_wrong_type_or_stop_arguments_are_rejected() {
    for value in ["Close", "closed", "open", "100", " Open "] {
        let xml = event(&command("SET_LEVEL_TARGET", value, "1"));
        let project = project(&xml);
        let config = cfg(true);
        let reviews = vec![review(&xml)];
        let options = MotorOptions::reviewed_project(&config, &project, &reviews).unwrap();
        assert!(
            compile_event_with_motors(&xml, &bindings(), options).is_err(),
            "{value}"
        );
    }
    for xml in [
        event(&command("SET_LEVEL_TARGET", "Open", "1"))
            .replace("type=\"STRING\"", "type=\"BOOLEAN\""),
        event(&command("SET_LEVEL_TARGET", "Open", "1"))
            .replace("<static>Open</static>", "<dynamic>Open</dynamic>"),
        event(&command("STOP", "", "1")).replace("<params/>", "<params><param/></params>"),
    ] {
        let project = project(&xml);
        let config = cfg(true);
        let reviews = vec![review(&xml)];
        let options = MotorOptions::reviewed_project(&config, &project, &reviews).unwrap();
        assert!(compile_event_with_motors(&xml, &bindings(), options).is_err());
    }
}
#[test]
fn missing_review_calibration_wrong_source_hash_or_split_proxy_identity_cannot_compile() {
    let xml = event(&condition("IF_STOPPED"));
    let project = project(&xml);
    let config = cfg(true);
    let reviews = vec![review(&xml)];
    assert!(MotorOptions::reviewed_project(&cfg(false), &project, &reviews).is_err());
    let options = MotorOptions::reviewed_project(&config, &project, &[]).unwrap();
    assert!(compile_event_with_motors(&xml, &bindings(), options).is_err());
    let mut bad = vec![review(&xml)];
    bad[0].source_xml_sha256 = "0".repeat(64);
    assert!(MotorOptions::reviewed_project(&config, &project, &bad).is_err());
    let mut bad = vec![review(&xml)];
    bad[0].device = DeviceId::new("control4:17").unwrap();
    assert!(MotorOptions::reviewed_project(&config, &project, &bad).is_err());
    let options = MotorOptions::reviewed_project(&config, &project, &reviews).unwrap();
    let wrong = [(
        "17".into(),
        Binding {
            device: DeviceId::new("control4:17").unwrap(),
            kind: ModuleKind::Motor,
        },
    )]
    .into();
    assert!(compile_event_with_motors(&xml, &wrong, options).is_err());
}

#[test]
fn original_integer_typed_open_closed_enum_is_not_a_numeric_percentage() {
    for (value, expected) in [("Open", MotorAction::Open), ("Closed", MotorAction::Close)] {
        let xml = event(&command("SET_LEVEL_TARGET", value, "1"))
            .replace("type=\"STRING\"", "type=\"INTEGER\"");
        let project = project(&xml);
        let config = cfg(true);
        let reviews = vec![review(&xml)];
        let options = MotorOptions::reviewed_project(&config, &project, &reviews).unwrap();
        let compiled = compile_event_with_motors(&xml, &bindings(), options).unwrap();
        assert!(matches!(compiled.actions[0].action,Action::Motor{action,..} if action==expected));
    }
}

#[test]
fn closest_item_ancestor_and_unique_descriptor_reject_foreign_or_ambiguous_proxy_claims() {
    let xml = event(&condition("IF_STOPPED"));
    let input = project(&xml);
    let config = cfg(true);
    let reviews = vec![review(&xml)];
    assert!(
        MotorOptions::reviewed_project(&config, &input, &reviews).is_ok(),
        "subitems wrapper retains the nearest controller ancestor"
    );
    let ambiguous=input.replace("</subitems>","<item><id>18</id><type>7</type><name>Duplicate proxy</name><itemdata><config_data_file>blind.c4i</config_data_file></itemdata></item></subitems>");
    assert!(MotorOptions::reviewed_project(&config, &ambiguous, &reviews).is_err());
    let nested=input.replace("<subitems><item><id>17", "<subitems><item><id>19</id><type>6</type><name>Other controller container</name><subitems><item><id>17").replace("</item></subitems></item></systemitems>","</item></subitems></item></subitems></item></systemitems>");
    assert!(
        MotorOptions::reviewed_project(&config, &nested, &reviews).is_err(),
        "another closest item ancestor cannot donate its proxy"
    );
    let wrong_descriptor = input.replace("blind.c4i", "other-proxy.c4i");
    assert!(MotorOptions::reviewed_project(&config, &wrong_descriptor, &reviews).is_err());
}
