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
    let mut result = Projection {
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
    };
    if !policy.enabled
        || !feedback.is_active()
        || !admission
            .diagnostics(controller, generation.clone())
            .await
            .is_ok_and(|d| d.mapping_readable())
    {
        return result;
    }
    let mut nodes = vec![];
    let mut variables = BTreeMap::new();
    for (i, m) in policy.members.iter().enumerate() {
        if matches!(m.predicate, Predicate::Ignore { .. }) {
            continue;
        }
        let Ok(device) = DeviceId::new(&m.device) else {
            return result;
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
    }
    let Ok((plan, lease)) = feedback.plan_rule(
        &nodes,
        &variables,
        Budget::default(),
        admission.monotonic_ms(),
        policy.max_age_ms,
    ) else {
        return result;
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
    // This revalidates boot/revision/transport epoch, freshness, feedback lease,
    // durable pending control and both physical components of a combined light.
    let valid_until = lease
        .observations
        .values()
        .filter_map(|at| at.and_then(|at| at.checked_add(policy.max_age_ms)))
        .min();
    let settled = admission
        .feedback_settled(controller, generation.clone(), lease)
        .await;
    if settled.is_err()
        || !admission
            .diagnostics(controller, generation.clone())
            .await
            .is_ok_and(|d| d.mapping_readable())
    {
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

pub async fn read(
    document: &homesense_config::rule_document::Document,
    id: &str,
    admission: &Admission,
    controller: uuid::Uuid,
    generation: &TaskGeneration,
    feedback: &Feedback,
) -> Result<Option<Projection>, String> {
    if let Some(policy) = document.scene_component_activity.get(id).filter(|p| p.enabled) {
        let views = admission.component_mode_views(controller, generation.clone()).await
            .map_err(|e| format!("component activity views: {e:?}"))?;
        let bundle = admission.automation_bundle(controller, generation.clone()).await
            .map_err(|e| format!("component activity bundle: {e:?}"))?;
        let proof = views.iter()
            .find(|v| v.policies.iter().any(|p| p.variable == policy.variable))
            .and_then(|view| crate::component_modes::Proof::prepare(
                view, &bundle.configuration, feedback, admission.monotonic_ms(),
            ).ok());
        let (activity, valid_until_ms) = admission
            .component_activity(controller, generation.clone(), id.into(), proof).await
            .map_err(|e| format!("component activity: {e:?}"))?;
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
    if document.scene_execution_kind(id)==Some(homesense_config::rule_document::SceneExecutionKind::Immediate){return Ok(SceneActivity::Unknown);}
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
