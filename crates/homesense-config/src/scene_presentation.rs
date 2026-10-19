//! Explicit public action/selection pairing. Internal profiles never become a
//! second user action or an authorization alias for another lighting scope.
use crate::{Configuration, Error, invalid, rule_document::Document, scene_switches::LightTarget};
use homesense_core::{
    Command, DeviceId, ModuleKind,
    rule::{Action, Expr, Node, Value},
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub scene: String,
    pub selection_scene: String,
    pub review_evidence: String,
}

fn vector(
    nodes: &[Node],
    config: &Configuration,
    variable: &str,
    value: i64,
) -> Result<BTreeMap<DeviceId, LightTarget>, Error> {
    let mut targets = BTreeMap::new();
    let mut assigned = None;
    for node in nodes {
        match &node.action {
            Action::Command { device, command } => {
                let (_, module) = config
                    .binding(device)
                    .ok_or_else(|| invalid("presentation output missing"))?;
                match (module.kind(), command) {
                    (ModuleKind::Switch, Command::Switch(on)) => {
                        targets.insert(device.clone(), LightTarget::Switch(*on));
                    }
                    (ModuleKind::Dimmer, Command::SetLevel { level, .. }) => {
                        targets.insert(device.clone(), LightTarget::Level(level.get()));
                    }
                    (ModuleKind::Keypad, Command::PanelLed(_) | Command::PanelLedOff) => {}
                    _ => {
                        return Err(invalid(
                            "presentation requires explicit lighting and LED commands",
                        ));
                    }
                }
            }
            Action::Set {
                variable: name,
                value: Expr::Literal(Value::Integer(v)),
            } if name == variable => assigned = Some(*v),
            _ => {
                return Err(invalid(
                    "presentation requires flat absolute On/Off and its own selection",
                ));
            }
        }
    }
    if assigned != Some(value) || targets.is_empty() {
        return Err(invalid("presentation outcome or lighting vector missing"));
    }
    Ok(targets)
}

