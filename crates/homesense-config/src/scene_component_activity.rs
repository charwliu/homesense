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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub scene: String,
    pub enabled: bool,
    pub variable: String,
    pub active_value: FirstValue,
    pub activity_name: String,
    pub review_evidence: String,
}

pub(crate) fn validate(
    policies: &[Policy],
    document: &Document,
    _config: &Configuration,
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
    }
    Ok(())
}
