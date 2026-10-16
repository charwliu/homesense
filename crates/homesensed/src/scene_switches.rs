//! Planning only: local selection and manual scene receipts are committed by
//! the admission owner. No guessed EA-5 variable or physical completion state.
use homesense_config::rule_document::Document;
use homesense_core::rule::Value;
use homesense_storage::variables::{Stored, Update};
use std::collections::{BTreeMap, BTreeSet};

/// A healthy but unknown selection is different from a failed observation.
/// Neither state is a boolean Off or permission to initialize the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum SelectionState {
    Known(bool),
    Unknown,
    Unavailable(UnavailableReason),
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    #[default]
    NotSampled,
    Invalidated,
    CacheExpired,
    RuntimeUnavailable,
    RuntimeNotReady,
    ReadFailed,
    ReadTimedOut,
}
impl SelectionState {
    pub fn from_selected(value: Option<bool>) -> Self {
        value.map_or(Self::Unknown, Self::Known)
    }
    pub fn selected(self) -> Option<bool> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown | Self::Unavailable(_) => None,
        }
    }
}

/// Source for a selection, not physical completion or EA-5 variable proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Details {
    pub selected: Option<bool>,
    pub selection_state: SelectionState,
    pub selection_source: &'static str,
    pub observation_current: Option<bool>,
    pub observation_boot: Option<String>,
    pub observed_ms: Option<String>,
    pub historical_selected: Option<bool>,
    pub pending_relative: bool,
    pub relative_unknown_allowed: bool,
}
pub(crate) fn details(
    document: &Document,
    snapshot: &BTreeMap<String, homesense_storage::scene_observation::Snapshot>,
) -> Result<BTreeMap<String, Details>, String> {
    let values = snapshot
        .iter()
        .map(|(name, state)| (name.clone(), state.stored.clone()))
        .collect();
    let selected = states(document, &values)?;
    let mut result = BTreeMap::new();
    for group in &document.scene_switch_groups {
        let state = &snapshot[&group.variable];
        let inferred = state.evidence.as_ref();
        for member in &group.members {
            result.insert(
                member.scene.clone(),
                Details {
                    selected: selected[&member.scene],
                    selection_state: SelectionState::from_selected(selected[&member.scene]),
                    selection_source: if state.component_inferred {"local_written_feedback_inference"} else if state.physical.is_some() {
                        "physical_feedback"
                    } else if inferred.is_some() {
                        "inferred"
                    } else if state.initialization.is_some() {
                        "light_initialization"
                    } else if state.stored.value.is_some() {
                        "local_intent"
                    } else {
                        "unknown"
                    },
                    observation_current: state.physical.as_ref().map(|p|p.observed_ms.is_some()).or_else(||inferred.map(|_| state.evidence_current)),
                    observation_boot: state.physical.as_ref().map(|p|format!("{:032x}",p.boot_id)).or_else(||inferred.map(|e| format!("{:032x}", e.scope.boot_id))),
                    observed_ms: state.physical.as_ref().and_then(|p|p.observed_ms).map(|v|v.to_string()).or_else(||state.physical.is_none().then(||inferred.map(|e|e.observed_ms.to_string())).flatten()),
                    historical_selected: state.physical.as_ref().and_then(|p|match p.history_value {Some(Value::Integer(value))=>Some(value==member.value),_=>None}),
                    pending_relative: state.physical.as_ref().is_some_and(|p|p.pending_relative),
                    relative_unknown_allowed: state.physical.as_ref().is_some_and(|p|!p.pending_relative && p.relative_eligible) && selected[&member.scene].is_none(),
                },
            );
        }
    }
    Ok(result)
}
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
    states_in_groups(&document.scene_switch_groups, snapshot)
}
pub(crate) fn states_in_groups(
    groups: &[homesense_config::scene_switches::Group],
    snapshot: &BTreeMap<String, Stored>,
) -> Result<BTreeMap<String, Option<bool>>, String> {
    let mut states = BTreeMap::new();
    for group in groups {
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
            if value == 0 || group.members.iter().any(|m| m.value == value)||group.unselected_values.contains(&value) =>
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

/// Only explicit group members and reset actions alter local selection.
pub(crate) fn related(document: &Document, scene: &str) -> bool {
    document.scene_switch_groups.iter().any(|g| {
        g.members.iter().any(|m| m.scene == scene) || g.reset_scenes.iter().any(|id| id == scene)
    })
}
pub(crate) async fn activate_forced(
    document: &Document,
    scene: &str,
    admission: &crate::admission::Admission,
    scheduler: &mut crate::scene_scheduler::SceneScheduler,
    controller: uuid::Uuid,
) -> Result<u64, String> {
    let generation = admission
        .task_generation()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let snapshot = admission
        .variable_snapshot(controller, generation, variable_names(document))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let plan = forced_plan(document, scene, &snapshot)?;
    let runs = scheduler
        .start_documents_with_variables_as(
            document.revision,
            plan.definitions,
            admission.monotonic_ms(),
            controller,
            plan.updates,
        )
        .await?;
    runs.into_iter()
        .next()
        .ok_or("manual activation receipt missing".into())
}
pub(crate) fn forced_plan(
    document: &Document,
    scene: &str,
    snapshot: &BTreeMap<String, Stored>,
) -> Result<Plan, String> {
    let mut plan = plan(document, &[(scene.into(), Some(true))], snapshot)?;
    if plan.definitions.is_empty() {
        plan.definitions.push(scene.into());
    }
    Ok(plan)
}
