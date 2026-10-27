//! Ephemeral actual activity. Never reads saved scene activity, selection intent,
//! recorded brightness, desired state or completed write targets. An explicitly
//! configured component provider additionally requires live causal ownership.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
    scene_activity::SceneActivity,
};
use homesense_config::scene_tracking::{Aggregation, Policy, Predicate, UnknownPolicy};
use homesense_core::{
    DeviceId,
    rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MemberObservation {
    pub id: String,
    pub ignored: bool,
    pub on: Option<bool>,
    pub actual_level: Option<u8>,
    pub matches: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Projection {
    pub activity: SceneActivity,
    pub activity_source: &'static str,
    pub scene: String,
    pub display_name: Option<String>,
    pub activity_name: String,
    pub boot: String,
    pub revision: u64,
    pub members: Vec<MemberObservation>,
    pub action_available: bool,
    pub activity_writable: bool,
    pub valid_until_ms: Option<u64>,
}
impl Projection {
    pub fn active(&self) -> Option<bool> {
        match self.activity {
            SceneActivity::Active => Some(true),
            SceneActivity::Inactive => Some(false),
            _ => None,
        }
    }
}
pub async fn project(
    policy: &Policy,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
) -> Projection {
    project_many(&[policy], admission, controller, generation, feedback)
        .await
        .remove(0)
}

/// One coherent load snapshot and bounded proof batches. No catalog metadata,
/// full configuration reload, transport query or control is needed for reads.
pub(crate) async fn project_many(
    policies: &[&Policy],
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
) -> Vec<Projection> {
    if policies.len() > 64
        || !admission
            .diagnostics(controller, generation.clone())
            .await
            .is_ok_and(|state| state.mapping_readable())
    {
        return policies
            .iter()
            .map(|policy| unknown(policy, admission, feedback))
            .collect();
    }
    let mut values = vec![];
    let mut proofs = vec![];
    for (index, policy) in policies.iter().enumerate() {
        let (value, proof) = prepare(policy, admission, feedback);
        values.push(value);
        if let Some(proof) = proof {
            proofs.push((index, proof));
        }
    }
    let mut results =
        vec![Err(hap::server::WriteStatus::ServiceCommunicationFailure); policies.len()];
    for chunk in proofs.chunks(32) {
        if let Ok(settled) = admission
            .feedback_settled_batch(
                controller,
                generation.clone(),
                chunk.iter().map(|(_, proof)| proof.clone()).collect(),
            )
            .await
            && settled.len() == chunk.len()
        {
            for ((index, _), result) in chunk.iter().zip(settled) {
                results[*index] = result;
            }
        }
    }
    let ready = admission
        .diagnostics(controller, generation.clone())
        .await
        .is_ok_and(|state| state.mapping_readable());
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            settle(
                policies[index],
                value,
                if ready {
                    results[index]
                } else {
                    Err(hap::server::WriteStatus::ServiceCommunicationFailure)
                },
            )
        })
        .collect()
}

fn unknown(policy: &Policy, admission: &Admission, feedback: &Feedback) -> Projection {
    Projection {
        activity: SceneActivity::Unknown,
        activity_source: "reviewed_tracking",
        scene: policy.scene.clone(),
        display_name: None,
        activity_name: policy.activity_name.clone(),
        boot: admission.boot_id().into(),
        revision: feedback.configuration_revision(),
        members: vec![],
        action_available: false,
        activity_writable: false,
        valid_until_ms: None,
    }
}

