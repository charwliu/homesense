//! Explicit one-time initialization from exact fresh physical light vectors.
//! No light command, LED command, scene action or external candidate is created.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    scene_scheduler::SceneScheduler,
};
use hap::server::WriteStatus;
use homesense_config::{
    Configuration,
    rule_document::Document,
    scene_switches::{Group, LightTarget, LightTargets},
};
use homesense_core::{
    DeviceId,
    queue::Epochs,
    rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use std::collections::BTreeMap;
use uuid::Uuid;

/// All lamps must be known. Off has priority; otherwise exactly one complete
/// final vector must match, including targets intentionally set to zero.
pub fn matching_member(
    targets: &LightTargets,
    actual: &BTreeMap<DeviceId, LightTarget>,
) -> Option<i64> {
    let first = targets.values().next()?;
    if actual.is_empty() || actual.keys().ne(first.keys()) {
        return None;
    }
    if targets.iter().any(|(value, vector)| {
        *value <= 0
            || vector.keys().ne(actual.keys())
            || vector.iter().any(|(id, target)| {
                std::mem::discriminant(target) != std::mem::discriminant(&actual[id])
            })
    }) {
        return None;
    }
    if actual.values().all(|value| value.is_off()) {
        return Some(0);
    }
    let mut matches = targets.iter().filter(|(_, expected)| *expected == actual);
    let value = *matches.next()?.0;
    matches.next().is_none().then_some(value)
}
pub(crate) struct Prepared {
    generation: TaskGeneration,
    controller: Uuid,
    group: String,
    variable: String,
    expected_version: u64,
    targets: LightTargets,
    actual: BTreeMap<DeviceId, LightTarget>,
    max_age_ms: u64,
    lease: RuleLease,
    selected: i64,
}
impl Prepared {
    pub(crate) async fn prepare(
        admission: &Admission,
        controller: Uuid,
        generation: TaskGeneration,
        group: &Group,
        document: &Document,
        feedback: &Feedback,
    ) -> Result<Option<Self>, WriteStatus> {
        let Some(policy) = group
            .initialization
            .as_ref()
            .filter(|policy| policy.enabled)
        else {
            return Ok(None);
        };
        let snapshot = admission
            .scene_selection_snapshot(controller, generation.clone(), vec![group.variable.clone()])
            .await?;
        let state = &snapshot[&group.variable];
        // Effective unknown from an invalidated observation is not raw NULL.
        if state.stored.value.is_some()
            || state.stored.version != 0
            || state.evidence.is_some()
            || state.initialization.is_some()
        {
            return Ok(None);
        }
        let targets = group
            .light_targets(&document.timed_scenes)
            .map_err(|_| WriteStatus::InvalidValueInRequest)?;
        let first = targets
            .values()
            .next()
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let mut variables = BTreeMap::new();
        let mut nodes = vec![];
        for (index, (device, target)) in first.iter().enumerate() {
            let variable = format!("sample:{index}");
            let (value_type, value) = match target {
                LightTarget::Switch(_) => (ValueType::Bool, Expr::DeviceOn(device.clone())),
                LightTarget::Level(_) => (ValueType::Integer, Expr::DeviceLevel(device.clone())),
            };
            variables.insert(
                variable.clone(),
                Variable {
                    value_type,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("initialization:{}:{index}", group.id),
                action: Action::Set { variable, value },
            });
        }
        // Plain physical predicates: never supply remembered dimmer settings.
        let (plan, lease) = feedback
            .plan_rule(
                &nodes,
                &variables,
                Budget::default(),
                admission.monotonic_ms(),
                policy.max_age_ms,
            )
            .map_err(|_| WriteStatus::ResourceBusy)?;
        let mut actual = BTreeMap::new();
        for (index, (id, _)) in first.iter().enumerate() {
            let value = match &plan.variables[&format!("sample:{index}")].value {
                Some(Value::Bool(on)) => LightTarget::Switch(*on),
                Some(Value::Integer(level)) if (0..=100).contains(level) => {
                    LightTarget::Level(*level as u8)
                }
                _ => return Ok(None),
            };
            actual.insert(id.clone(), value);
        }
        let Some(selected) = matching_member(&targets, &actual) else {
            return Ok(None);
        };
        Ok(Some(Self {
            generation,
            controller,
            group: group.id.clone(),
            variable: group.variable.clone(),
            expected_version: state.stored.version,
            targets,
            actual,
            max_age_ms: policy.max_age_ms,
            lease,
            selected,
        }))
    }
    pub(crate) fn variable(&self)->&str{&self.variable}
    pub(crate) fn owned_by(&self, controller: Uuid, generation: &TaskGeneration) -> bool {
        self.controller == controller && &self.generation == generation
    }
    /// Called synchronously by the admission owner after readiness/queue checks.
    pub(crate) fn commit(
        &self,
        store: &mut homesense_storage::Store,
        config: &Configuration,
        document: &Document,
        boot: &str,
        epochs: Epochs,
        now: u64,
    ) -> Result<(), WriteStatus> {
        let group = document
            .scene_switch_groups
            .iter()
            .find(|group| group.id == self.group && group.variable == self.variable)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let policy = group
            .initialization
            .as_ref()
            .filter(|policy| policy.enabled && policy.max_age_ms == self.max_age_ms)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        if policy.review_evidence.trim().is_empty()
            || group
                .light_targets(&document.timed_scenes)
                .map_err(|_| WriteStatus::InvalidValueInRequest)?
                != self.targets
            || matching_member(&self.targets, &self.actual) != Some(self.selected)
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        if self.lease.observations.is_empty()
            || !self.lease.recorded_off_reads.is_empty()
            || self.lease.observations.values().any(Option::is_none)
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        for id in self.actual.keys() {
            if !self.lease.observations.contains_key(id)
                || config
                    .devices()
                    .get(id)
                    .and_then(|device| device.supply_relay.as_ref())
                    .is_some_and(|relay| !self.lease.observations.contains_key(relay))
            {
                return Err(WriteStatus::InvalidValueInRequest);
            }
        }
        let boot_id = Uuid::parse_str(boot)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
            .as_u128();
        let _guard = self
            .lease
            .lock(boot_id, document.revision, epochs.transport, now)
            .map_err(|_| WriteStatus::ResourceBusy)?;
        if store
            .has_pending_scene(boot, document.revision)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
        {
            return Err(WriteStatus::ResourceBusy);
        }
        for (device, observed) in &self.lease.observations {
            if !store
                .feedback_is_settled_in_epoch(boot, device, *observed, epochs.transport)
                .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
            {
                return Err(WriteStatus::ResourceBusy);
            }
        }
        let observed_ms = self
            .lease
            .observations
            .values()
            .filter_map(|observed| *observed)
            .min()
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        store
            .initialize_scene_selection(
                &self.variable,
                self.expected_version,
                &homesense_storage::scene_initialization::Origin {
                    group: self.group.clone(),
                    boot_id,
                    epochs,
                    observed_ms,
                    initialized_ms: now,
                    value: self.selected,
                },
            )
            .map_err(|error| match error {
                homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit => {
                    WriteStatus::InvalidValueInRequest
                }
                _ => WriteStatus::ServiceCommunicationFailure,
            })
    }
}
pub async fn tick(
    admission: &Admission,
    controller: Uuid,
    document: &Document,
    scenes: &SceneScheduler,
    feedback: &Feedback,
) -> Result<(), String> {
    if !document.scene_switch_groups.iter().any(|group| {
        group
            .initialization
            .as_ref()
            .is_some_and(|policy| policy.enabled)
    }) || !scenes.publication_impact().is_empty()
    {
        return Ok(());
    }
    let generation = match admission.task_generation().await {
        Ok(generation) => generation,
        Err(WriteStatus::ResourceBusy) => return Ok(()),
        Err(error) => return Err(format!("initialization scope: {error:?}")),
    };
    for group in &document.scene_switch_groups {
        let prepared = match Prepared::prepare(
            admission,
            controller,
            generation.clone(),
            group,
            document,
            feedback,
        )
        .await
        {
            Ok(Some(prepared)) => prepared,
            Ok(None) | Err(WriteStatus::ResourceBusy) => continue,
            Err(error) => return Err(format!("initialization read: {error:?}")),
        };
        match admission
            .initialize_scene_selection(controller, generation.clone(), prepared)
            .await
        {
            Ok(()) | Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {}
            Err(error) => return Err(format!("initialization commit: {error:?}")),
        }
    }
    Ok(())
}
