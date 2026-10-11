use homesense_core::{
    DeviceId, ModuleKind,
    rule::{self, Action, Budget, Lifetime, Value, ValueType, Variable},
};
use homesense_import::semantic::{Binding, compile_event};
#[test]
fn original_study_xml_compiles_then_else_and_unknown_without_display_inference() {
    let bindings = [(
        "63".into(),
        Binding {
            device: DeviceId::new("control4:63").unwrap(),
            kind: ModuleKind::Keypad,
        },
    )]
    .into();
    for (mode, xml) in [
        (1, include_str!("fixtures/study-events/2.xml")),
        (2, include_str!("fixtures/study-events/6.xml")),
        (3, include_str!("fixtures/study-events/10.xml")),
    ] {
        let nodes = compile_event(xml, &bindings).unwrap();
        for old in [None, Some(0), Some(1), Some(2), Some(3)] {
            let vars = [(
                "411".into(),
                Variable {
                    value_type: ValueType::Integer,
                    lifetime: Lifetime::Persistent,
                    value: old.map(Value::Integer),
                },
            )]
            .into();
            let plan = rule::evaluate(&nodes, &vars, Budget::default()).unwrap();
            if old.is_none() {
                assert!(plan.actions.is_empty());
                assert_eq!(plan.unknown_branches.len(), 1);
                continue;
            }
            assert_eq!(
                plan.variables["411"].value,
                Some(Value::Integer(if old == Some(mode) { 0 } else { mode }))
            );
            let scene = plan
                .actions
                .iter()
                .find(|n| matches!(n.action, Action::Scene { .. }))
                .unwrap();
            assert!(
                matches!(&scene.action,Action::Scene {activate,..} if *activate==(old!=Some(mode)))
            );
        }
    }
}
#[test]
fn missing_binding_or_unknown_command_never_becomes_noop() {
    let xml = include_str!("fixtures/study-events/2.xml");
    assert!(compile_event(xml, &Default::default()).is_err());
    let bindings = [(
        "63".into(),
        Binding {
            device: DeviceId::new("control4:63").unwrap(),
            kind: ModuleKind::Keypad,
        },
    )]
    .into();
    assert!(compile_event(&xml.replace("Turn On", "Unrecognized Action"), &bindings).is_err());
}

#[test]
fn toggle_and_load_conditions_use_feedback_and_skip_unknown() {
    use homesense_core::{
        Command,
        rule::{Feedback, FeedbackSnapshot},
    };
    let id = DeviceId::new("switch").unwrap();
    let bindings = [(
        "17".into(),
        Binding {
            device: id.clone(),
            kind: ModuleKind::Switch,
        },
    )]
    .into();
    let xml = r#"<event><deviceid>17</deviceid><eventid>1</eventid><codeitem><id>1</id><device>17</device><type>1</type><enabled>True</enabled><cmdcond><devicecommand owneridtype="" owneriditem="-1"><command>TOGGLE</command><params/></devicecommand></cmdcond><subitems/></codeitem></event>"#;
    let nodes = compile_event(xml, &bindings).unwrap();
    assert!(
        rule::evaluate(&nodes, &Default::default(), Budget::default())
            .unwrap()
            .commands
            .is_empty()
    );
    for on in [false, true] {
        let devices = [(
            id.clone(),
            Feedback {
                on: Some(on),
                level: None,
                observed_ms: 100,
            },
        )]
        .into();
        let plan = rule::evaluate_with_feedback(
            &nodes,
            &Default::default(),
            Budget::default(),
            FeedbackSnapshot {
                devices: &devices,
                now_ms: 100,
                max_age_ms: 10,
            },
        )
        .unwrap();
        assert_eq!(plan.commands[0].command, Command::Switch(!on));
    }
    for name in ["LOAD_ON", "LOAD_OFF"] {
        let condition_xml = format!(
            r#"<event><deviceid>17</deviceid><eventid>1</eventid><codeitem><id>2</id><device>17</device><type>2</type><enabled>True</enabled><cmdcond><deviceconditional owneridtype="" owneriditem="-1"><name>{name}</name></deviceconditional></cmdcond><subitems>{}</subitems></codeitem></event>"#,
            &xml[xml.find("<codeitem>").unwrap()..xml.rfind("</event>").unwrap()]
        );
        let nodes = compile_event(&condition_xml, &bindings).unwrap();
        for on in [false, true] {
            let devices = [(
                id.clone(),
                Feedback {
                    on: Some(on),
                    level: None,
                    observed_ms: 100,
                },
            )]
            .into();
            let plan = rule::evaluate_with_feedback(
                &nodes,
                &Default::default(),
                Budget::default(),
                FeedbackSnapshot {
                    devices: &devices,
                    now_ms: 100,
                    max_age_ms: 10,
                },
            )
            .unwrap();
            assert_eq!(plan.commands.len(), usize::from(on == (name == "LOAD_ON")));
        }
    }
}

