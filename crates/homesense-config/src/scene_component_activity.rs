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
    /// Explicit absolute recipes use this component's existing causal owner
    /// for HAP readback. The owner itself never executes or authorizes writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hap_action: Option<crate::scene_tracking::HapAction>,
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
    /// Recorded mode intent is not an inferred physical state. It is reset at
    /// scope changes; Active still needs an owned write and fresh feedback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_selection: Option<RecordedSelection>,
    /// Fresh negative physical feedback may disprove an unowned program. It
    /// never establishes a selected mode or an Active program from its shape.
    #[serde(default, skip_serializing_if = "is_false")]
    pub negative_feedback: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedSelection {
    pub source_scenes: Vec<String>,
    pub reset_variables: Vec<String>,
    pub review_evidence: String,
}

fn assigns(nodes: &[homesense_core::rule::Node], variable: &str) -> bool {
    use homesense_core::rule::Action;
    nodes.iter().any(|n| match &n.action {
        Action::Set { variable: name, .. } => name == variable,
        Action::If {
            then_actions,
            else_actions,
            ..
        } => assigns(then_actions, variable) || assigns(else_actions, variable),
        _ => false,
    })
}
fn has_future_scene(nodes: &[homesense_core::rule::Node]) -> bool {
    use homesense_core::rule::Action;
    nodes.iter().any(|n| match &n.action {
        Action::Scene { .. } => true,
        Action::If {
            then_actions,
            else_actions,
            ..
        } => has_future_scene(then_actions) || has_future_scene(else_actions),
        _ => false,
    })
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
    let mut recipes = BTreeSet::new();
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
            || p.program_confirmation
                .as_ref()
                .is_none_or(|c| c.recorded_selection.is_none())
                && !document.component_mode_confirmations.iter().any(|c| {
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
        if let Some(action) = &p.hap_action {
            if !p.enabled
                || p.program_confirmation.is_none()
                || action.readback != crate::scene_tracking::ActionReadback::ConfirmedComponent
                || action.on_scene == action.off_scene
                || [&action.on_scene, &action.off_scene].into_iter().any(|id| {
                    id == &p.scene
                        || !recipes.insert(id.clone())
                        || !document.hidden_scene_entries.contains(id)
                        || !document.scene_action_available(id)
                        || document.scene_rooms.get(id) != document.scene_rooms.get(&p.scene)
                        || document.scene_scopes.get(id) != document.scene_scopes.get(&p.scene)
                        || document
                            .scene_component_activity
                            .get(id)
                            .is_some_and(|p| p.hap_action.is_some())
                        || document.scene_tracking.values().any(|p| {
                            p.hap_action.as_ref().is_some_and(|a| {
                                &p.scene == id || &a.on_scene == id || &a.off_scene == id
                            })
                        })
                })
            {
                return Err(invalid(
                    "component HAP action needs unique hidden same-room absolute recipes and its confirmed owner",
                ));
            }
            crate::component_hap::validate(p, action, document, config)?;
        }
        if let Some(program) = &p.program_confirmation {
            if let Some(intent) = &program.recorded_selection {
                let sources = intent.source_scenes.iter().collect::<BTreeSet<_>>();
                let resets = intent.reset_variables.iter().collect::<BTreeSet<_>>();
                if sources.is_empty()
                    || sources.len() > 64
                    || sources.len() != intent.source_scenes.len()
                    || !sources.contains(&p.scene)
                    || resets.is_empty()
                    || resets.len() > 64
                    || resets.len() != intent.reset_variables.len()
                    || !resets.contains(&p.variable)
                    || intent.review_evidence.trim().is_empty()
                    || intent.review_evidence.len() > 8192
                    || intent.reset_variables.iter().any(|name| {
                        document
                            .variables
                            .get(name)
                            .is_none_or(|v| v.lifetime != Lifetime::Persistent)
                            || document
                                .component_mode_confirmations
                                .iter()
                                .any(|c| &c.variable == name)
                            || document
                                .scene_switch_groups
                                .iter()
                                .any(|g| &g.variable == name)
                    })
                    || intent.source_scenes.iter().any(|scene| {
                        !document.scene_action_available(scene)
                            || document.scene_execution_kind(scene)
                                != Some(crate::rule_document::SceneExecutionKind::Immediate)
                            || !document
                                .linked_manual_scenes(std::slice::from_ref(scene))
                                .is_ok_and(|nodes| {
                                    assigns(&nodes, &p.variable) && !has_future_scene(&nodes)
                                })
                    })
                {
                    return Err(invalid(
                        "recorded selection needs distinct reviewed writers and reset-only persistent intent",
                    ));
                }
            }
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
                || program.recorded_selection.is_none() && Some(&declared) != common.as_ref()
                || program.active_conditions.iter().any(|c| {
                    homesense_core::DeviceId::new(&c.device)
                        .ok()
                        .and_then(|id| config.binding(&id))
                        .is_none_or(|(_, m)| {
                            !c.on
                                || !matches!(
                                    m.kind(),
                                    homesense_core::ModuleKind::Switch
                                        | homesense_core::ModuleKind::Dimmer
                                )
                                || c.level.is_some_and(|level| {
                                    !(1..=100).contains(&level)
                                        || m.profile() != homesense_core::ProtocolProfile::Ad8Din
                                })
                        })
                })
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
