//! Bound explicit HAP component recipes to the reviewed source program.
//! This checks scope and operations, not semantic equivalence of its branches;
//! candidate qualification must still prove each absolute intent and peer mode.
use crate::{
    Configuration, Error, invalid, rule_document::Document, scene_component_activity::Policy,
    scene_tracking::HapAction,
};
use homesense_core::{
    Command, DeviceId, LevelTransition, ModuleKind,
    rule::{Action, Condition, Expr, Node},
};
use std::collections::BTreeSet;

#[derive(Default)]
struct Scope {
    commands: Vec<(DeviceId, Command)>,
    assignments: Vec<(String, Expr)>,
    variables: BTreeSet<String>,
    devices: BTreeSet<DeviceId>,
}
fn expression(e: &Expr, scope: &mut Scope) {
    match e {
        Expr::Variable(name) => {
            scope.variables.insert(name.clone());
        }
        Expr::DeviceOn(id) | Expr::DeviceLevel(id) => {
            scope.devices.insert(id.clone());
        }
        Expr::Add(left, right) | Expr::Subtract(left, right) => {
            expression(left, scope);
            expression(right, scope);
        }
        Expr::Literal(_) => {}
    }
}
fn condition(c: &Condition, scope: &mut Scope) -> bool {
    match c {
        Condition::Equal(a, b) | Condition::Less(a, b) => {
            expression(a, scope);
            expression(b, scope);
        }
        Condition::Not(c) => return condition(c, scope),
        Condition::All(cs) | Condition::Any(cs) => return cs.iter().all(|c| condition(c, scope)),
        Condition::Unknown(e) => expression(e, scope),
        Condition::MotorState(_) => return false,
    }
    true
}
fn collect(nodes: &[Node], scope: &mut Scope) -> bool {
    for n in nodes {
        match &n.action {
            Action::Command { device, command } => {
                scope.devices.insert(device.clone());
                scope.commands.push((device.clone(), *command));
            }
            Action::Set { variable, value } => {
                scope.variables.insert(variable.clone());
                expression(value, scope);
                scope.assignments.push((variable.clone(), value.clone()));
            }
            Action::If {
                condition: c,
                then_actions,
                else_actions,
            } => {
                if !condition(c, scope)
                    || !collect(then_actions, scope)
                    || !collect(else_actions, scope)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}
fn same_expr(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::Literal(a), Expr::Literal(b)) => a == b,
        (Expr::Variable(a), Expr::Variable(b)) => a == b,
        (Expr::DeviceOn(a), Expr::DeviceOn(b)) | (Expr::DeviceLevel(a), Expr::DeviceLevel(b)) => {
            a == b
        }
        (Expr::Add(a, b), Expr::Add(c, d)) | (Expr::Subtract(a, b), Expr::Subtract(c, d)) => {
            same_expr(a, c) && same_expr(b, d)
        }
        _ => false,
    }
}
pub(crate) fn validate(
    p: &Policy,
    a: &HapAction,
    doc: &Document,
    config: &Configuration,
) -> Result<(), Error> {
    let fail = || {
        invalid("component HAP recipes must retain reviewed immediate source operations and scope")
    };
    let source = doc.immediate_scenes.get(&p.scene).ok_or_else(fail)?;
    let mut original = Scope::default();
    if !collect(&source.activate, &mut original)
        || source
            .deactivate
            .as_ref()
            .is_some_and(|n| !collect(n, &mut original))
    {
        return Err(fail());
    }
    for id in [&a.on_scene, &a.off_scene] {
        let scene = doc.immediate_scenes.get(id).ok_or_else(fail)?;
        let mut scope = Scope::default();
        if !collect(&scene.activate, &mut scope)
            || !scope.variables.is_subset(&original.variables)
            || !scope.devices.is_subset(&original.devices)
            || scope.assignments.iter().any(|(name, value)| {
                !original
                    .assignments
                    .iter()
                    .any(|(n, v)| n == name && same_expr(v, value))
            })
            || scope
                .commands
                .iter()
                .any(|c| !original.commands.contains(c))
            || !scope
                .assignments
                .iter()
                .any(|(name, _)| name == &p.variable)
        {
            return Err(fail());
        }
        let mut lighting = false;
        for (id, command) in &scope.commands {
            let (_, module) = config.binding(id).ok_or_else(fail)?;
            match (module.kind(), command) {
                (ModuleKind::Switch, Command::Switch(_))
                | (
                    ModuleKind::Dimmer,
                    Command::SetLevel {
                        transition: LevelTransition::Immediate,
                        ..
                    },
                ) => lighting = true,
                (ModuleKind::Keypad, Command::PanelLed(_) | Command::PanelLedOff) => {}
                _ => return Err(fail()),
            }
        }
        if !lighting {
            return Err(fail());
        }
    }
    Ok(())
}
