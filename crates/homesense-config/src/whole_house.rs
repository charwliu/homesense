//! Absolute household aliases must exactly equal the separately reviewed Away scope.
use crate::{Configuration, Error, invalid, rule_document::{Document, SceneExecutionKind, SceneScope}};
use homesense_core::{Command, LevelTransition, ModuleKind, rule::Action};
use std::collections::BTreeSet;
pub(crate) fn validate(document: &Document, config: &Configuration) -> Result<(), Error> {
    let Some(policy) = &document.away_off else { return Ok(()); };
    let (on, off) = match (&policy.whole_house_on_scene, &policy.whole_house_off_scene) {
        (None, None) => return Ok(()),
        (Some(on), Some(off)) if policy.enabled && on != off => (on, off),
        _ => return Err(invalid("household scenes require two distinct aliases and enabled Away scope")),
    };
    let scope = policy.modules.iter().flat_map(|module|module.devices.iter()).cloned().collect::<BTreeSet<_>>();
    if config.devices().iter().any(|(id,d)|d.enabled && d.supply_relay.as_ref().is_some_and(|s|scope.contains(s.as_str())) && !scope.contains(id.as_str())) {
        return Err(invalid("household supply scope must include all logical owners"));
    }
    for (scene, on) in [(on, true), (off, false)] {
        if document.scene_execution_kind(scene) != Some(SceneExecutionKind::Immediate)
            || document.hidden_scene_entries.contains(scene) || document.scene_presentations.contains_key(scene)
            || document.scene_tracking.contains_key(scene) || document.scene_component_activity.contains_key(scene)
            || document.scene_scopes.get(scene)!=Some(&SceneScope::Household)
            || document.scene_switch_groups.iter().any(|g|g.members.iter().any(|m| &m.scene==scene)) {
            return Err(invalid("household alias must be a public standalone absolute immediate scene"));
        }
        let definition = &document.immediate_scenes[scene];
        if definition.deactivate.is_some() || definition.activate.len() != scope.len() {
            return Err(invalid("household scene must explicitly cover every reviewed output once"));
        }
        let mut seen = BTreeSet::new();
        for node in &definition.activate {
            let Action::Command {device, command} = &node.action else {return Err(invalid("household scene requires direct absolute commands"));};
            let (_, module) = config.binding(device).ok_or_else(||invalid("household output unavailable"))?;
            let correct = match (module.kind(), command) {
                (ModuleKind::Switch, Command::Switch(value)) => *value==on,
                (ModuleKind::Dimmer, Command::SetLevel{level,transition:LevelTransition::Immediate}) => level.get()==if on {100}else{0},
                _ => false,
            };
            if !correct || !scope.contains(device.as_str()) || !seen.insert(device.as_str().to_owned()) {
                return Err(invalid("household scene differs from its reviewed absolute module scope"));
            }
        }
    }
    // The managed shutdown may not be embedded as an ordinary AST action,
    // which would bypass its priority, confirmation and retry owner.
    fn references(nodes: &[homesense_core::rule::Node], target: &str) -> bool {
        nodes.iter().any(|node|match &node.action {
            Action::Scene{id,..}=>id==target,
            Action::If{then_actions,else_actions,..}=>references(then_actions,target)||references(else_actions,target),
            _=>false,
        })
    }
    if document.rules.iter().any(|rule|references(&rule.actions,off)||references(&rule.actions,on)) {
        return Err(invalid("managed household Off cannot be embedded in ordinary rules"));
    }
    for (id, definition) in &document.immediate_scenes {
        if id==off || id==on {continue;}
        if references(&definition.activate,off) || references(&definition.activate,on)
            || definition.deactivate.as_ref().is_some_and(|nodes|references(nodes,off)||references(nodes,on)) {
            return Err(invalid("managed household Off must use its dedicated entry"));
        }
    }
    Ok(())
}