pub(crate) fn validate(
    presentations: &[Presentation],
    doc: &Document,
    config: &Configuration,
) -> Result<(), Error> {
    if presentations.len() > 128 || presentations.len() != doc.scene_presentations.len() {
        return Err(invalid("duplicate/excessive scene presentations"));
    }
    let mut providers = BTreeSet::new();
    for p in presentations {
        crate::identifier(&p.scene)?;
        crate::identifier(&p.selection_scene)?;
        if p.scene == p.selection_scene
            || p.review_evidence.trim().is_empty()
            || p.review_evidence.len() > 4096
            || !providers.insert(&p.selection_scene)
            || doc.scene_presentations.contains_key(&p.selection_scene)
            || doc.disabled_immediate_scenes.contains(&p.scene)
            || doc.scene_rooms.get(&p.scene) != doc.scene_rooms.get(&p.selection_scene)
            || doc.scene_scopes.get(&p.scene).copied().unwrap_or(crate::rule_document::SceneScope::Unassigned)
                != doc.scene_scopes.get(&p.selection_scene).copied().unwrap_or(crate::rule_document::SceneScope::Unassigned)
        {
            return Err(invalid("invalid, chained or cross-room scene presentation"));
        }
        let scene = doc
            .immediate_scenes
            .get(&p.scene)
            .ok_or_else(|| invalid("presentation action must be immediate"))?;
        let (group, member) = doc
            .scene_switch_groups
            .iter()
            .find_map(|g| {
                g.members
                    .iter()
                    .find(|m| m.scene == p.selection_scene)
                    .map(|m| (g, m))
            })
            .ok_or_else(|| invalid("presentation selection must be a native member"))?;
        let expected = group
            .light_targets(&doc.timed_scenes)?
            .remove(&member.value)
            .ok_or_else(|| invalid("presentation member targets missing"))?;
        if !doc
            .timed_scenes
            .get(&p.selection_scene)
            .is_some_and(|s| s.enabled)
        {
            return Err(invalid("presentation profile disabled"));
        }
        let off_nodes = scene
            .deactivate
            .as_ref()
            .ok_or_else(|| invalid("presentation explicit Off missing"))?;
        let outputs = |nodes: &[Node]| {
            nodes
                .iter()
                .filter_map(|n| match &n.action {
                    Action::Command { device, .. } => Some(device.clone()),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        };
        if outputs(&scene.activate) != outputs(off_nodes) {
            return Err(invalid(
                "presentation On and Off must have the same output scope, including LEDs",
            ));
        }
        let actual = vector(&scene.activate, config, &group.variable, member.value)?;
        let off = vector(off_nodes, config, &group.variable, 0)?;
        if actual != expected || off.keys().ne(expected.keys()) || off.values().any(|v| !v.is_off())
        {
            return Err(invalid(
                "presentation On/Off differs from native lighting scope",
            ));
        }
        for (value, targets) in [(member.value, &actual), (0, &off)] {
            let confirmed = doc.component_mode_confirmations.iter().any(|policy| {
                policy.scenes.contains(&p.scene)
                    && policy.variable == group.variable
                    && policy.shared_selection_group.as_ref() == Some(&group.id)
                    && policy.basis == crate::component_modes::Basis::Physical
                    && policy.confirm_when_known
                    && policy.when_initial_unknown.is_empty()
                    && !policy.include_when_first_unknown
                    && policy.first_value == crate::component_modes::FirstValue::Integer(value)
                    && policy.when_values
                        == [(
                            group.variable.clone(),
                            crate::component_modes::FirstValue::Integer(value),
                        )]
                        .into()
                    && policy.postconditions.len() == targets.len()
                    && policy.postconditions.iter().all(|condition| {
                        DeviceId::new(&condition.device)
                            .ok()
                            .and_then(|id| targets.get(&id))
                            .is_some_and(|target| {
                                condition.on == !target.is_off()
                                    && condition.level.is_none_or(
                                        |level| matches!(target,LightTarget::Level(v)if *v==level),
                                    )
                            })
                    })
            });
            if !confirmed {
                return Err(invalid(
                    "presentation requires source-owned Written and feedback confirmation for On and Off",
                ));
            }
        }
        if config.keypads().values().flat_map(|p|p.keys.values()).any(|k|matches!(&k.action,Some(crate::keypad::KeyAction::SceneToggle{scene,..}|crate::keypad::KeyAction::SceneExecute{scene,..})if scene==&p.selection_scene)){
            return Err(invalid("keypad must use the public action, not its internal profile"));
        }
    }
    Ok(())
}

impl Presentation {
    /// A keypad press is a guarded rule decision; ordinary public On/Off keeps
    /// the source definition's absolute branches. Unknown explicitly selects On.
    pub fn toggle_nodes(&self, doc: &Document) -> Result<Vec<Node>, Error> {
        let (group, member) = doc
            .scene_switch_groups
            .iter()
            .find_map(|g| {
                g.members
                    .iter()
                    .find(|m| m.scene == self.selection_scene)
                    .map(|m| (g, m))
            })
            .ok_or_else(|| invalid("presentation member missing"))?;
        let call = |activate| {
            vec![Node {
                source: format!(
                    "presentation/{}/{}",
                    self.scene,
                    if activate { "on" } else { "off" }
                ),
                action: Action::Scene {
                    id: self.scene.clone(),
                    activate,
                },
            }]
        };
        Ok(vec![Node {
            source: format!("presentation/{}/unknown", self.scene),
            action: Action::If {
                condition: homesense_core::rule::Condition::Unknown(Expr::Variable(
                    group.variable.clone(),
                )),
                then_actions: call(true),
                else_actions: vec![Node {
                    source: format!("presentation/{}/selected", self.scene),
                    action: Action::If {
                        condition: homesense_core::rule::Condition::Equal(
                            Expr::Variable(group.variable.clone()),
                            Expr::Literal(Value::Integer(member.value)),
                        ),
                        then_actions: call(false),
                        else_actions: call(true),
                    },
                }],
            },
        }])
    }
}
