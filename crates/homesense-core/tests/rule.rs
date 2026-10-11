use homesense_core::{Command, DeviceId, rule::*};
use std::collections::BTreeMap;
fn variable(v: Option<Value>) -> BTreeMap<String, Variable> {
    [(
        "mode".into(),
        Variable {
            value_type: ValueType::Integer,
            lifetime: Lifetime::Persistent,
            value: v,
        },
    )]
    .into()
}
fn node(action: Action) -> Node {
    Node {
        source: "event/1/node/2".into(),
        action,
    }
}
fn command(on: bool) -> Node {
    node(Action::Command {
        device: DeviceId::new("light").unwrap(),
        command: Command::Switch(on),
    })
}
fn condition() -> Condition {
    Condition::Equal(
        Expr::Variable("mode".into()),
        Expr::Literal(Value::Integer(1)),
    )
}
#[test]
fn unknown_skips_both_branches_and_ordered_assignment_is_visible() {
    let branch = node(Action::If {
        condition: condition(),
        then_actions: vec![command(true)],
        else_actions: vec![command(false)],
    });
    let vars = variable(None);
    let plan = evaluate(std::slice::from_ref(&branch), &vars, Budget::default()).unwrap();
    assert!(plan.commands.is_empty());
    assert_eq!(plan.unknown_branches.len(), 1);
    let plan = evaluate(
        &[
            node(Action::Set {
                variable: "mode".into(),
                value: Expr::Literal(Value::Integer(1)),
            }),
            branch,
        ],
        &vars,
        Budget::default(),
    )
    .unwrap();
    assert_eq!(plan.commands[0].command, Command::Switch(true));
    assert_eq!(vars["mode"].value, None);
}
#[test]
fn nested_else_and_three_valued_logic_preserve_source() {
    let c = Condition::All(vec![
        Condition::Not(Box::new(condition())),
        Condition::Unknown(Expr::Variable("mode".into())),
    ]);
    let nodes = [node(Action::If {
        condition: c,
        then_actions: vec![command(true)],
        else_actions: vec![command(false)],
    })];
    assert!(
        evaluate(&nodes, &variable(None), Budget::default())
            .unwrap()
            .commands
            .is_empty()
    );
    assert_eq!(
        evaluate(
            &nodes,
            &variable(Some(Value::Integer(1))),
            Budget::default()
        )
        .unwrap()
        .commands[0]
            .command,
        Command::Switch(false)
    );
}
#[test]
fn budget_failure_returns_no_partial_plan_or_input_mutation() {
    let vars = variable(Some(Value::Integer(0)));
    let nodes = [
        node(Action::Set {
            variable: "mode".into(),
            value: Expr::Literal(Value::Integer(1)),
        }),
        command(true),
        command(false),
    ];
    assert!(matches!(
        evaluate(
            &nodes,
            &vars,
            Budget {
                commands: 1,
                ..Budget::default()
            }
        ),
        Err(Error::Budget)
    ));
    assert_eq!(vars["mode"].value, Some(Value::Integer(0)));
    assert!(matches!(
        evaluate(
            &nodes,
            &vars,
            Budget {
                depth: 0,
                ..Budget::default()
            }
        ),
        Err(Error::Budget)
    ));
}
#[test]
fn restart_lifetimes_and_cache_write_protection() {
    let mut vars = variable(Some(Value::Integer(1)));
    vars.insert(
        "cache".into(),
        Variable {
            value_type: ValueType::Bool,
            lifetime: Lifetime::FeedbackCache,
            value: Some(Value::Bool(true)),
        },
    );
    vars.insert(
        "temporary".into(),
        Variable {
            value_type: ValueType::Bool,
            lifetime: Lifetime::Temporary,
            value: Some(Value::Bool(true)),
        },
    );
    restart(&mut vars);
    assert_eq!(vars["mode"].value, Some(Value::Integer(1)));
    assert!(vars["cache"].value.is_none());
    assert!(vars["temporary"].value.is_none());
    assert!(matches!(
        evaluate(
            &[node(Action::Set {
                variable: "cache".into(),
                value: Expr::Literal(Value::Bool(false))
            })],
            &vars,
            Budget::default()
        ),
        Err(Error::ReadOnlyVariable)
    ));
}
#[test]
fn invalid_reference_and_types_fail() {
    assert!(matches!(
        evaluate(
            &[node(Action::If {
                condition: condition(),
                then_actions: vec![],
                else_actions: vec![]
            })],
            &variable(Some(Value::Bool(true))),
            Budget::default()
        ),
        Err(Error::TypeMismatch)
    ));
    assert!(matches!(
        evaluate(
            &[node(Action::Set {
                variable: "missing".into(),
                value: Expr::Literal(Value::Integer(0))
            })],
            &variable(None),
            Budget::default()
        ),
        Err(Error::UndefinedVariable(_))
    ));
}

