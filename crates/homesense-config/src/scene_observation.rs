//! Reviewed observation-only key ownership. Disabled unless explicitly enabled.
use crate::{
    Configuration, Error, invalid,
    rule_document::{Holds, Rule, Trigger},
};
use homesense_core::{DeviceId, ModuleKind};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEdge {
    #[default]
    Pressed,
    Released,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub device: String,
    pub group: String,
    pub member: i64,
    pub review_evidence: Option<String>,
    #[serde(default)]
    pub trigger_edge: TriggerEdge,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    pub debounce_ms: u64,
    pub max_age_ms: u64,
    pub bindings: Vec<Binding>,
}
impl Settings {
    /// Expanded rules must not turn an unknown/inferred selection into an
    /// automatic predicate or blindly write its masked read set back. Explicit
    /// literal selection assignments remain local intents; UI/manual planning
    /// and the independent indicator synchronizer use their own trusted paths.
    pub(crate) fn validate_linked(
        &self,
        document: &crate::rule_document::Document,
    ) -> Result<(), Error> {
        use homesense_core::rule::{Action, Condition, Expr, Node};
        if !self.enabled {
            return Ok(());
        }
        let variables: BTreeSet<_> = self
            .bindings
            .iter()
            .filter_map(|binding| {
                document
                    .scene_switch_groups
                    .iter()
                    .find(|g| g.id == binding.group)
                    .map(|g| g.variable.as_str())
            })
            .collect();
        fn expression(expr: &Expr, variables: &BTreeSet<&str>) -> bool {
            match expr {
                Expr::Variable(name) => variables.contains(name.as_str()),
                Expr::Add(a, b) | Expr::Subtract(a, b) => {
                    expression(a, variables) || expression(b, variables)
                }
                _ => false,
            }
        }
        fn condition(test: &Condition, variables: &BTreeSet<&str>) -> bool {
            match test {
                Condition::Equal(a, b) | Condition::Less(a, b) => {
                    expression(a, variables) || expression(b, variables)
                }
                Condition::Not(test) => condition(test, variables),
                Condition::All(tests) | Condition::Any(tests) => {
                    tests.iter().any(|t| condition(t, variables))
                }
                Condition::Unknown(expr) => expression(expr, variables),
            }
        }
        fn reads(nodes: &[Node], variables: &BTreeSet<&str>) -> bool {
            nodes.iter().any(|node| match &node.action {
                Action::Set { value, .. } => expression(value, variables),
                Action::If {
                    condition: test,
                    then_actions,
                    else_actions,
                } => {
                    condition(test, variables)
                        || reads(then_actions, variables)
                        || reads(else_actions, variables)
                }
                _ => false,
            })
        }
        if document
            .linked_rules_with_deferred()?
            .iter()
            .any(|rule| reads(&rule.actions, &variables))
        {
            return Err(invalid(
                "observed scene selections cannot feed automatic rule predicates",
            ));
        }
        Ok(())
    }
    pub(crate) fn validate(
        &self,
        configuration: &Configuration,
        groups: &[crate::scene_switches::Group],
        rules: &[Rule],
        holds: Option<&Holds>,
    ) -> Result<(), Error> {
        if !(30..=2000).contains(&self.debounce_ms)
            || !(self.debounce_ms..=30000).contains(&self.max_age_ms)
            || self.bindings.is_empty()
            || self.bindings.len() > 256
        {
            return Err(invalid("invalid scene observation bounds"));
        }
        let mut devices = BTreeSet::new();
        let mut observed_variables = BTreeSet::new();
        for binding in &self.bindings {
            let id = DeviceId::new(&binding.device)?;
            if binding.device.len() > 256
                || binding.device.chars().any(char::is_control)
                || !devices.insert(binding.device.as_str())
            {
                return Err(invalid("duplicate or invalid scene observation key"));
            }
            let group = groups
                .iter()
                .find(|g| g.id == binding.group)
                .ok_or_else(|| invalid("scene observation group missing"))?;
            if group.members.len() > 32 || !group.members.iter().any(|m| m.value == binding.member)
            {
                return Err(invalid(
                    "scene observation member missing or capacity exceeded",
                ));
            }
            if self.enabled {
                if binding.review_evidence.as_ref().is_none_or(|v| {
                    v.trim().is_empty() || v.len() > 8192 || v.chars().any(char::is_control)
                }) {
                    return Err(invalid(
                        "enabled scene observation key needs review evidence",
                    ));
                }
                let (_, module) = configuration
                    .binding(&id)
                    .ok_or_else(|| invalid("enabled scene observation key must be enabled"))?;
                if module.kind() != ModuleKind::Keypad {
                    return Err(invalid("scene observation requires a verified keypad"));
                }
                observed_variables.insert(group.variable.as_str());
            }
        }
        if self.enabled
            && (rules
                .iter()
                .filter(|r| r.enabled)
                .any(|r| match &r.trigger {
                    Trigger::Panel { device, .. } => devices.contains(device.as_str()),
                    Trigger::VariableChanged { variable } => {
                        observed_variables.contains(variable.as_str())
                    }
                    _ => false,
                })
                || holds.is_some_and(|h| {
                    h.bindings
                        .iter()
                        .any(|b| b.enabled && devices.contains(b.key.as_str()))
                }))
        {
            return Err(invalid(
                "scene observation cannot own an executing panel key or trigger variable automation",
            ));
        }
        Ok(())
    }
}
