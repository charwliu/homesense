//! Explicit, reviewed latching scene groups. Zero means no local selection;
//! an unknown value stays unknown and is never inferred from a light level.
use crate::{Error, invalid, rule_document::TimedScene};
use homesense_core::rule::{Lifetime, ValueType, Variable};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub scene: String,
    pub value: i64,
    pub off_scene: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub variable: String,
    pub review_evidence: String,
    pub members: Vec<Member>,
    #[serde(default)]
    pub reset_scenes: Vec<String>,
}
pub(crate) fn validate(
    groups: &[Group],
    variables: &BTreeMap<String, Variable>,
    scenes: &BTreeMap<String, TimedScene>,
) -> Result<(), Error> {
    let identifier =
        |s: &str| !s.trim().is_empty() && s.len() <= 250 && !s.chars().any(char::is_control);
    if groups.len() > 32 {
        return Err(invalid("scene switch group capacity exceeded"));
    }
    let mut ids = BTreeSet::new();
    let mut vars = BTreeSet::new();
    let mut members = BTreeSet::new();
    let mut off = BTreeSet::new();
    for group in groups {
        if !identifier(&group.id)
            || !identifier(&group.variable)
            || !ids.insert(&group.id)
            || !vars.insert(&group.variable)
            || group.review_evidence.trim().is_empty()
            || group.review_evidence.len() > 8192
            || !(2..=64).contains(&group.members.len())
            || group.reset_scenes.len() > 64
        {
            return Err(invalid("invalid or unreviewed scene switch group"));
        }
        if !variables.get(&group.variable).is_some_and(|v| {
            v.lifetime == Lifetime::Persistent && v.value_type == ValueType::Integer
        }) {
            return Err(invalid(
                "scene switch selection requires a persistent integer variable",
            ));
        }
        let reviewed = |id: &str| {
            identifier(id)
                && scenes.get(id).is_some_and(|s| {
                    s.enabled
                        && s.review_evidence
                            .as_ref()
                            .is_some_and(|v| !v.trim().is_empty())
                })
        };
        let mut values = BTreeSet::new();
        for member in &group.members {
            if member.value <= 0
                || !values.insert(member.value)
                || !members.insert(&member.scene)
                || !reviewed(&member.scene)
                || !reviewed(&member.off_scene)
            {
                return Err(invalid("invalid scene switch member or shutdown scene"));
            }
            off.insert(&member.off_scene);
        }
        let mut resets = BTreeSet::new();
        for id in &group.reset_scenes {
            if !reviewed(id) || !resets.insert(id) {
                return Err(invalid("invalid scene switch reset scene"));
            }
            off.insert(id);
        }
    }
    if members.len() > 512 || members.iter().any(|id| off.contains(id)) {
        return Err(invalid("ambiguous scene switch action identity"));
    }
    Ok(())
}
