use homesense_core::{
    Command, DeviceId,
    curtain_state::{Movement, Provenance, Scope},
    queue::Epochs,
    rule::{self, Action, Budget, Condition, Expr, Node, Truth, Value},
    rule_motor::{Evidence, Predicate, Reading, Request, Snapshot, State},
};
fn scope() -> Scope {
    Scope {
        boot: 1,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
    }
}
fn predicate(state: State) -> Predicate {
    Predicate {
        device: DeviceId::new("motor").unwrap(),
        state,
        evidence: Evidence::ActualOnly,
        max_age_ms: 100,
        mapping_review: "synthetic mapping".into(),
        evidence_review: None,
    }
}
fn nodes(condition: Condition) -> Vec<Node> {
    vec![Node {
        source: "guard".into(),
        action: Action::If {
            condition,
            then_actions: vec![Node {
                source: "then".into(),
                action: Action::Command {
                    device: DeviceId::new("relay").unwrap(),
                    command: Command::Switch(true),
                },
            }],
            else_actions: vec![Node {
                source: "else".into(),
                action: Action::Command {
                    device: DeviceId::new("relay").unwrap(),
                    command: Command::Switch(false),
                },
            }],
        },
    }]
}
fn evaluate(
    condition: Condition,
    samples: &[Reading],
    expected: Option<Scope>,
    now: u64,
) -> rule::Plan {
    rule::evaluate_with_inputs(
        &nodes(condition),
        &Default::default(),
        Budget::default(),
        rule::FeedbackSnapshot {
            devices: &Default::default(),
            now_ms: now,
            max_age_ms: 100,
        },
        Snapshot {
            readings: samples,
            scope: expected,
            now_ms: now,
        },
    )
    .unwrap()
}
#[test]
fn trusted_moving_and_stopped_are_distinct_and_unknown_skips_both_branches() {
    for state in [State::Moving, State::Stopped] {
        let p = predicate(state);
        let condition = Condition::MotorState(p.clone());
        let unknown = evaluate(condition.clone(), &[], Some(scope()), 10);
        assert!(unknown.actions.is_empty());
        assert_eq!(unknown.unknown_branches, vec!["guard"]);
        assert_eq!(unknown.motor_reads[0].truth, Truth::Unknown);
        assert!(unknown.feedback_reads.is_empty());
        for movement in [Movement::Opening, Movement::Closing, Movement::Stopped] {
            let reading = Reading {
                request: Request {
                    source: "guard".into(),
                    predicate: p.clone(),
                },
                scope: scope(),
                captured_ms: 10,
                movement: Some(movement),
                provenance: Some(Provenance::Observed),
                pending: false,
            };
            let result = evaluate(condition.clone(), &[reading], Some(scope()), 11);
            let matching = if state == State::Stopped {
                movement == Movement::Stopped
            } else {
                movement != Movement::Stopped
            };
            assert_eq!(result.commands[0].command, Command::Switch(matching));
            assert_eq!(result.motor_reads[0].provenance, Some(Provenance::Observed));
        }
    }
}
#[test]
fn inferred_is_opt_in_and_pending_expiry_boot_and_all_epochs_are_unknown() {
    let mut p = predicate(State::Stopped);
    let mut r = Reading {
        request: Request {
            source: "guard".into(),
            predicate: p.clone(),
        },
        scope: scope(),
        captured_ms: 10,
        movement: Some(Movement::Stopped),
        provenance: Some(Provenance::Inferred),
        pending: false,
    };
    assert!(
        evaluate(
            Condition::MotorState(p.clone()),
            &[r.clone()],
            Some(scope()),
            11
        )
        .actions
        .is_empty()
    );
    p.evidence = Evidence::ReviewedCommandEstimate;
    p.evidence_review = Some("synthetic predicate review".into());
    r.request.predicate = p.clone();
    let result = evaluate(
        Condition::MotorState(p.clone()),
        &[r.clone()],
        Some(scope()),
        11,
    );
    assert_eq!(result.motor_reads[0].provenance, Some(Provenance::Inferred));
    assert_eq!(result.commands.len(), 1);
    for now in [9, 110] {
        assert!(
            evaluate(
                Condition::MotorState(p.clone()),
                &[r.clone()],
                Some(scope()),
                now
            )
            .actions
            .is_empty()
        );
    }
    r.pending = true;
    assert!(
        evaluate(
            Condition::MotorState(p.clone()),
            &[r.clone()],
            Some(scope()),
            11
        )
        .actions
        .is_empty()
    );
    r.pending = false;
    for changed in [
        Scope { boot: 2, ..scope() },
        Scope {
            epochs: Epochs {
                configuration: 2,
                ..scope().epochs
            },
            ..scope()
        },
        Scope {
            epochs: Epochs {
                transport: 2,
                ..scope().epochs
            },
            ..scope()
        },
        Scope {
            epochs: Epochs {
                control: 2,
                ..scope().epochs
            },
            ..scope()
        },
    ] {
        assert!(
            evaluate(
                Condition::MotorState(p.clone()),
                &[r.clone()],
                Some(changed),
                11
            )
            .actions
            .is_empty()
        );
    }
}
#[test]
fn unknown_motor_guard_remains_unknown_across_decisive_boolean_operands_and_negation() {
    let motor = Condition::MotorState(predicate(State::Stopped));
    let t = Condition::Equal(
        Expr::Literal(Value::Bool(true)),
        Expr::Literal(Value::Bool(true)),
    );
    let f = Condition::Not(Box::new(t.clone()));
    for condition in [
        motor.clone(),
        Condition::Not(Box::new(motor.clone())),
        Condition::All(vec![f.clone(), motor.clone()]),
        Condition::Any(vec![t.clone(), motor.clone()]),
    ] {
        let p = evaluate(condition, &[], Some(scope()), 10);
        assert!(p.actions.is_empty());
        assert_eq!(p.unknown_branches, vec!["guard"]);
    }
    assert_eq!(
        evaluate(
            Condition::All(vec![
                f,
                Condition::Unknown(Expr::Literal(Value::Bool(true)))
            ]),
            &[],
            Some(scope()),
            10
        )
        .commands[0]
            .command,
        Command::Switch(false),
        "old non-motor Boolean semantics unchanged"
    );
}
#[test]
fn unresolved_logical_motor_actions_fail_before_any_physical_plan() {
    let action = Node {
        source: "motor-source".into(),
        action: Action::Motor {
            device: DeviceId::new("motor").unwrap(),
            action: homesense_core::MotorAction::Open,
            mapping_review: "synthetic".into(),
        },
    };
    assert!(matches!(
        rule::evaluate(&[action], &Default::default(), Budget::default()),
        Err(rule::Error::UnresolvedMotorAction)
    ));
}