fn prepare(
    policy: &Policy,
    admission: &Admission,
    feedback: &Feedback,
) -> (Projection, Option<crate::feedback::RuleLease>) {
    let mut result = unknown(policy, admission, feedback);
    if !policy.enabled || !feedback.is_active() {
        return (result, None);
    }
    let mut nodes = vec![];
    let mut variables = BTreeMap::new();
    for (i, m) in policy.members.iter().enumerate() {
        if matches!(m.predicate, Predicate::Ignore { .. }) {
            continue;
        }
        let Ok(device) = DeviceId::new(&m.device) else {
            return (result, None);
        };
        for (suffix, expr, kind) in [
            ("on", Expr::DeviceOn(device.clone()), ValueType::Bool),
            ("level", Expr::DeviceLevel(device), ValueType::Integer),
        ] {
            if suffix == "level" && !matches!(m.predicate, Predicate::LevelRange { .. }) {
                continue;
            }
            let name = format!("tracking:{i}:{suffix}");
            variables.insert(
                name.clone(),
                Variable {
                    value_type: kind,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("tracking:{}:{}", policy.scene, m.id),
                action: Action::Set {
                    variable: name,
                    value: expr,
                },
            });
        }
        if let Predicate::PoweredLight { dimmer } = &m.predicate {
            let Ok(dimmer) = DeviceId::new(dimmer) else {
                return (result, None);
            };
            let name = format!("tracking:{i}:dimmer_on");
            variables.insert(
                name.clone(),
                Variable {
                    value_type: ValueType::Bool,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("tracking:{}:{}:dimmer", policy.scene, m.id),
                action: Action::Set {
                    variable: name,
                    value: Expr::DeviceOn(dimmer),
                },
            });
        }
    }
    let Ok((plan, lease)) = feedback.plan_rule(
        &nodes,
        &variables,
        Budget::default(),
        admission.monotonic_ms(),
        policy.max_age_ms,
    ) else {
        return (result, None);
    };
    for (i, m) in policy.members.iter().enumerate() {
        let ignored = matches!(m.predicate, Predicate::Ignore { .. });
        let on = plan
            .variables
            .get(&format!("tracking:{i}:on"))
            .and_then(|v| match v.value {
                Some(Value::Bool(v)) => Some(v),
                _ => None,
            });
        let on = if matches!(m.predicate, Predicate::PoweredLight { .. }) {
            let dimmer_on = plan
                .variables
                .get(&format!("tracking:{i}:dimmer_on"))
                .and_then(|v| match v.value {
                    Some(Value::Bool(v)) => Some(v),
                    _ => None,
                });
            match (on, dimmer_on) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            }
        } else {
            on
        };
        let actual_level = if on == Some(true) {
            plan.variables
                .get(&format!("tracking:{i}:level"))
                .and_then(|v| match v.value {
                    Some(Value::Integer(v)) => u8::try_from(v).ok(),
                    _ => None,
                })
        } else {
            None
        };
        let matches = match m.predicate {
            Predicate::Ignore { .. } => None,
            Predicate::Power { on: target } => on.map(|v| v == target),
            Predicate::PoweredLight { .. } => on,
            Predicate::LevelRange { minimum, maximum } => match on {
                Some(false) => Some(false),
                Some(true) => actual_level.map(|v| v >= minimum && v <= maximum),
                None => None,
            },
        };
        result.members.push(MemberObservation {
            id: m.id.clone(),
            ignored,
            on,
            actual_level,
            matches,
        });
    }
    result.valid_until_ms = lease
        .observations
        .values()
        .filter_map(|at| at.and_then(|at| at.checked_add(policy.max_age_ms)))
        .min();
    (result, Some(lease))
}

fn settle(
    policy: &Policy,
    mut result: Projection,
    settled: Result<bool, hap::server::WriteStatus>,
) -> Projection {
    let valid_until = result.valid_until_ms.take();
    if settled.is_err() {
        for m in &mut result.members {
            m.on = None;
            m.actual_level = None;
            m.matches = None;
        }
        return result;
    }
    if settled == Ok(false) {
        result.activity = SceneActivity::Pending;
        for m in &mut result.members {
            m.on = None;
            m.actual_level = None;
            m.matches = None;
        }
        return result;
    }
    let values = result
        .members
        .iter()
        .filter(|m| !m.ignored)
        .map(|m| m.matches)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return result;
    }
    let unknown = values.iter().any(Option::is_none);
    let active = match policy.aggregation {
        Aggregation::All if values.contains(&Some(false)) => Some(false),
        Aggregation::Any if values.contains(&Some(true)) => Some(true),
        _ if unknown => None,
        Aggregation::All => Some(true),
        Aggregation::Any => Some(false),
    };
    let active = if unknown && policy.unknown_policy == UnknownPolicy::RequireAllKnown {
        None
    } else {
        active
    };
    if active.is_some() {
        result.valid_until_ms = valid_until;
    }
    result.activity = match active {
        Some(true) => SceneActivity::Active,
        Some(false) => SceneActivity::Inactive,
        None => SceneActivity::Unknown,
    };
    result
}