#[test]
fn integer_arithmetic_comparison_unknown_and_overflow_follow_source_semantics() {
    let increment = Expr::Add(
        Box::new(Expr::Variable("mode".into())),
        Box::new(Expr::Literal(Value::Integer(4))),
    );
    let nodes = [
        node(Action::Set {
            variable: "mode".into(),
            value: increment,
        }),
        node(Action::If {
            condition: Condition::Less(
                Expr::Variable("mode".into()),
                Expr::Literal(Value::Integer(100)),
            ),
            then_actions: vec![command(true)],
            else_actions: vec![command(false)],
        }),
    ];
    let plan = evaluate(
        &nodes,
        &variable(Some(Value::Integer(95))),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(plan.variables["mode"].value, Some(Value::Integer(99)));
    assert_eq!(plan.commands[0].command, Command::Switch(true));
    let plan = evaluate(&nodes, &variable(None), Budget::default()).unwrap();
    assert!(plan.commands.is_empty());
    assert_eq!(plan.unknown_branches.len(), 1);
    assert!(matches!(
        evaluate(
            &nodes,
            &variable(Some(Value::Integer(i64::MAX))),
            Budget::default()
        ),
        Err(Error::ArithmeticOverflow)
    ));
    let minus = [node(Action::Set {
        variable: "mode".into(),
        value: Expr::Subtract(
            Box::new(Expr::Variable("mode".into())),
            Box::new(Expr::Literal(Value::Integer(4))),
        ),
    })];
    assert_eq!(
        evaluate(
            &minus,
            &variable(Some(Value::Integer(0))),
            Budget::default()
        )
        .unwrap()
        .variables["mode"]
            .value,
        Some(Value::Integer(-4))
    );
}

#[test]
fn device_conditions_require_fresh_actual_feedback() {
    let id = DeviceId::new("light").unwrap();
    let branch = node(Action::If {
        condition: Condition::Equal(Expr::DeviceOn(id.clone()), Expr::Literal(Value::Bool(true))),
        then_actions: vec![command(false)],
        else_actions: vec![command(true)],
    });
    let vars = BTreeMap::new();
    assert!(
        evaluate(std::slice::from_ref(&branch), &vars, Budget::default())
            .unwrap()
            .commands
            .is_empty()
    );
    for (observed_ms, max_age_ms, on, expected) in [
        (91, 10, Some(true), Some(false)),
        (91, 10, Some(false), Some(true)),
        (90, 10, Some(true), None),
        (101, 10, Some(true), None),
        (100, 0, Some(true), None),
        (100, 10, None, None),
    ] {
        let devices = [(
            id.clone(),
            Feedback {
                on,
                level: None,
                observed_ms,
            },
        )]
        .into();
        let plan = evaluate_with_feedback(
            std::slice::from_ref(&branch),
            &vars,
            Budget::default(),
            FeedbackSnapshot {
                devices: &devices,
                now_ms: 100,
                max_age_ms,
            },
        )
        .unwrap();
        match expected {
            Some(on) => assert_eq!(plan.commands[0].command, Command::Switch(on)),
            None => {
                assert!(plan.commands.is_empty());
                assert_eq!(plan.unknown_branches.len(), 1);
            }
        }
    }
    let devices = [(
        id.clone(),
        Feedback {
            on: None,
            level: Some(homesense_core::Percent::new(37).unwrap()),
            observed_ms: 100,
        },
    )]
    .into();
    let plan = evaluate_with_feedback(
        &[node(Action::If {
            condition: Condition::Equal(Expr::DeviceLevel(id), Expr::Literal(Value::Integer(37))),
            then_actions: vec![command(true)],
            else_actions: vec![],
        })],
        &vars,
        Budget::default(),
        FeedbackSnapshot {
            devices: &devices,
            now_ms: 100,
            max_age_ms: 10,
        },
    )
    .unwrap();
    assert_eq!(plan.commands.len(), 1);
}

#[test]
fn feedback_read_set_includes_unknown_but_excludes_unvisited_branch() {
    let missing = DeviceId::new("missing").unwrap();
    let skipped = DeviceId::new("skipped").unwrap();
    let nodes = [node(Action::If {
        condition: Condition::Equal(
            Expr::DeviceOn(missing.clone()),
            Expr::Literal(Value::Bool(true)),
        ),
        then_actions: vec![node(Action::If {
            condition: Condition::Unknown(Expr::DeviceLevel(skipped)),
            then_actions: vec![command(true)],
            else_actions: vec![],
        })],
        else_actions: vec![],
    })];
    let plan = evaluate(&nodes, &BTreeMap::new(), Budget::default()).unwrap();
    assert_eq!(plan.feedback_reads, [missing].into());
    assert!(plan.commands.is_empty());
}
