use homesense_core::{
    Command, DeviceId,
    rule::{Action, Budget, Node},
    rule_chain::{Chain, Error},
};
fn command() -> Node {
    Node {
        source: "fixture".into(),
        action: Action::Command {
            device: DeviceId::new("lamp").unwrap(),
            command: Command::Switch(true),
        },
    }
}
fn chain(id: u128, commands: usize) -> Chain {
    Chain::new(
        id,
        0,
        1000,
        Budget {
            commands,
            ..Budget::default()
        },
        8,
    )
    .unwrap()
}
#[test]
fn nested_causes_keep_correlation_and_cycles_stop_chain() {
    let mut c = chain(1, 8);
    let root = c.root();
    let a = c
        .plan(&root, "a", &[command()], &Default::default(), 0)
        .unwrap();
    let b = c
        .plan(&a.cause, "b", &[command()], &Default::default(), 1)
        .unwrap();
    assert_eq!(b.causation_id, a.cause.event_id());
    assert_eq!(b.cause.correlation_id(), 1);
    assert!(matches!(
        c.plan(&b.cause, "a", &[], &Default::default(), 2),
        Err(Error::Cycle)
    ));
    assert!(matches!(
        c.plan(&root, "c", &[], &Default::default(), 3),
        Err(Error::Stopped)
    ));
}
#[test]
fn sibling_rules_share_action_and_dispatch_budget() {
    let mut c = chain(1, 1);
    let root = c.root();
    c.plan(&root, "a", &[command()], &Default::default(), 0)
        .unwrap();
    assert!(matches!(
        c.plan(&root, "b", &[command()], &Default::default(), 1),
        Err(Error::Rule(_))
    ));
    let mut c = Chain::new(
        2,
        0,
        1000,
        Budget {
            nodes: 2,
            ..Budget::default()
        },
        8,
    )
    .unwrap();
    let root = c.root();
    for id in ["a", "b"] {
        c.plan(&root, id, &[], &Default::default(), 0).unwrap();
    }
    assert!(matches!(
        c.plan(&root, "c", &[], &Default::default(), 0),
        Err(Error::Budget)
    ));
}
#[test]
fn elapsed_deadline_disconnect_and_wrong_chain_are_rejected() {
    let mut c = chain(1, 8);
    let foreign = chain(2, 8).root();
    assert!(matches!(
        c.plan(&foreign, "a", &[], &Default::default(), 0),
        Err(Error::ForeignCause)
    ));
    let root = c.root();
    assert!(matches!(
        c.plan(&root, "a", &[], &Default::default(), 1000),
        Err(Error::Budget)
    ));
    let mut c = chain(3, 8);
    let root = c.root();
    c.cancel();
    assert!(matches!(
        c.plan(&root, "a", &[], &Default::default(), 1),
        Err(Error::Stopped)
    ));
}