/// One catalog's immutable mapping and captured component views. Each proof is
/// still revalidated by the admission owner; no observed activity is cached.
pub(crate) struct CatalogReadContext<'a> {
    config: &'a homesense_config::Configuration,
    component_views: Option<Vec<crate::component_modes::View>>,
}
impl<'a> CatalogReadContext<'a> {
    pub(crate) fn new(config: &'a homesense_config::Configuration) -> Self {
        Self {
            config,
            component_views: None,
        }
    }
}

pub async fn read(
    document: &homesense_config::rule_document::Document,
    id: &str,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
) -> Result<Option<Projection>, String> {
    read_inner(
        document, id, admission, controller, generation, feedback, None,
    )
    .await
}

pub(crate) async fn read_catalog(
    document: &homesense_config::rule_document::Document,
    id: &str,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
    context: &mut CatalogReadContext<'_>,
) -> Result<Option<Projection>, String> {
    read_inner(
        document,
        id,
        admission,
        controller,
        generation,
        feedback,
        Some(context),
    )
    .await
}

// A first activation may still be owned by the physical component rather
// than by a continuing program record. Read that owner before applying an
// unowned, negative-only fallback; no read may establish a new selection.
async fn read_component_state(
    policy: &homesense_config::scene_component_activity::Policy,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
    context: Option<&mut CatalogReadContext<'_>>,
) -> Result<(SceneActivity, Option<u64>), String> {
    let proof = if let Some(context) = context {
        if context.component_views.is_none() {
            context.component_views = Some(
                admission
                    .component_mode_views(controller, generation.clone())
                    .await
                    .map_err(|e| format!("catalog component activity views: {e:?}"))?,
            );
        }
        context
            .component_views
            .as_ref()
            .expect("captured component views")
            .iter()
            .find(|v| v.policies.iter().any(|p| p.variable == policy.variable))
            .and_then(|view| {
                crate::component_modes::Proof::prepare(
                    view,
                    context.config,
                    feedback,
                    admission.monotonic_ms(),
                )
                .ok()
            })
    } else {
        let views = admission
            .component_mode_views(controller, generation.clone())
            .await
            .map_err(|e| format!("component activity views: {e:?}"))?;
        let bundle = admission
            .automation_bundle_shared(controller, generation.clone())
            .await
            .map_err(|e| format!("component activity bundle: {e:?}"))?;
        views
            .iter()
            .find(|v| v.policies.iter().any(|p| p.variable == policy.variable))
            .and_then(|view| {
                crate::component_modes::Proof::prepare(
                    view,
                    &bundle.configuration,
                    feedback,
                    admission.monotonic_ms(),
                )
                .ok()
            })
    };
    admission
        .component_activity(controller, generation.clone(), policy.scene.clone(), proof)
        .await
        .map_err(|e| format!("component activity: {e:?}"))
}