#[test]
fn compound_and_or_preserve_three_valued_logic() {
    fn operand(id: u8, variable: &str, expression: &str, children: &str) -> String {
        format!(
            r#"<codeitem><id>{id}</id><device>100001</device><type>2</type><enabled>True</enabled><cmdcond><deviceconditional owneridtype="variable" owneriditem="{variable}" name="=="><param name="value" type="int">1</param></deviceconditional></cmdcond>{expression}<subitems>{children}</subitems></codeitem>"#
        )
    }
    let action = r#"<codeitem><id>4</id><device>100001</device><type>1</type><enabled>True</enabled><cmdcond><devicecommand owneridtype="variable" owneriditem="result" name="="><param name="value" type="int">1</param></devicecommand></cmdcond><subitems/></codeitem>"#;
    for op in ["AND", "OR"] {
        let expression = format!(
            r#"<expression><codeitem><id>2</id><device>100000</device><type>6</type><display>{op}</display><enabled>True</enabled><cmdcond/><subitems/></codeitem>{}</expression>"#,
            operand(3, "b", "", "")
        );
        let xml = format!(
            "<event><deviceid>1</deviceid><eventid>1</eventid>{}</event>",
            operand(1, "a", &expression, action)
        );
        let nodes = compile_event(&xml, &Default::default()).unwrap();
        for a in [None, Some(0), Some(1)] {
            for b in [None, Some(0), Some(1)] {
                let vars = [("a", a), ("b", b), ("result", Some(0))]
                    .into_iter()
                    .map(|(name, v)| {
                        (
                            name.into(),
                            Variable {
                                value_type: ValueType::Integer,
                                lifetime: Lifetime::Persistent,
                                value: v.map(Value::Integer),
                            },
                        )
                    })
                    .collect();
                let plan = rule::evaluate(&nodes, &vars, Budget::default()).unwrap();
                let active = if op == "AND" {
                    a == Some(1) && b == Some(1)
                } else {
                    a == Some(1) || b == Some(1)
                };
                assert_eq!(
                    plan.variables["result"].value,
                    Some(Value::Integer(i64::from(active)))
                );
            }
        }
        assert!(
            compile_event(
                &xml.replace(
                    &format!("<display>{op}</display>"),
                    "<display>XOR</display>"
                ),
                &Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn dimmer_level_comparison_is_strict_and_off_is_zero() {
    use homesense_core::{
        Command, LevelTransition, Percent,
        rule::{Feedback, FeedbackSnapshot},
    };
    let id = DeviceId::new("dimmer").unwrap();
    let bindings = [(
        "17".into(),
        Binding {
            device: id.clone(),
            kind: ModuleKind::Dimmer,
        },
    )]
    .into();
    let xml = r#"<event><deviceid>17</deviceid><eventid>1</eventid><codeitem><id>1</id><device>17</device><type>2</type><enabled>True</enabled><cmdcond><deviceconditional owneridtype="" owneriditem="-1"><name>IS_LEVEL</name><conditional>GREATER_THAN</conditional><params><param><name>SETTING</name><value type="INTEGER"><static>10</static></value></param></params></deviceconditional></cmdcond><subitems><codeitem><id>2</id><device>17</device><type>1</type><enabled>True</enabled><cmdcond><devicecommand><command>OFF</command><params/></devicecommand></cmdcond><subitems/></codeitem></subitems></codeitem></event>"#;
    let nodes = compile_event(xml, &bindings).unwrap();
    for level in [None, Some(9), Some(10), Some(11), Some(100)] {
        let devices = [(
            id.clone(),
            Feedback {
                on: None,
                level: level.map(|v| Percent::new(v).unwrap()),
                observed_ms: 100,
            },
        )]
        .into();
        let plan = rule::evaluate_with_feedback(
            &nodes,
            &Default::default(),
            Budget::default(),
            FeedbackSnapshot {
                devices: &devices,
                now_ms: 100,
                max_age_ms: 10,
            },
        )
        .unwrap();
        if level.is_some_and(|n| n > 10) {
            assert_eq!(
                plan.commands[0].command,
                Command::SetLevel {
                    level: Percent::new(0).unwrap(),
                    transition: LevelTransition::Immediate
                }
            );
        } else {
            assert!(plan.commands.is_empty());
        }
    }
    assert!(
        compile_event(
            &xml.replace("<static>10</static>", "<static>101</static>"),
            &bindings
        )
        .is_err()
    );
    assert!(compile_event(&xml.replace("GREATER_THAN", "UNKNOWN"), &bindings).is_err());
}
