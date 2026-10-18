use std::collections::{BTreeMap,BTreeSet};
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

#[test]
fn deferred_activation_preserves_order_source_and_requires_explicit_link() {
    let scenes=[("parent".into(),Definition {activate:vec![output("first"),call("later",true),output("last")],deactivate:None})].into();
    let deferred=["later".into()].into();
    let result=expand_with_deferred(&[call("parent",true)],&scenes,&deferred,4).unwrap();
    assert_eq!(result.iter().map(|n|n.source.as_str()).collect::<Vec<_>>(),vec!["caller -> first","caller -> caller","caller -> last"]);
    assert!(matches!(&result[1].action,Action::Scene{id,activate:true} if id=="later"));
    assert_eq!(expand_with_deferred(&[call("parent",true)],&scenes,&deferred,3).err(),Some(Error::Budget));
    assert_eq!(expand(&[call("parent",true)],&scenes,4).err(),Some(Error::MissingScene("later".into())));
    assert_eq!(expand_with_deferred(&[call("later",false)],&scenes,&deferred,4).err(),Some(Error::UnknownDeactivation("later".into())));
    assert_eq!(expand_with_deferred(&[],&scenes,&["parent".into()].into(),4).err(),Some(Error::AmbiguousScene("parent".into())));
}

#[test]
fn borrowed_definitions_keep_nested_sources_and_global_ambiguity_guards() {
    let definitions:BTreeMap<String,Definition>=[("outer".into(),Definition{activate:vec![output("before"),call("inner",true)],deactivate:None}),("inner".into(),Definition{activate:vec![output("after")],deactivate:None})].into();
    let borrowed=definitions.iter().map(|(id,scene)|(id.clone(),scene)).collect();
    let expanded=expand_borrowed_with_deactivations(&[call("outer",true)],&borrowed,&BTreeSet::new(),&BTreeMap::new(),16).unwrap();
    assert_eq!(expanded.iter().map(|n|n.source.as_str()).collect::<Vec<_>>(),vec!["caller -> before","caller -> caller -> after"]);
    assert!(matches!(expand_borrowed_with_deactivations(&[call("outer",true)],&borrowed,&["inner".into()].into(),&BTreeMap::new(),16),Err(Error::AmbiguousScene(id))if id=="inner"));
    assert!(matches!(expand_borrowed_with_deactivations(&[call("outer",true)],&borrowed,&BTreeSet::new(),&BTreeMap::new(),1),Err(Error::Budget)));
    assert!(matches!(expand_borrowed_with_deactivations(&[call("inner",false)],&borrowed,&BTreeSet::new(),&BTreeMap::new(),16),Err(Error::UnknownDeactivation(id))if id=="inner"));
}