async fn read_inner(
    document: &homesense_config::rule_document::Document,
    id: &str,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
    mut context: Option<&mut CatalogReadContext<'_>>,
) -> Result<Option<Projection>, String> {
    if let Some(policy) = document
        .scene_component_activity
        .get(id)
        .filter(|p| p.enabled)
    {
        let negative = policy
            .program_confirmation
            .as_ref()
            .is_some_and(|p| p.recorded_selection.is_some() || p.negative_feedback);
        let view = if policy.program_confirmation.is_some() {
            admission
                .program_activity_read_view(controller, generation.clone(), id.into())
                .await
                .map_err(|e| format!("program activity view: {e:?}"))?
        } else {
            None
        };
        let component = if view.is_none()
            && policy
                .program_confirmation
                .as_ref()
                .is_some_and(|p| p.negative_feedback && p.recorded_selection.is_none())
        {
            Some(
                read_component_state(
                    policy,
                    admission,
                    controller,
                    generation,
                    feedback,
                    context.as_deref_mut(),
                )
                .await?,
            )
        } else {
            None
        };
        if policy.program_confirmation.is_some()
            && (view.is_some() || negative)
            && component
                .as_ref()
                .is_none_or(|value| value.0 == SceneActivity::Unknown)
        {
            let missing = view.is_none();
            let proof = if let Some(Some(view)) = view {
                if let Some(context) = context.as_deref() {
                    crate::program_activity::Proof::prepare(
                        &view,
                        context.config,
                        feedback,
                        admission.monotonic_ms(),
                    )
                    .ok()
                } else {
                    let bundle = admission
                        .automation_bundle_shared(controller, generation.clone())
                        .await
                        .map_err(|e| format!("program bundle: {e:?}"))?;
                    crate::program_activity::Proof::prepare(
                        &view,
                        &bundle.configuration,
                        feedback,
                        admission.monotonic_ms(),
                    )
                    .ok()
                }
            } else if missing && negative {
                if let Some(context) = context.as_deref() {
                    crate::program_activity::Proof::prepare_negative(
                        policy,
                        context.config,
                        feedback,
                        generation.clone(),
                        admission.monotonic_ms(),
                    )
                    .ok()
                } else {
                    let bundle = admission
                        .automation_bundle_shared(controller, generation.clone())
                        .await
                        .map_err(|e| format!("program bundle: {e:?}"))?;
                    crate::program_activity::Proof::prepare_negative(
                        policy,
                        &bundle.configuration,
                        feedback,
                        generation.clone(),
                        admission.monotonic_ms(),
                    )
                    .ok()
                }
            } else {
                None
            };
            let (activity, valid_until_ms) = admission
                .program_activity(controller, generation.clone(), id.into(), proof)
                .await
                .map_err(|e| format!("program activity: {e:?}"))?
                .unwrap_or((SceneActivity::Unknown, None));
            return Ok(Some(Projection {
                activity,
                activity_source: "confirmed_program",
                scene: id.into(),
                display_name: document.scene_metadata.get(id).cloned(),
                activity_name: policy.activity_name.clone(),
                boot: admission.boot_id().into(),
                revision: document.revision,
                members: vec![],
                action_available: document.scene_action_available(id),
                activity_writable: false,
                valid_until_ms,
            }));
        }
        let (activity, valid_until_ms) = match component {
            Some(value) => value,
            None => {
                read_component_state(policy, admission, controller, generation, feedback, context)
                    .await?
            }
        };
        return Ok(Some(Projection {
            activity,
            activity_source: "confirmed_component",
            scene: id.into(),
            display_name: document.scene_metadata.get(id).cloned(),
            activity_name: policy.activity_name.clone(),
            boot: admission.boot_id().into(),
            revision: document.revision,
            members: vec![],
            action_available: document.scene_action_available(id),
            activity_writable: false,
            valid_until_ms,
        }));
    }
    let Some(policy) = document.scene_tracking.get(id).filter(|p| p.enabled) else {
        return Ok(None);
    };
    if !admission
        .diagnostics(controller, generation.clone())
        .await
        .map_err(|e| format!("tracking readiness: {e:?}"))?
        .mapping_readable()
    {
        return Err("tracking runtime not ready".into());
    }
    let mut result = project(policy, admission, controller, generation, feedback).await;
    result.display_name = document.scene_metadata.get(id).cloned();
    // This policy publishes read-only activity. Explicit scene execution remains
    // a separate action API; stateful Off needs a future reviewed off contract.
    result.action_available = document.scene_action_available(id);
    Ok(Some(result))
}
pub(crate) async fn activity(
    document: &homesense_config::rule_document::Document,
    id: &str,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
    max_age: u64,
) -> Result<SceneActivity, String> {
    if let Some(value) = read(document, id, admission, controller, generation, feedback).await? {
        return Ok(value.activity);
    }
    if document.scene_execution_kind(id)
        == Some(homesense_config::rule_document::SceneExecutionKind::Immediate)
    {
        return Ok(SceneActivity::Unknown);
    }
    let scene = document.timed_scenes.get(id).ok_or("unknown timed scene")?;
    if !scene.enabled
        || scene
            .review_evidence
            .as_ref()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err("timed scene disabled or unreviewed".into());
    }
    crate::scene_activity::Targets::new(&scene.steps)
        .project(admission, controller, generation, feedback, max_age)
        .await
}
