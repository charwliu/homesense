use homesense_core::{
    Command, DeviceId, deferred_plan,
    rule::{self, Action, Budget, Node},
    scene::Step,
    scene_expand::{self, Definition},
};
use std::collections::{BTreeMap, BTreeSet};
fn call(id: &str, on: bool) -> Node {
    Node {
        source: "qualified/original/call".into(),
        action: Action::Scene {
            id: id.into(),
            activate: on,
        },
    }
}
#[test]
fn deferred_false_requires_an_explicit_complete_target_and_budget() {
    let source = Step {
        device: DeviceId::new("source-only").unwrap(),
        command: Command::Switch(true),
        delay_ms: 0,
        source: "source-member".into(),
    };
    let target = (0..36)
        .map(|i| Step {
            device: DeviceId::new(format!("target-{i}")).unwrap(),
            command: Command::Switch(false),
            delay_ms: 10 + i,
            source: format!("target-member-{i}"),
        })
        .collect::<Vec<_>>();
    let defs = BTreeMap::from([
        ("source".into(), vec![source]),
        ("target".into(), target.clone()),
    ]);
    let links = BTreeMap::from([("source".into(), "target".into())]);
    let plan = || {
        rule::evaluate(
            &[call("source", false)],
            &BTreeMap::new(),
            Budget::default(),
        )
        .unwrap()
    };
    assert_eq!(
        deferred_plan::reserve(plan(), &defs, 100).unwrap_err(),
        deferred_plan::Error::UnknownDeactivation("source".into())
    );
    assert_eq!(
        deferred_plan::reserve_with_deactivations(plan(), &defs, &links, 36).unwrap_err(),
        deferred_plan::Error::Budget
    );
    let out = deferred_plan::reserve_with_deactivations(plan(), &defs, &links, 37).unwrap();
    assert_eq!(out.deactivated_scenes, BTreeSet::from(["source".into()]));
    assert_eq!(out.invocations.len(), 1);
    assert_eq!(out.invocations[0].definition, "target");
    assert_eq!(out.invocations[0].steps.len(), 36);
    for (i, step) in out.invocations[0].steps.iter().enumerate() {
        assert_eq!(step.device, target[i].device);
        assert_eq!(step.delay_ms, target[i].delay_ms);
        assert_eq!(step.command, Command::Switch(false));
        assert!(step.source.ends_with(&format!("target-member-{i}")));
    }
}
#[test]
fn immediate_and_timed_source_target_pairs_keep_guard_and_target_vector() {
    for source_timed in [false, true] {
        for target_timed in [false, true] {
            let output = Node {
                source: "target/member".into(),
                action: Action::Command {
                    device: DeviceId::new("target-only").unwrap(),
                    command: Command::Switch(false),
                },
            };
            let mut immediate = BTreeMap::new();
            let mut deferred = BTreeSet::new();
            for (id, timed) in [("source", source_timed), ("target", target_timed)] {
                if timed {
                    deferred.insert(id.into());
                } else {
                    immediate.insert(
                        id.into(),
                        Definition {
                            activate: vec![output.clone()],
                            deactivate: None,
                        },
                    );
                }
            }
            let out = scene_expand::expand_with_deactivations(
                &[call("source", false)],
                &immediate,
                &deferred,
                &BTreeMap::from([("source".into(), "target".into())]),
                100,
            )
            .unwrap();
            assert_eq!(out.len(), 2);
            assert!(matches!(&out[0].action,Action::RequireQuiescence{scene} if scene=="source"));
            if target_timed {
                assert!(matches!(&out[1].action,Action::Scene{id,activate:true} if id=="target"));
            } else {
                assert!(
                    matches!(&out[1].action,Action::Command{device,..} if device.as_str()=="target-only")
                );
            }
            assert!(out[1].source.contains("deactivation-target:target"));
        }
    }
}
