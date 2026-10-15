//! Explicit relative scene operations, separate from absolute HAP writes.
//! All physical phases are consumed durably before queue admission. Queries
//! establish actual state, never an absolute acknowledgement for native Toggle.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
};
use hap::server::WriteStatus;
use homesense_config::{Configuration, rule_document::Document, scene_switches::Group};
use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Percent, queue::Epochs};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::{
    Store,
    native_scene::{Record, Request, Status},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone)]
struct Recipe {
    targets: BTreeMap<DeviceId, Command>,
    initial: Vec<(DeviceId, Command)>,
    physical: BTreeSet<DeviceId>,
    conditional: BTreeMap<DeviceId, (DeviceId, Percent)>,
    toggled_dimmers: BTreeSet<DeviceId>,
}
fn positive(command: Command) -> bool {
    matches!(command, Command::Switch(true))
        || matches!(command,Command::SetLevel{level,..} if level.get()>0)
}
fn absolute(module: &homesense_core::ModuleSpec, command: Command) -> Result<Command, WriteStatus> {
    match command {
        Command::Switch(_) => Ok(command),
        Command::SetLevel { level, .. } => crate::light_command::plan(
            module,
            Command::SetLevel {
                level,
                transition: LevelTransition::Immediate,
            },
        )
        .map_err(|_| WriteStatus::InvalidValueInRequest),
        _ => Err(WriteStatus::InvalidValueInRequest),
    }
}
fn build(
    config: &Configuration,
    group: &Group,
    document: &Document,
    scene: &str,
    powers: &BTreeMap<DeviceId, Option<bool>>,
) -> Result<Recipe, WriteStatus> {
    if !group.reconciliation.as_ref().is_some_and(|p| p.enabled) {
        return Err(WriteStatus::InvalidValueInRequest);
    }
    let member = group
        .members
        .iter()
        .find(|m| m.scene == scene)
        .ok_or(WriteStatus::InvalidValueInRequest)?;
    let mut vectors = group
        .absolute_light_targets(&document.timed_scenes)
        .map_err(|_| WriteStatus::InvalidValueInRequest)?;
    let targets = vectors
        .remove(&member.value)
        .ok_or(WriteStatus::InvalidValueInRequest)?;
    if targets.keys().ne(powers.keys()) {
        return Err(WriteStatus::InvalidValueInRequest);
    }
    let supplies = config
        .devices()
        .values()
        .filter_map(|d| d.supply_relay.as_ref())
        .collect::<BTreeSet<_>>();
    let mut initial = Vec::new();
    let mut seen = BTreeSet::new();
    let mut physical = BTreeSet::new();
    let mut conditional = BTreeMap::new();
    let mut toggled_dimmers = BTreeSet::new();
    let mut add = |id: DeviceId, command: Command| -> Result<(), WriteStatus> {
        if !seen.insert(id.clone()) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        initial.push((id, command));
        Ok(())
    };
    for (id, target) in &targets {
        if supplies.contains(id) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (device, module) = config
            .binding(id)
            .ok_or(WriteStatus::ResourceDoesNotExist)?;
        physical.insert(id.clone());
        if let Some(relay) = &device.supply_relay {
            physical.insert(relay.clone());
            let Command::SetLevel { level, .. } = *target else {
                return Err(WriteStatus::InvalidValueInRequest);
            };
            if !positive(*target) {
                add(relay.clone(), Command::Switch(false))?;
            } else if powers[id].is_none() {
                add(relay.clone(), Command::Toggle)?;
                conditional.insert(id.clone(), (relay.clone(), level));
            } else {
                for (output, command) in crate::combined_light::plan(
                    Command::SetLevel {
                        level,
                        transition: LevelTransition::Immediate,
                    },
                    None,
                    device.default_on_level,
                )
                .map_err(|_| WriteStatus::InvalidValueInRequest)?
                {
                    let physical_id = match output {
                        crate::combined_light::Output::Relay => relay.clone(),
                        crate::combined_light::Output::Dimmer => id.clone(),
                    };
                    add(physical_id, command)?;
                }
            }
        } else if positive(*target) && powers[id].is_none() {
            add(id.clone(), Command::Toggle)?;
            if module.kind() == ModuleKind::Dimmer {
                toggled_dimmers.insert(id.clone());
            }
        } else {
            add(id.clone(), absolute(module, *target)?)?;
        }
    }
    if physical.len() > 128 {
        return Err(WriteStatus::ResourceBusy);
    }
    Ok(Recipe {
        targets,
        initial,
        physical,
        conditional,
        toggled_dimmers,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Initial,
    RelayQueries,
    DimmerTargets,
    FinalQueries,
}
pub(crate) struct Submission {
    request: Request,
    generation: TaskGeneration,
    recipe: Recipe,
    powers: BTreeMap<DeviceId, Option<bool>>,
    kind: Kind,
    phase: u64,
    lease: RuleLease,
    captured_ms: u64,
    max_age_ms: u64,
}
pub(crate) struct Permit {
    pub request: Request,
    pub phase: u64,
    pub fence: u64,
    pub batch: Batch,
    pub lease: RuleLease,
}
impl Submission {
    pub(crate) fn request(&self) -> &Request {
        &self.request
    }
    pub(crate) fn generation(&self) -> &TaskGeneration {
        &self.generation
    }
    /// Actor-only checks, before consuming the next durable phase.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit(
        self,
        store: &mut Store,
        config: &Configuration,
        document: &Document,
        boot: &str,
        epochs: Epochs,
        now: u64,
        current_unknown: bool,
        manual: &BTreeMap<DeviceId, u64>,
    ) -> Result<Permit, WriteStatus> {
        if self.request.boot != boot
            || self.request.epochs != epochs
            || self.generation.epochs != epochs
            || now >= self.request.deadline_ms
            || now >= self.captured_ms.saturating_add(self.max_age_ms)
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let group = document
            .scene_switch_groups
            .iter()
            .find(|g| g.id == self.request.group && g.variable == self.request.variable)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let vectors = group
            .absolute_light_targets(&document.timed_scenes)
            .map_err(|_| WriteStatus::InvalidValueInRequest)?;
        let member = group
            .members
            .iter()
            .find(|m| m.scene == self.request.scene)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        if !group
            .reconciliation
            .as_ref()
            .is_some_and(|p| p.enabled && p.max_age_ms == self.max_age_ms)
            || vectors.get(&member.value) != Some(&self.recipe.targets)
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let _guard = self
            .lease
            .lock(
                Uuid::parse_str(boot)
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                    .as_u128(),
                document.revision,
                epochs.transport,
                now,
            )
            .map_err(|_| WriteStatus::ResourceBusy)?;
        let existing = store
            .native_scene_request(&self.request.controller, &self.request.key)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
        let fence = if self.kind == Kind::Initial {
            if !current_unknown || existing.is_some() {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            if let Some(after) = store
                .native_scene_failure_after(boot, epochs, &self.request.group)
                .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                && self
                    .lease
                    .observations
                    .values()
                    .any(|at| at.is_none_or(|at| at <= after))
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
            store
                .reserve_native_scene(&self.request, now)
                .map_err(|e| match e {
                    homesense_storage::Error::Conflict => WriteStatus::ResourceBusy,
                    homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                    _ => WriteStatus::ServiceCommunicationFailure,
                })?;
            0
        } else {
            let row = existing.ok_or(WriteStatus::InvalidValueInRequest)?;
            if row.request != self.request
                || row.phase != self.phase
                || row.status != Status::Pending
                || self
                    .recipe
                    .physical
                    .iter()
                    .any(|id| manual.get(id).is_some_and(|v| *v >= row.fence))
            {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            row.fence
        };
        if self.kind == Kind::DimmerTargets {
            for (id, observed) in &self.lease.observations {
                if !store
                    .feedback_is_settled_in_epoch(boot, id, *observed, epochs.transport)
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                {
                    return Err(WriteStatus::ResourceBusy);
                }
            }
        }
        let intents = match self.kind {
            Kind::Initial => self
                .recipe
                .initial
                .iter()
                .map(|(device, command)| Intent {
                    device: device.clone(),
                    command: *command,
                })
                .collect(),
            Kind::RelayQueries => self
                .recipe
                .conditional
                .values()
                .map(|(id, _)| id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|device| Intent {
                    device,
                    command: Command::Query,
                })
                .collect(),
            Kind::FinalQueries => self
                .recipe
                .physical
                .iter()
                .cloned()
                .map(|device| Intent {
                    device,
                    command: Command::Query,
                })
                .collect(),
            Kind::DimmerTargets => {
                let mut intents = vec![];
                for (id, (relay, level)) in &self.recipe.conditional {
                    match self.powers.get(relay) {
                        Some(Some(true)) => {
                            let (_, module) = config
                                .binding(id)
                                .ok_or(WriteStatus::ResourceDoesNotExist)?;
                            intents.push(Intent {
                                device: id.clone(),
                                command: absolute(
                                    module,
                                    Command::SetLevel {
                                        level: *level,
                                        transition: LevelTransition::Immediate,
                                    },
                                )?,
                            });
                        }
                        Some(Some(false)) => {}
                        _ => return Err(WriteStatus::ResourceBusy),
                    }
                }
                intents
            }
        };
        store
            .consume_native_phase(&self.request, self.phase, now)
            .map_err(|_| WriteStatus::InvalidValueInRequest)?;
        drop(_guard);
        Ok(Permit {
            request: self.request.clone(),
            phase: self.phase + 1,
            fence,
            batch: Batch {
                controller: Uuid::parse_str(&self.request.controller)
                    .map_err(|_| WriteStatus::InvalidValueInRequest)?,
                intents,
                subscriptions: vec![],
            },
            lease: self.lease,
        })
    }
}

pub(crate) struct Completion {
    pub request: Request,
    pub generation: TaskGeneration,
    pub phase: u64,
    pub projection: crate::scene_reconciliation::Prepared,
    pub levels: Vec<(DeviceId, u64, Percent)>,
    pub physical: BTreeSet<DeviceId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub key: String,
    pub scene: String,
    pub boot: String,
    pub status: String,
    pub selected: Option<bool>,
}
pub(crate) fn receipt(row: &Record, document: &Document) -> Receipt {
    let value = document
        .scene_switch_groups
        .iter()
        .flat_map(|g| &g.members)
        .find(|m| m.scene == row.request.scene)
        .map(|m| m.value);
    Receipt {
        key: row.request.key.clone(),
        scene: row.request.scene.clone(),
        boot: row.request.boot.clone(),
        status: match row.status {
            Status::Pending => "pending",
            Status::Observed => "observed",
            Status::Unknown => "unknown",
        }
        .into(),
        selected: row
            .selected
            .zip(value)
            .map(|(actual, value)| actual == value),
    }
}
enum Phase {
    Controls(Vec<u64>),
    RelayQuery {
        ids: Vec<u64>,
        before: BTreeMap<DeviceId, u64>,
    },
    DimmerControls(Vec<u64>),
    FinalQuery {
        ids: Vec<u64>,
        before: BTreeMap<DeviceId, u64>,
    },
}
struct Run {
    source:Option<crate::source_auth::Source>,
    request: Request,
    generation: TaskGeneration,
    recipe: Recipe,
    phase: u64,
    stage: Phase,
    toggles: BTreeMap<DeviceId, u64>,
    max_age: u64,
}
#[derive(Default)]
pub(crate) struct Manager {
    runs: BTreeMap<String, Run>,
}
impl Manager {
    #[allow(clippy::too_many_arguments)] // Trusted runtime context plus explicit scene/request identity.
    pub(crate) async fn start(
        &mut self,
        admission: &Admission,
        controller: Uuid,
        generation: TaskGeneration,
        document: &Document,
        scene: String,
        key: String,
        feedback: &Feedback,
    ) -> Result<Receipt, String> {
        if let Some(row) = admission
            .native_scene_status(controller, generation.clone(), key.clone())
            .await
            .map_err(|e| format!("{e:?}"))?
        {
            if row.request.scene != scene || row.request.epochs.configuration != document.revision {
                return Err("relative request identity conflict".into());
            }
            return Ok(receipt(&row, document));
        }
        if self.runs.len() >= 32 {
            return Err("relative scene capacity reached".into());
        }
        let bundle = admission
            .automation_bundle(controller, generation.clone())
            .await
            .map_err(|e| format!("{e:?}"))?;
        let group = document
            .scene_switch_groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.scene == scene))
            .ok_or("relative scene member missing")?;
        let policy = group
            .reconciliation
            .as_ref()
            .filter(|p| p.enabled)
            .ok_or("relative scene reconciliation disabled")?;
        let snapshot = admission
            .scene_selection_snapshot(controller, generation.clone(), vec![group.variable.clone()])
            .await
            .map_err(|e| format!("{e:?}"))?;
        if snapshot[&group.variable].stored.value.is_some() {
            return Err("relative scene selection is known; use explicit On/Off".into());
        }
        let targets = group
            .absolute_light_targets(&document.timed_scenes)
            .map_err(|e| e.to_string())?;
        let ids = targets
            .values()
            .next()
            .ok_or("relative targets missing")?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let now = admission.monotonic_ms();
        let (powers, _, lease) = feedback.native_scene_capture(&ids, now, policy.max_age_ms)?;
        let recipe = build(&bundle.configuration, group, document, &scene, &powers)
            .map_err(|e| format!("{e:?}"))?;
        let request = Request {
            controller: controller.to_string(),
            key: key.clone(),
            scene,
            group: group.id.clone(),
            variable: group.variable.clone(),
            variable_version: snapshot[&group.variable].stored.version,
            boot: admission.boot_id().into(),
            epochs: generation.epochs,
            deadline_ms: now.saturating_add(5000),
        };
        let submission = Submission {
            request: request.clone(),
            generation: generation.clone(),
            recipe: recipe.clone(),
            powers,
            kind: Kind::Initial,
            phase: 0,
            lease,
            captured_ms: now,
            max_age_ms: policy.max_age_ms,
        };
        let ids = admission
            .submit_native_scene(generation.clone(), submission)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let toggles = recipe
            .initial
            .iter()
            .zip(&ids)
            .filter(|((_, command), _)| *command == Command::Toggle)
            .map(|((device, _), id)| (device.clone(), *id))
            .collect();
        self.runs.insert(
            format!("{controller}:{key}"),
            Run {
                source:admission.source(),
                request,
                generation,
                recipe,
                phase: 1,
                stage: Phase::Controls(ids),
                toggles,
                max_age: policy.max_age_ms,
            },
        );
        let row = admission
            .native_scene_status(
                controller,
                self.runs[&format!("{controller}:{key}")].generation.clone(),
                key,
            )
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("relative receipt missing")?;
        Ok(receipt(&row, document))
    }
    pub(crate) async fn tick(
        &mut self,
        admission: &Admission,
        document: &Document,
        feedback: &Feedback,
    ) {
        let keys = self.runs.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let mut run = self.runs.remove(&key).unwrap();
            let scoped_admission=admission.with_source(run.source.clone());
            let outcome=if scoped_admission.source_valid(){run.advance(&scoped_admission,document,feedback).await}else{Err("source authority revoked before native phase".into())};
            match outcome {
                Ok(false) => {
                    self.runs.insert(key, run);
                }
                Ok(true) => {}
                Err(error) => {
                    log::warn!("relative scene outcome unknown: {error}");
                    let controller = Uuid::parse_str(&run.request.controller).unwrap();
                    let _ = admission
                        .fail_native_scene(
                            controller,
                            run.generation.clone(),
                            run.request.key.clone(),
                        )
                        .await;
                }
            }
        }
    }
}
impl Run {
    async fn written(&self, admission: &Admission, ids: &[u64]) -> Result<bool, String> {
        if ids.is_empty() {
            return Ok(true);
        }
        use homesense_storage::journal::Status as S;
        let values = admission
            .command_statuses(
                Uuid::parse_str(&self.request.controller).unwrap(),
                self.generation.clone(),
                ids.to_vec(),
            )
            .await
            .map_err(|e| format!("{e:?}"))?;
        if values
            .iter()
            .any(|s| !matches!(s, S::Accepted | S::Writing | S::Written))
        {
            return Err("relative phase failed or uncertain; no replay".into());
        }
        Ok(values.iter().all(|s| *s == S::Written))
    }
    fn counters(
        &self,
        feedback: &Feedback,
        ids: impl Iterator<Item = DeviceId>,
    ) -> Result<BTreeMap<DeviceId, u64>, String> {
        ids.map(|id| {
            feedback
                .observation_count(&id)
                .map(|count| (id, count))
                .ok_or("relative observation counter missing".into())
        })
        .collect()
    }
    async fn submit(
        &mut self,
        admission: &Admission,
        feedback: &Feedback,
        kind: Kind,
        powers: BTreeMap<DeviceId, Option<bool>>,
    ) -> Result<Vec<u64>, String> {
        let now = admission.monotonic_ms();
        let lease = if kind == Kind::DimmerTargets {
            feedback
                .native_scene_capture(
                    &self
                        .recipe
                        .conditional
                        .values()
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>(),
                    now,
                    self.max_age,
                )?
                .2
        } else {
            feedback
                .plan_rule(
                    &[],
                    &BTreeMap::new(),
                    homesense_core::rule::Budget::default(),
                    now,
                    self.max_age,
                )?
                .1
        };
        let submission = Submission {
            request: self.request.clone(),
            generation: self.generation.clone(),
            recipe: self.recipe.clone(),
            powers,
            kind,
            phase: self.phase,
            lease,
            captured_ms: now,
            max_age_ms: self.max_age,
        };
        let ids = admission
            .submit_native_scene(self.generation.clone(), submission)
            .await
            .map_err(|e| format!("{e:?}"))?;
        self.phase += 1;
        Ok(ids)
    }
    async fn advance(
        &mut self,
        admission: &Admission,
        document: &Document,
        feedback: &Feedback,
    ) -> Result<bool, String> {
        if admission.monotonic_ms() >= self.request.deadline_ms
            || admission
                .task_generation()
                .await
                .map_err(|e| format!("{e:?}"))?
                != self.generation
        {
            return Err("relative deadline or generation ended".into());
        }
        let row = admission
            .native_scene_status(
                Uuid::parse_str(&self.request.controller).unwrap(),
                self.generation.clone(),
                self.request.key.clone(),
            )
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("relative record missing")?;
        if row.status != Status::Pending {
            return Ok(true);
        }
        match &self.stage {
            Phase::Controls(ids) if self.written(admission, ids).await? => {
                if self.recipe.conditional.is_empty() {
                    self.final_queries(admission, feedback).await?;
                } else {
                    let before = self.counters(
                        feedback,
                        self.recipe.conditional.values().map(|(id, _)| id.clone()),
                    )?;
                    let ids = self
                        .submit(admission, feedback, Kind::RelayQueries, BTreeMap::new())
                        .await?;
                    self.stage = Phase::RelayQuery { ids, before };
                }
            }
            Phase::RelayQuery { ids, before } if self.written(admission, ids).await? => {
                if !before.iter().all(|(id, count)| {
                    feedback
                        .observation_count(id)
                        .is_some_and(|current| current > *count)
                }) {
                    return Ok(false);
                }
                let (powers, _, lease) = feedback.native_scene_capture(
                    &before.keys().cloned().collect::<Vec<_>>(),
                    admission.monotonic_ms(),
                    self.max_age,
                )?;
                if powers.values().any(Option::is_none)
                    || !admission
                        .feedback_settled(
                            Uuid::parse_str(&self.request.controller).unwrap(),
                            self.generation.clone(),
                            lease,
                        )
                        .await
                        .map_err(|e| format!("{e:?}"))?
                {
                    return Ok(false);
                }
                let ids = self
                    .submit(admission, feedback, Kind::DimmerTargets, powers)
                    .await?;
                self.stage = Phase::DimmerControls(ids);
            }
            Phase::DimmerControls(ids) if self.written(admission, ids).await? => {
                self.final_queries(admission, feedback).await?;
            }
            Phase::FinalQuery { ids, before } if self.written(admission, ids).await? => {
                if !before.iter().all(|(id, count)| {
                    feedback
                        .observation_count(id)
                        .is_some_and(|current| current > *count)
                }) {
                    return Ok(false);
                }
                let group = document
                    .scene_switch_groups
                    .iter()
                    .find(|g| g.id == self.request.group)
                    .ok_or("relative group removed")?;
                let controller = Uuid::parse_str(&self.request.controller).unwrap();
                let Some(projection) = crate::scene_reconciliation::Prepared::prepare(
                    admission,
                    controller,
                    self.generation.clone(),
                    group,
                    document,
                    feedback,
                )
                .await
                .map_err(|e| format!("{e:?}"))?
                else {
                    return Ok(false);
                };
                let (powers, levels, _) = feedback.native_scene_capture(
                    &self.recipe.targets.keys().cloned().collect::<Vec<_>>(),
                    admission.monotonic_ms(),
                    self.max_age,
                )?;
                if powers.values().any(Option::is_none) {
                    return Ok(false);
                }
                let levels = self
                    .recipe
                    .toggled_dimmers
                    .iter()
                    .map(|id| {
                        Ok((
                            id.clone(),
                            *self
                                .toggles
                                .get(id)
                                .ok_or("native toggle receipt missing")?,
                            *levels.get(id).ok_or("native dimmer reply missing")?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let completion = Completion {
                    request: self.request.clone(),
                    generation: self.generation.clone(),
                    phase: self.phase,
                    projection,
                    levels,
                    physical: self.recipe.physical.clone(),
                };
                match admission.finish_native_scene(controller, completion).await {
                    Ok(()) => return Ok(true),
                    Err(WriteStatus::ResourceBusy) => return Ok(false),
                    Err(error) => return Err(format!("relative finish: {error:?}")),
                }
            }
            _ => {}
        }
        Ok(false)
    }
    async fn final_queries(
        &mut self,
        admission: &Admission,
        feedback: &Feedback,
    ) -> Result<(), String> {
        let before = self.counters(feedback, self.recipe.physical.iter().cloned())?;
        let ids = self
            .submit(admission, feedback, Kind::FinalQueries, BTreeMap::new())
            .await?;
        self.stage = Phase::FinalQuery { ids, before };
        Ok(())
    }
}
