//! Read-only scene activity backed by a live confirmed component owner.
//! A saved variable or a logical-source label alone is never evidence.
use crate::{
    Configuration, Error,
    component_modes::{Basis, FirstValue},
    invalid,
    rule_document::Document,
};
use homesense_core::rule::{Lifetime, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub scene: String,
    pub enabled: bool,
    pub variable: String,
    pub active_value: FirstValue,
    pub activity_name: String,
    pub review_evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_confirmation: Option<ProgramConfirmation>,
}

/// Source identity plus the complete reviewed positive component shared by
/// every first-activation vector. Neither is sufficient on its own.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramConfirmation {
    pub active_conditions: Vec<crate::component_modes::Postcondition>,
    pub known_values: Vec<FirstValue>,
    pub timeout_ms: u64,
    pub max_age_ms: u64,
    pub review_evidence: String,
}

pub(crate) fn validate(
    policies: &[Policy],
    document: &Document,
    config: &Configuration,
) -> Result<(), Error> {
    if policies.len() > 128 {
        return Err(invalid("component activity capacity exceeded"));
    }
    let mut scenes = BTreeSet::new();
    for p in policies {
        crate::identifier(&p.scene)?;
        crate::identifier(&p.variable)?;
        let variable = document
            .variables
            .get(&p.variable)
            .ok_or_else(|| invalid("component activity variable missing"))?;
        if !scenes.insert(&p.scene)
            || variable.lifetime != Lifetime::Persistent
            || !matches!(
                (variable.value_type, &p.active_value),
                (ValueType::Bool, FirstValue::Bool(_))
                    | (ValueType::Integer, FirstValue::Integer(_))
            )
            || p.activity_name.trim().is_empty()
            || p.activity_name.len() > 64
            || p.activity_name.chars().any(char::is_control)
            || p.review_evidence.trim().is_empty()
            || p.review_evidence.len() > 8192
            || !document.immediate_scenes.contains_key(&p.scene)
            || p.enabled && !document.scene_action_available(&p.scene)
            || document
                .scene_tracking
                .get(&p.scene)
                .is_some_and(|v| v.enabled)
            || document.scene_presentations.contains_key(&p.scene)
            || document.scene_switch_groups.iter().any(|g| {
                g.members
                    .iter()
                    .any(|m| m.scene == p.scene || m.off_scene == p.scene)
                    || g.reset_scenes.contains(&p.scene)
            })
            || !document.component_mode_confirmations.iter().any(|c| {
                c.basis == Basis::Physical
                    && c.variable == p.variable
                    && c.first_value == p.active_value
                    && c.scenes.contains(&p.scene)
            })
        {
            return Err(invalid(
                "component activity requires a unique reviewed physical component provider",
            ));
        }
        if let Some(program) = &p.program_confirmation {
            let candidates = document.component_mode_confirmations.iter().filter(|c| {
                c.basis == Basis::Physical
                    && c.variable == p.variable
                    && c.first_value == p.active_value
                    && c.scenes.contains(&p.scene)
            });
            let mut common: Option<
                std::collections::BTreeMap<String, crate::component_modes::Postcondition>,
            > = None;
            for candidate in candidates {
                let positive = candidate
                    .postconditions
                    .iter()
                    .filter(|c| c.on)
                    .map(|c| (c.device.clone(), c.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>();
                match &mut common {
                    None => common = Some(positive),
                    Some(common) => common.retain(|id, c| positive.get(id) == Some(c)),
                }
            }
            let declared = program
                .active_conditions
                .iter()
                .map(|c| (c.device.clone(), c.clone()))
                .collect::<std::collections::BTreeMap<_, _>>();
            let values = program.known_values.iter().collect::<BTreeSet<_>>();
            if declared.is_empty()
                || declared.len() > 32
                || declared.len() != program.active_conditions.len()
                || Some(&declared) != common.as_ref()
                || program.timeout_ms == 0
                || program.timeout_ms > 15_000
                || program.max_age_ms == 0
                || program.max_age_ms > config.refresh_policy().observation_max_age_ms()
                || program.review_evidence.trim().is_empty()
                || program.review_evidence.len() > 8192
                || values.is_empty()
                || values.len() > 64
                || values.len() != program.known_values.len()
                || !values.contains(&p.active_value)
                || values.iter().any(|value| {
                    !matches!(
                        (variable.value_type, value),
                        (ValueType::Bool, FirstValue::Bool(_))
                            | (ValueType::Integer, FirstValue::Integer(_))
                    )
                })
            {
                return Err(invalid(
                    "program activity needs a complete reviewed common component and typed outcomes",
                ));
            }
        }
    }
    Ok(())
}
