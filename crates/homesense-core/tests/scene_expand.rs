use homesense_core::{
    Command, DeviceId,
    rule::{Action, Node},
    scene_expand::*,
};
fn call(id: &str, on: bool) -> Node {
    Node {
        source: "caller".into(),
        action: Action::Scene {
            id: id.into(),
            activate: on,
        },
    }
}
fn output(id: &str) -> Node {
    Node {
        source: id.into(),
        action: Action::Command {
            device: DeviceId::new(id).unwrap(),
            command: Command::Switch(true),
        },
    }
}
#[test]
fn nested_order_sources_and_explicit_deactivation() {
    let scenes = [
        (
            "a".into(),
            Definition {
                activate: vec![output("first"), call("b", true), output("last")],
                deactivate: None,
            },
        ),
        (
            "b".into(),
            Definition {
                activate: vec![output("middle")],
                deactivate: Some(vec![]),
            },
        ),
    ]
    .into();
    let out = expand(&[call("a", true)], &scenes, 20).unwrap();
    assert_eq!(
        out.iter().map(|n| n.source.as_str()).collect::<Vec<_>>(),
        vec![
            "caller -> first",
            "caller -> caller -> middle",
            "caller -> last"
        ]
    );
    assert!(matches!(
        expand(&[call("a", false)], &scenes, 20),
        Err(Error::UnknownDeactivation(_))
    ));
    assert!(expand(&[call("b", false)], &scenes, 20).unwrap().is_empty());
}
#[test]
fn cycles_missing_references_and_expansion_budgets_reject_whole_plan() {
    let scenes = [
        (
            "a".into(),
            Definition {
                activate: vec![call("b", true)],
                deactivate: None,
            },
        ),
        (
            "b".into(),
            Definition {
                activate: vec![call("a", true)],
                deactivate: None,
            },
        ),
    ]
    .into();
    assert!(matches!(
        expand(&[call("a", true)], &scenes, 20),
        Err(Error::Cycle(_))
    ));
    assert!(matches!(
        expand(&[call("missing", true)], &scenes, 20),
        Err(Error::MissingScene(_))
    ));
    assert_eq!(
        expand(&[output("a"), output("b")], &scenes, 1).err(),
        Some(Error::Budget)
    );
}
