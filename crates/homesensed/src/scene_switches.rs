//! Planning only: local selection and manual scene receipts are committed by
//! the admission owner. No guessed EA-5 variable or physical completion state.
use homesense_config::rule_document::Document;
use homesense_core::rule::Value;
use homesense_storage::variables::{Stored, Update};
use std::collections::{BTreeMap, BTreeSet};

pub struct Plan {
    pub definitions: Vec<String>,
    pub updates: Vec<Update>,
}
pub fn variable_names(document: &Document) -> Vec<String> {
    document
        .scene_switch_groups
        .iter()
        .map(|g| g.variable.clone())
        .collect()
}
pub fn states(
    document: &Document,
    snapshot: &BTreeMap<String, Stored>,
) -> Result<BTreeMap<String, Option<bool>>, String> {
    let mut states = BTreeMap::new();
    for group in &document.scene_switch_groups {
        let selected = selection(group, snapshot)?;
        for member in &group.members {
            states.insert(member.scene.clone(), selected.map(|v| v == member.value));
        }
    }
    Ok(states)
}
fn selection(
    group: &homesense_config::scene_switches::Group,
    snapshot: &BTreeMap<String, Stored>,
) -> Result<Option<i64>, String> {
    match snapshot
        .get(&group.variable)
        .ok_or("scene selection variable missing")?
        .value
    {
        None => Ok(None),
        Some(Value::Integer(value))
            if value == 0 || group.members.iter().any(|m| m.value == value) =>
        {
            Ok(Some(value))
        }
        _ => Err("invalid scene selection value".into()),
    }
}
pub fn plan(
    document: &Document,
    writes: &[(String, Option<bool>)],
    snapshot: &BTreeMap<String, Stored>,
) -> Result<Plan, String> {
    let mut addressed = BTreeSet::new();
    for (id, _) in writes {
        if !addressed.insert(id)
            || !document.timed_scenes.get(id).is_some_and(|s| {
                s.enabled
                    && s.review_evidence
                        .as_ref()
                        .is_some_and(|v| !v.trim().is_empty())
            })
        {
            return Err("scene missing, duplicated, disabled or unreviewed".into());
        }
    }
    let mut handled = BTreeSet::new();
    let mut definitions = BTreeSet::new();
    let mut updates = vec![];
    for group in &document.scene_switch_groups {
        let member_writes = writes
            .iter()
            .filter_map(|(id, on)| {
                group
                    .members
                    .iter()
                    .find(|m| m.scene == *id)
                    .and_then(|m| on.map(|on| (m, on)))
            })
            .collect::<Vec<_>>();
        let resets = writes
            .iter()
            .filter(|(id, on)| *on == Some(true) && group.reset_scenes.contains(id))
            .collect::<Vec<_>>();
        if member_writes.is_empty() && resets.is_empty() {
            continue;
        }
        let selected = selection(group, snapshot)?;
        for (member, _) in &member_writes {
            handled.insert(member.scene.clone());
        }
        let enabled = member_writes
            .iter()
            .filter(|(_, on)| *on)
            .map(|(m, _)| *m)
            .collect::<Vec<_>>();
        if enabled.len() > 1 || (!enabled.is_empty() && !resets.is_empty()) {
            return Err("conflicting scene selections in one group".into());
        }
        let next = if let Some(member) = enabled.first() {
            if selected != Some(member.value) {
                definitions.insert(member.scene.clone());
                Some(member.value)
            } else {
                None
            }
        } else if !resets.is_empty() {
            for (id, _) in resets {
                definitions.insert(id.clone());
            }
            Some(0)
        } else {
            let selected =
                selected.ok_or("unknown selection: cannot infer which scene to turn off")?;
            if let Some((member, _)) = member_writes
                .iter()
                .find(|(m, on)| !*on && m.value == selected)
            {
                definitions.insert(member.off_scene.clone());
                Some(0)
            } else {
                None
            } // Off for a nonselected member cannot turn off another scene.
        };
        if let Some(next) = next {
            updates.push(Update {
                revision: document.revision,
                name: group.variable.clone(),
                expected_version: snapshot[&group.variable].version,
                value: Some(Value::Integer(next)),
            });
        }
    }
    // Unconfigured scenes remain momentary actions; subscription-only and false
    // writes never turn into a physical action.
    for (id, on) in writes {
        if *on == Some(true) && !handled.contains(id) {
            definitions.insert(id.clone());
        }
    }
    // Preserve input order for explicit starts; appended shutdown definitions
    // have deterministic order, and never run alongside a new group selection.
    let mut ordered = vec![];
    for (id, _) in writes {
        if definitions.remove(id) {
            ordered.push(id.clone());
        }
    }
    ordered.extend(definitions);
    Ok(Plan {
        definitions: ordered,
        updates,
    })
}
