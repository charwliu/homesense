//! Explicit first-unknown activation confirmation. Never a variable default or
//! a policy changing the meaning of ordinary Set/Unknown expressions.
use crate::{
    Configuration, Error, invalid,
    rule_document::{Document, SceneExecutionKind, Trigger},
};
use homesense_core::{
    DeviceId, ModuleKind,
    rule::{Lifetime, Value, ValueType},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FirstValue {
    Bool(bool),
    Integer(i64),
}
impl FirstValue {
    pub fn value(&self) -> Value {
        match self {
            Self::Bool(v) => Value::Bool(*v),
            Self::Integer(v) => Value::Integer(*v),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Postcondition {
    pub device: String,
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    #[default]
    Physical,
    LogicalSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: String,
    pub variable: String,
    pub scenes: Vec<String>,
    pub first_value: FirstValue,
    #[serde(default)]
    pub basis: Basis,
    /// Explicit companion mutation; active only with a primary Unknown-first.
    #[serde(default)]
    pub include_when_first_unknown: bool,
    #[serde(default)]
    pub confirm_when_known: bool,
    #[serde(default)]
    pub shared_selection_group: Option<String>,
    /// Exact already-evaluated persistent values selecting this physical vector.
    #[serde(default)]
    pub when_values: BTreeMap<String, FirstValue>,
    #[serde(default)]
    pub when_initial_unknown: Vec<String>,
    pub timeout_ms: u64,
    pub max_age_ms: u64,
    pub review_evidence: String,
    #[serde(default)]
    pub postconditions: Vec<Postcondition>,
}
pub(crate) fn validate(
    policies: &[Policy],
    document: &Document,
    config: &Configuration,
) -> Result<(), Error> {
    if policies.len() > 512
        || policies
            .iter()
            .map(|p| p.variable.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            > 64
    {
        return Err(invalid("component mode policy capacity exceeded"));
    }
    let mut ids = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    for p in policies {
        crate::identifier(&p.id)?;
        crate::identifier(&p.variable)?;
        let variable = document
            .variables
            .get(&p.variable)
            .ok_or_else(|| invalid("component variable missing"))?;
        let kind = match p.first_value {
            FirstValue::Bool(_) => ValueType::Bool,
            FirstValue::Integer(_) => ValueType::Integer,
        };
        if !ids.insert(&p.id)||variable.lifetime!=Lifetime::Persistent||variable.value_type!=kind||p.review_evidence.trim().is_empty()||p.review_evidence.len()>4096
            ||p.timeout_ms==0||p.timeout_ms>60000||p.max_age_ms==0||p.max_age_ms>config.refresh_policy().observation_max_age_ms()
            ||p.scenes.is_empty()||p.scenes.len()>64||p.postconditions.len()>32
            ||(p.basis==Basis::Physical&&p.postconditions.is_empty())
            ||(p.basis==Basis::LogicalSource&&(!p.postconditions.is_empty()||!p.confirm_when_known||p.include_when_first_unknown||p.when_values.is_empty()||p.when_values.contains_key(&p.variable)||!p.when_initial_unknown.is_empty()||p.shared_selection_group.is_some()))
            ||document.scene_switch_groups.iter().any(|g|g.variable==p.variable&&p.shared_selection_group.as_deref()!=Some(g.id.as_str()))
            ||p.shared_selection_group.as_ref().is_some_and(|id|document.scene_switch_groups.iter().all(|g|g.id!=*id||g.variable!=p.variable||!matches!(p.first_value,FirstValue::Integer(v) if v==0||g.members.iter().any(|m|m.value==v)||g.unselected_values.contains(&v))))
            ||(p.include_when_first_unknown&&p.confirm_when_known)
            ||document.rules.iter().any(|r|matches!(&r.trigger,Trigger::VariableChanged{variable} if *variable==p.variable)) {
            return Err(invalid("invalid, conflicting or unsupported component confirmation policy"));
        }
        if p.when_values.len() > 16
            || p.when_values.iter().any(|(name, value)| {
                document.variables.get(name).is_none_or(|v| {
                    v.lifetime != Lifetime::Persistent
                        || !matches!(
                            (v.value_type, value),
                            (ValueType::Bool, FirstValue::Bool(_))
                                | (ValueType::Integer, FirstValue::Integer(_))
                        )
                })
            })
        {
            return Err(invalid(
                "component outcome selector requires declared persistent typed values",
            ));
        }
        if p.when_initial_unknown.len() > 16
            || p.when_initial_unknown.iter().collect::<BTreeSet<_>>().len()
                != p.when_initial_unknown.len()
            || p.when_initial_unknown.iter().any(|name| {
                document
                    .variables
                    .get(name)
                    .is_none_or(|v| v.lifetime != Lifetime::Persistent)
            })
        {
            return Err(invalid(
                "initial unknown scope needs distinct persistent variables",
            ));
        }
        for scene in &p.scenes {
            if !pairs.insert((
                p.variable.clone(),
                scene.clone(),
                p.first_value.clone(),
                p.when_values.clone(),
                p.basis,
                p.when_initial_unknown
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )) || document.scene_execution_kind(scene) != Some(SceneExecutionKind::Immediate)
                || !document.scene_action_available(scene)
            {
                return Err(invalid(
                    "component confirmation needs unique reviewed manual immediate scene",
                ));
            }
            // Deferred timelines require their own retained causal continuation;
            // do not silently confirm before their future outputs exist.
            fn has_deferred(nodes: &[homesense_core::rule::Node]) -> bool {
                nodes.iter().any(|n| match &n.action {
                    homesense_core::rule::Action::Scene { .. } => true,
                    homesense_core::rule::Action::If {
                        then_actions,
                        else_actions,
                        ..
                    } => has_deferred(then_actions) || has_deferred(else_actions),
                    _ => false,
                })
            }
            if has_deferred(&document.linked_manual_scenes(std::slice::from_ref(scene))?) {
                return Err(invalid(
                    "component confirmation cannot own future timelines",
                ));
            }
        }
        let mut targets = BTreeSet::new();
        for condition in &p.postconditions {
            let id = DeviceId::new(&condition.device)?;
            if !targets.insert(id.clone())
                || config.binding(&id).is_none_or(|(_, m)| {
                    !matches!(m.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
                        || condition.level.is_some_and(|level| {
                            !condition.on
                                || !(1..=100).contains(&level)
                                || m.profile() != homesense_core::ProtocolProfile::Ad8Din
                        })
                })
            {
                return Err(invalid(
                    "component postcondition requires distinct enabled logical lighting",
                ));
            }
        }
    }
    Ok(())
}
