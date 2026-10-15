//! Ephemeral current physical scene projection. No persisted intent changes,
//! scene actions, commands or external toggle baselines are produced.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    scene_scheduler::SceneScheduler,
};
use hap::server::WriteStatus;
use homesense_config::{Configuration, rule_document::Document, scene_switches::{Group, ReconciliationPolicy}};
use homesense_core::{
    queue::Epochs,
    rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable},
    scene_pattern::{MemberPatterns, MemberLevels, LevelObservations, Observations, matching_member, matching_member_with_levels,brightness_reads_for_collision},
};
use homesense_storage::{
    Store,
    scene_observation::{PhysicalProjection, Snapshot},
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) struct Prepared {
    generation: TaskGeneration,
    controller: Uuid,
    boot: String,
    variable: String,
    group: String,
    version: u64,
    patterns: MemberPatterns,
    actual: Observations,
    level_patterns: Option<MemberLevels>,
    levels: LevelObservations,
    policy: ReconciliationPolicy,
    max_age_ms: u64,
    lease: RuleLease,
    relative_eligible: bool,
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
        let Some(policy) = group.reconciliation.as_ref().filter(|p| p.enabled) else {
            return Ok(None);
        };
        let snapshot = admission
            .scene_selection_snapshot(controller, generation.clone(), vec![group.variable.clone()])
            .await?;
        let patterns = group
            .light_on_patterns(&document.timed_scenes)
            .map_err(|_| WriteStatus::InvalidValueInRequest)?;
        let first = patterns
            .values()
            .next()
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let mut variables = BTreeMap::new();
        let mut nodes = vec![];
        for (index, id) in first.keys().enumerate() {
            let name = format!("sample:{index}");
            variables.insert(
                name.clone(),
                Variable {
                    value_type: ValueType::Bool,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("reconciliation:{}:{index}", group.id),
                action: Action::Set {
                    variable: name,
                    value: Expr::DeviceOn(id.clone()),
                },
            });
        }
        // Physical samples only. Remembered brightness remains separate.
        let (mut plan, mut lease) = feedback
            .plan_rule(
                &nodes,
                &variables,
                Budget::default(),
                admission.monotonic_ms(),
                policy.max_age_ms,
            )
            .map_err(|_| WriteStatus::ResourceBusy)?;
        let state = &snapshot[&group.variable];
        let after = state
            .evidence
            .as_ref()
            .filter(|_| state.evidence_current)
            .map(|e| e.observed_ms);
        let powers=|plan:&homesense_core::rule::Plan,lease:&RuleLease|->Observations { first
            .keys()
            .enumerate()
            .map(|(index, id)| {
                let after_external = after.is_none_or(|after| {
                    lease
                        .observations
                        .get(id)
                        .and_then(|v| *v)
                        .is_some_and(|t| t > after)
                        && feedback.supply_relay(id).is_none_or(|relay| {
                            lease
                                .observations
                                .get(relay)
                                .and_then(|v| *v)
                                .is_some_and(|t| t > after)
                        })
                });
                let value = match plan.variables[&format!("sample:{index}")].value {
                    Some(Value::Bool(on)) if after_external => Some(on.into()),
                    _ => None,
                };
                (id.clone(), value)
            })
            .collect() };
        let mut actual=powers(&plan,&lease);
        let level_patterns=if policy.policy==ReconciliationPolicy::PowerPatternThenProfileLevel {
            Some(group.profile_level_patterns(&document.timed_scenes).map_err(|_|WriteStatus::InvalidValueInRequest)?)
        }else{None};
        let mut levels=LevelObservations::new();
        if let Some(targets)=&level_patterns {
            let reads=brightness_reads_for_collision(&patterns,targets,&actual).unwrap_or_default();
            if !reads.is_empty() {
                for (index,id) in reads.iter().enumerate() {
                    let name=format!("level:{index}");
                    variables.insert(name.clone(),Variable{value_type:ValueType::Integer,lifetime:Lifetime::Temporary,value:None});
                    nodes.push(Node{source:format!("reconciliation-level:{}:{index}",group.id),action:Action::Set{variable:name,value:Expr::DeviceLevel(id.clone())}});
                }
                // One read proof covers both powers and only the colliding
                // candidates' discriminating levels, including hidden supplies.
                (plan,lease)=feedback.plan_rule(&nodes,&variables,Budget::default(),admission.monotonic_ms(),policy.max_age_ms).map_err(|_|WriteStatus::ResourceBusy)?;
                actual=powers(&plan,&lease);
                for (index,id) in reads.into_iter().enumerate() {
                    let value=match plan.variables[&format!("level:{index}")].value {
                        Some(Value::Integer(value)) if actual.get(&id).is_some_and(Option::is_some)=>u8::try_from(value).ok().and_then(|v|homesense_core::Percent::new(v).ok()),
                        _=>None,
                    };
                    levels.insert(id,value);
                }
            }
        }
        let relative_eligible=feedback.native_scene_capture(&first.keys().cloned().collect::<Vec<_>>(),admission.monotonic_ms(),policy.max_age_ms).is_ok();
        Ok(Some(Self {
            controller,
            boot: admission.boot_id().to_owned(),
            generation,
            variable: group.variable.clone(),
            group: group.id.clone(),
            version: snapshot[&group.variable].stored.version,
            patterns,
            actual,
            level_patterns,
            levels,
            policy: policy.policy,
            max_age_ms: policy.max_age_ms,
            lease,
            relative_eligible,
        }))
    }
    pub(crate) fn variable(&self) -> &str {
        &self.variable
    }
    pub(crate) fn owned_by(&self, controller: Uuid, generation: &TaskGeneration) -> bool {
        self.controller == controller && &self.generation == generation
    }
    pub(crate) fn validate(
        &self,
        store: &Store,
        config: &Configuration,
        document: &Document,
        boot: &str,
        epochs: Epochs,
        now: u64,
    ) -> Result<(Option<i64>, Option<u64>), WriteStatus> {
        let group = document
            .scene_switch_groups
            .iter()
            .find(|g| g.id == self.group && g.variable == self.variable)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let policy = group
            .reconciliation
            .as_ref()
            .filter(|p| p.enabled)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        if self.boot != boot
            || self.generation.epochs != epochs
            || policy.max_age_ms != self.max_age_ms
            || policy.policy != self.policy
            || (policy.policy==ReconciliationPolicy::PowerPatternThenProfileLevel && group.profile_level_patterns(&document.timed_scenes).map_err(|_|WriteStatus::InvalidValueInRequest)?!=self.level_patterns.clone().ok_or(WriteStatus::InvalidValueInRequest)?)
            || group
                .light_on_patterns(&document.timed_scenes)
                .map_err(|_| WriteStatus::InvalidValueInRequest)?
                != self.patterns
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let state = store
            .logical_variable(document.revision, &self.variable)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        if state.version != self.version || !self.lease.recorded_off_reads.is_empty() {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        for id in self.actual.keys() {
            if !self.lease.observations.contains_key(id)
                || config
                    .devices()
                    .get(id)
                    .and_then(|d| d.supply_relay.as_ref())
                    .is_some_and(|id| !self.lease.observations.contains_key(id))
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
        if let Some(after)=store.native_scene_failure_after(boot,epochs,&self.group).map_err(|_|WriteStatus::ServiceCommunicationFailure)?
            && self.lease.observations.values().any(|observed|observed.is_none_or(|at|at<=after)) {return Err(WriteStatus::ResourceBusy)}
        if store
            .has_pending_scene(boot, document.revision)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
        {
            return Err(WriteStatus::ResourceBusy);
        }
        for (id, observed) in &self.lease.observations {
            if !store
                .feedback_is_settled_in_epoch(boot, id, *observed, epochs.transport)
                .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
            {
                return Err(WriteStatus::ResourceBusy);
            }
        }
        let selected = match &self.level_patterns {
            Some(targets)=>matching_member_with_levels(&self.patterns,targets,&self.actual,&self.levels),
            None=>matching_member(&self.patterns, &self.actual),
        };
        // Unknown samples have no complete observation time. They are never
        // synthesized as Off, including non-queryable panel/motor endpoints.
        let observed = if self.actual.values().all(Option::is_some)
            && self.levels.values().all(Option::is_some)
            && self.lease.observations.values().all(Option::is_some)
        {
            self.lease.observations.values().filter_map(|v| *v).min()
        } else {
            None
        };
        Ok((selected, observed))
    }
}

/// Called on every public selection read, including before the first drive tick.
/// A stale/missing proof masks history immediately instead of awaiting polling.
#[allow(clippy::too_many_arguments)]
pub(crate) fn project(
    snapshot: &mut Snapshot,
    proof: Option<&Prepared>,
    group: &Group,
    store: &Store,
    config: &Configuration,
    document: &Document,
    boot: &str,
    epochs: Epochs,
    now: u64,
    ready: bool,
) -> Result<(), WriteStatus> {
    if !group.reconciliation.as_ref().is_some_and(|p| p.enabled) {
        return Ok(());
    }
    let mut relative_eligible=false;
    let mut selected = None;
    let mut observed_ms = None;
    let pending_relative=store.native_scene_pending(boot,epochs,&group.id).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
    if ready && !pending_relative && let Some(proof) = proof {
        match proof.validate(store, config, document, boot, epochs, now) {
            Ok((value, observed)) => {
                selected = value;
                relative_eligible=proof.relative_eligible;
                observed_ms = observed
            }
            Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {}
            Err(error) => return Err(error),
        }
    }
    snapshot.physical = Some(PhysicalProjection {
        boot_id: Uuid::parse_str(boot)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
            .as_u128(),
        epochs,
        observed_ms,
        history_value: snapshot.stored.value.clone(),
        pending_relative,
        relative_eligible,
    });
    snapshot.stored.value = selected.map(Value::Integer);
    Ok(())
}

pub async fn tick(
    admission: &Admission,
    controller: Uuid,
    document: &Document,
    scenes: &SceneScheduler,
    feedback: &Feedback,
) -> Result<(), String> {
    if !document
        .scene_switch_groups
        .iter()
        .any(|g| g.reconciliation.as_ref().is_some_and(|p| p.enabled))
        || !scenes.publication_impact().is_empty()
    {
        return Ok(());
    }
    let generation = match admission.task_generation().await {
        Ok(g) => g,
        Err(WriteStatus::ResourceBusy) => return Ok(()),
        Err(error) => return Err(format!("reconciliation scope: {error:?}")),
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
            Ok(Some(p)) => p,
            Ok(None) | Err(WriteStatus::ResourceBusy) => continue,
            Err(error) => return Err(format!("reconciliation read: {error:?}")),
        };
        match admission
            .reconcile_scene_selection(controller, generation.clone(), prepared)
            .await
        {
            Ok(()) | Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {}
            Err(error) => return Err(format!("reconciliation admission: {error:?}")),
        }
    }
    Ok(())
}
