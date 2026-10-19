//! Read-only proof of an executed program's component state. This owner never
//! changes a logical variable, queues I/O, or grants a scene On/Off authority.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    source_auth::Source,
};
use hap::server::WriteStatus as W;
use homesense_config::{
    Configuration, component_modes::Postcondition, rule_document::Document,
    scene_component_activity::Policy,
};
use homesense_core::{
    Command, DeviceId, ModuleKind,
    queue::QueuedCommand,
    rule::{Action, Budget, Condition, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use homesense_storage::{
    Store,
    component_modes::Canonical,
    variables::{ReadCheck, Stored, Update},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) type Activity = Option<(crate::scene_activity::SceneActivity, Option<u64>)>;

#[derive(Clone)]
struct Guard {
    check: ReadCheck,
    value: Option<Value>,
}
pub(crate) struct Input {
    scenes: Vec<String>,
    reads: Vec<Guard>,
}
impl Input {
    pub(crate) fn capture(
        document: &Document,
        scenes: &[String],
        snapshot: &BTreeMap<String, Stored>,
        reads: &BTreeSet<String>,
    ) -> Option<Self> {
        if scenes.is_empty()
            || scenes.len() > 64
            || !document
                .scene_component_activity
                .values()
                .any(|p| p.enabled && p.program_confirmation.is_some())
        {
            return None;
        }
        let reads = snapshot
            .iter()
            .filter(|(name, s)| s.lifetime == Lifetime::Persistent && reads.contains(*name))
            .map(|(name, s)| Guard {
                check: ReadCheck {
                    revision: document.revision,
                    name: name.clone(),
                    expected_version: s.version,
                },
                value: s.value.clone(),
            })
            .collect::<Vec<_>>();
        (reads.len() <= 128).then(|| Self {
            scenes: scenes.to_vec(),
            reads,
        })
    }
}
struct Seed {
    policy: Policy,
    update: Update,
    decision: Vec<Guard>,
    continuing: Vec<Guard>,
    explicit: bool,
    prior: Vec<Postcondition>,
    prior_deadline: Option<u64>,
    inherited: Vec<(u64, DeviceId, u64)>,
}
pub(crate) struct Prepared {
    changed: BTreeSet<String>,
    seeds: Vec<Seed>,
    targets: Vec<Postcondition>,
}
impl Prepared {
    pub(crate) fn invalidate(updates: &[Update]) -> Self {
        Self {
            changed: updates.iter().map(|u| u.name.clone()).collect(),
            seeds: vec![],
            targets: vec![],
        }
    }
    pub(crate) fn capture(
        input: Option<&Input>,
        document: &Document,
        config: &Configuration,
        updates: &[Update],
        canonical: &[Canonical],
        tickets: &[QueuedCommand],
    ) -> Self {
        let changed = updates.iter().map(|u| u.name.clone()).collect();
        let mut result = Self {
            changed,
            seeds: vec![],
            targets: vec![],
        };
        let Some(input) = input else {
            return result;
        };
        let mut targets = BTreeMap::new();
        for ticket in tickets {
            if matches!(
                ticket.command,
                Command::PanelLed(_) | Command::PanelLedOff | Command::Query
            ) {
                continue;
            }
            let Some((_, module)) = config.binding(&ticket.device_id) else {
                return result;
            };
            let target = match (module.kind(), ticket.command) {
                (ModuleKind::Switch, Command::Switch(on)) => Postcondition {
                    device: ticket.device_id.as_str().to_owned(),
                    on,
                    level: None,
                },
                (ModuleKind::Dimmer, Command::SetLevel { level, .. }) => Postcondition {
                    device: ticket.device_id.as_str().to_owned(),
                    on: level.get() > 0,
                    level: (level.get() > 0).then_some(level.get()),
                },
                _ => return result,
            };
            targets.insert(ticket.device_id.clone(), target);
        }
        if targets.len() > 128 {
            return result;
        }
        result.targets = targets.into_values().collect();
        let mut decision = vec![];
        for read in &input.reads {
            let mut read = read.clone();
            if let Some(update) = updates.iter().find(|u| u.name == read.check.name) {
                if update.expected_version != read.check.expected_version {
                    return result;
                }
                let Some(version) = update.expected_version.checked_add(1) else {
                    return result;
                };
                read.check.expected_version = version;
                read.value = update.value.clone();
            }
            decision.push(read);
        }
        for policy in document
            .scene_component_activity
            .values()
            .filter(|p| p.enabled && p.program_confirmation.is_some())
        {
            let Some(update) = updates.iter().find(|u| u.name == policy.variable) else {
                continue;
            };
            let program = policy.program_confirmation.as_ref().unwrap();
            if !program
                .known_values
                .iter()
                .any(|v| Some(v.value()) == update.value)
                || !program.recorded_selection.as_ref().map_or_else(|| document.component_mode_confirmations.iter().any(|p| {
                    p.variable == policy.variable
                        && p.scenes.iter().any(|s| input.scenes.contains(s))
                }), |intent| intent.source_scenes.iter().any(|s| input.scenes.contains(s)))
            {
                continue;
            }
            let mut update = update.clone();
            let Some(version) = update.expected_version.checked_add(1) else {
                continue;
            };
            update.expected_version = version;
            let mut continuing = vec![Guard {
                check: ReadCheck {
                    revision: update.revision,
                    name: update.name.clone(),
                    expected_version: version,
                },
                value: update.value.clone(),
            }];
            if let Some(canonical) = canonical.iter().find(|c| c.update.name == policy.variable) {
                continuing.extend(canonical.sources.iter().map(|s| Guard {
                    check: s.check.clone(),
                    value: Some(s.value.clone()),
                }));
            }
            let mut decision = decision.clone();
            let mut consistent = true;
            for guard in &continuing {
                if let Some(previous) = decision.iter().find(|g| g.check.name == guard.check.name) {
                    if previous.check.expected_version != guard.check.expected_version
                        || previous.value != guard.value
                    {
                        consistent = false;
                        break;
                    }
                } else {
                    decision.push(guard.clone());
                }
            }
            if consistent {
                result.seeds.push(Seed {
                    policy: policy.clone(),
                    update,
                    decision,
                    continuing,
                    explicit: input.scenes.contains(&policy.scene),
                    prior: vec![],
                    prior_deadline: None,
                    inherited: vec![],
                });
            }
        }
        result
    }
    pub(crate) fn devices(&self, config: &Configuration) -> BTreeSet<DeviceId> {
        self.targets
            .iter()
            .chain(self.seeds.iter().flat_map(|s| {
                &s.policy
                    .program_confirmation
                    .as_ref()
                    .unwrap()
                    .active_conditions
            }))
            .filter_map(|c| DeviceId::new(&c.device).ok())
            .flat_map(|id| {
                let supply = config
                    .devices()
                    .get(&id)
                    .and_then(|d| d.supply_relay.clone());
                std::iter::once(id).chain(supply)
            })
            .collect()
    }
    pub(crate) fn decline(&mut self) {
        self.seeds.clear();
    }
    /// Untouched components require continuing prior ownership. A genuine
    /// explicit operation of this component may instead establish its own
    /// identity with a relevant completed write and the reviewed full vector.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inherit(
        &mut self,
        store: &Store,
        physical: &crate::component_modes::Owner,
        program: &Owner,
        config: &Configuration,
        generation: &TaskGeneration,
        now: u64,
        authorized: impl Fn(Uuid) -> bool,
    ) -> Result<(), W> {
        let touched = self
            .targets
            .iter()
            .map(|c| c.device.as_str())
            .collect::<BTreeSet<_>>();
        let mut retained = vec![];
        for mut seed in self.seeds.drain(..) {
            let conditions = &seed
                .policy
                .program_confirmation
                .as_ref()
                .unwrap()
                .active_conditions;
            let explicit = seed.explicit
                && conditions
                    .iter()
                    .any(|c| touched.contains(c.device.as_str()));
            let previous = program
                .conditions(store, &seed.policy.scene, generation, now, &authorized)?
                .or(physical.activity_conditions(
                    store,
                    &seed.policy.variable,
                    generation,
                    now,
                    &authorized,
                )?);
            let mut supported = true;
            if !explicit {
                for condition in conditions
                    .iter()
                    .filter(|c| !touched.contains(c.device.as_str()))
                {
                    if let Some((prior, deadline)) = &previous {
                        if let Some(prior) = prior.iter().find(|c| c.device == condition.device) {
                            seed.prior.push(prior.clone());
                            seed.prior_deadline =
                                Some(seed.prior_deadline.map_or(*deadline, |v| v.min(*deadline)));
                        } else {
                            supported = false;
                            break;
                        }
                    } else {
                        supported = false;
                        break;
                    }
                }
            }
            if supported {
                let ids = conditions
                    .iter()
                    .filter_map(|c| DeviceId::new(&c.device).ok())
                    .flat_map(|id| {
                        std::iter::once(id.clone()).chain(
                            config
                                .devices()
                                .get(&id)
                                .and_then(|d| d.supply_relay.clone()),
                        )
                    })
                    .collect::<BTreeSet<_>>();
                seed.inherited = program
                    .receipts
                    .values()
                    .flatten()
                    .filter(|(_, d, _)| ids.contains(d))
                    .cloned()
                    .collect();
                retained.push(seed);
            }
        }
        self.seeds = retained;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Actual {
    on: bool,
    level: Option<u8>,
}
#[derive(Clone)]
pub(crate) struct View {
    id: u64,
    generation: TaskGeneration,
    policy: Policy,
    targets: Vec<Postcondition>,
    after: BTreeMap<DeviceId, u64>,
    confirmed: bool,
    prior: Vec<Postcondition>,
}
struct Record {
    id: u64,
    generation: TaskGeneration,
    policy: Policy,
    update: Update,
    decision: Vec<Guard>,
    continuing: Vec<Guard>,
    controller: Uuid,
    source: Option<Source>,
    tickets: Vec<QueuedCommand>,
    written: BTreeMap<u64, u64>,
    write_devices: BTreeMap<u64, DeviceId>,
    targets: Vec<Postcondition>,
    manual: BTreeMap<DeviceId, u64>,
    primary: BTreeSet<DeviceId>,
    wires: Vec<homesense_core::gateway_scope::WireScope>,
    deadline: u64,
    signature: Option<Vec<Actual>>,
    prior: Vec<Postcondition>,
}
#[derive(Default)]
pub(crate) struct Owner {
    next: u64,
    records: BTreeMap<String, Record>,
    receipt_scope: Option<TaskGeneration>,
    tracked: BTreeSet<DeviceId>,
    writing: BTreeMap<u64, DeviceId>,
    receipts: BTreeMap<DeviceId, std::collections::VecDeque<(u64, DeviceId, u64)>>,
    policies: BTreeMap<String, Policy>,
}
fn guards_current(store: &Store, guards: &[Guard]) -> Result<bool, W> {
    for guard in guards {
        if store
            .logical_variable(guard.check.revision, &guard.check.name)
            .map_err(|_| W::ServiceCommunicationFailure)?
            .is_none_or(|s| s.version != guard.check.expected_version || s.value != guard.value)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
impl Owner {
    pub(crate) fn reset_recorded_for_gateway(
        &self, store: &mut Store, config: &Configuration, generation: &TaskGeneration,
        gateway: &str, now: u64,
    ) -> Result<(), W> {
        let mut targets = BTreeMap::new();
        for policy in self.policies.values().filter(|p| p.enabled) {
            let Some(program) = &policy.program_confirmation else { continue; };
            let Some(intent) = &program.recorded_selection else { continue; };
            if !program.active_conditions.iter().any(|c| DeviceId::new(&c.device).ok()
                .and_then(|id| config.devices().get(&id)).is_some_and(|d| d.gateway == gateway)) { continue; }
            for variable in &intent.reset_variables {
                targets.insert(variable.clone(), policy.scene.clone());
            }
        }
        if targets.is_empty() { return Ok(()); }
        store.ensure_component_mode_audit().map_err(|_| W::ServiceCommunicationFailure)?;
        let mut resets = vec![];
        for (name, policy) in targets {
            if let Some(value) = store.logical_variable(generation.epochs.configuration, &name)
                .map_err(|_| W::ServiceCommunicationFailure)?.filter(|v| v.value.is_some()) {
                resets.push((Update { revision: generation.epochs.configuration, name,
                    expected_version: value.version, value: None }, policy));
            }
        }
        for chunk in resets.chunks(64) {
            store.apply_component_mode_inference(chunk, &[], generation.boot_id(),
                "recorded-selection-gateway-reset", now, &[], false)
                .map_err(|_| W::ServiceCommunicationFailure)?;
        }
        Ok(())
    }
    pub(crate) fn configure(&mut self, document: &Document, config: &Configuration) {
        self.policies = document.scene_component_activity.clone();
        self.tracked = document
            .scene_component_activity
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.program_confirmation.as_ref())
            .flat_map(|p| &p.active_conditions)
            .filter_map(|c| DeviceId::new(&c.device).ok())
            .flat_map(|id| {
                std::iter::once(id.clone()).chain(
                    config
                        .devices()
                        .get(&id)
                        .and_then(|d| d.supply_relay.clone()),
                )
            })
            .collect();
    }
    pub(crate) fn dispatched(&mut self, command: &QueuedCommand) {
        if self.tracked.contains(&command.device_id)
            && self
                .receipt_scope
                .as_ref()
                .is_some_and(|g| g.epochs == command.epochs)
            && matches!(
                command.command,
                Command::Switch(_) | Command::SetLevel { .. } | Command::Toggle
            )
        {
            if self.writing.len() >= 128
                && let Some(id) = self.writing.keys().next().copied()
            {
                self.writing.remove(&id);
            }
            self.writing.insert(command.id, command.device_id.clone());
        }
    }
    pub(crate) fn unsent(&mut self, id: u64) {
        self.writing.remove(&id);
    }
    fn conditions(
        &self,
        store: &Store,
        scene: &str,
        generation: &TaskGeneration,
        now: u64,
        authorized: &impl Fn(Uuid) -> bool,
    ) -> Result<Option<(Vec<Postcondition>, u64)>, W> {
        let Some(r) = self.records.get(scene) else {
            return Ok(None);
        };
        if r.generation != *generation
            || now >= r.deadline
            || !authorized(r.controller)
            || r.source
                .as_ref()
                .is_some_and(|s| !s.valid_for(r.controller, generation.epochs.configuration))
            || !guards_current(store, &r.continuing)?
        {
            return Ok(None);
        }
        let Some(signature) = &r.signature else {
            return Ok(None);
        };
        Ok(Some((
            r.policy
                .program_confirmation
                .as_ref()
                .unwrap()
                .active_conditions
                .iter()
                .zip(signature)
                .map(|(c, a)| Postcondition {
                    device: c.device.clone(),
                    on: a.on,
                    level: a.level,
                })
                .collect(),
            r.deadline,
        )))
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(
        &mut self,
        prepared: Prepared,
        generation: TaskGeneration,
        controller: Uuid,
        source: Option<Source>,
        tickets: &[QueuedCommand],
        manual: &BTreeMap<DeviceId, u64>,
        config: &Configuration,
        wires: Vec<homesense_core::gateway_scope::WireScope>,
        deadline_limit: u64,
        now: u64,
    ) {
        self.records
            .retain(|_, r| !prepared.changed.contains(&r.update.name));
        let lighting = tickets
            .iter()
            .filter(|t| {
                prepared
                    .targets
                    .iter()
                    .any(|p| p.device == t.device_id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        for seed in prepared.seeds {
            let Some(id) = self.next.checked_add(1) else {
                self.records.clear();
                return;
            };
            self.next = id;
            let program = seed.policy.program_confirmation.as_ref().unwrap();
            let primary = program
                .active_conditions
                .iter()
                .filter_map(|c| DeviceId::new(&c.device).ok())
                .flat_map(|id| {
                    let supply = config
                        .devices()
                        .get(&id)
                        .and_then(|d| d.supply_relay.clone());
                    std::iter::once(id).chain(supply)
                })
                .collect::<BTreeSet<_>>();
            let dependencies = primary
                .iter()
                .cloned()
                .chain(
                    prepared
                        .targets
                        .iter()
                        .filter_map(|c| DeviceId::new(&c.device).ok())
                        .flat_map(|id| {
                            let supply = config
                                .devices()
                                .get(&id)
                                .and_then(|d| d.supply_relay.clone());
                            std::iter::once(id).chain(supply)
                        }),
                )
                .collect::<BTreeSet<_>>();
            // Sending TTL and feedback confirmation are distinct budgets.
            // The latter retains the operation's original, earlier limit.
            let deadline = now
                .saturating_add(program.timeout_ms)
                .min(deadline_limit)
                .min(seed.prior_deadline.unwrap_or(u64::MAX));
            let written = seed
                .inherited
                .iter()
                .map(|(id, _, at)| (*id, *at))
                .collect();
            let write_devices = seed
                .inherited
                .iter()
                .map(|(id, d, _)| (*id, d.clone()))
                .chain(lighting.iter().map(|t| (t.id, t.device_id.clone())))
                .collect();
            self.records.insert(
                seed.policy.scene.clone(),
                Record {
                    id,
                    generation: generation.clone(),
                    policy: seed.policy,
                    update: seed.update,
                    decision: seed.decision,
                    continuing: seed.continuing,
                    controller,
                    source: source.clone(),
                    tickets: lighting.clone(),
                    written,
                    write_devices,
                    targets: prepared.targets.clone(),
                    manual: dependencies
                        .into_iter()
                        .map(|d| {
                            let v = manual.get(&d).copied().unwrap_or(0);
                            (d, v)
                        })
                        .collect(),
                    primary,
                    wires: wires.clone(),
                    deadline,
                    signature: None,
                    prior: seed.prior,
                },
            );
        }
    }
    pub(crate) fn written(&mut self, id: u64, at: u64) {
        if let Some(device) = self.writing.remove(&id) {
            let ring = self.receipts.entry(device.clone()).or_default();
            ring.push_back((id, device, at));
            while ring.len() > 128 {
                ring.pop_front();
            }
        }
        for r in self.records.values_mut() {
            if r.tickets.iter().any(|t| t.id == id) {
                r.written.insert(id, at);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn audit(
        &mut self,
        store: &Store,
        generation: Option<&TaskGeneration>,
        manual: &BTreeMap<DeviceId, u64>,
        authorized: impl Fn(Uuid) -> bool,
        wire_ready: impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> Result<(), W> {
        if self.receipt_scope.as_ref() != generation {
            self.receipts.clear();
            self.writing.clear();
            self.receipt_scope = generation.cloned();
        }
        let mut retire = vec![];
        for (scene, r) in &self.records {
            let invalid = generation != Some(&r.generation)
                || now >= r.deadline
                || !authorized(r.controller)
                || r.source
                    .as_ref()
                    .is_some_and(|s| !s.valid_for(r.controller, r.generation.epochs.configuration))
                || r.manual
                    .iter()
                    .any(|(d, v)| manual.get(d).copied().unwrap_or(0) != *v)
                || r.wires.iter().any(|w| !wire_ready(w))
                || !guards_current(
                    store,
                    if r.signature.is_some() {
                        &r.continuing
                    } else {
                        &r.decision
                    },
                )?;
            let mut terminal_bad = false;
            for ticket in &r.tickets {
                if store
                    .component_mode_ticket(r.generation.boot_id(), ticket.id)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .is_none_or(|(s, _)| matches!(s.as_str(), "failed" | "cancelled" | "uncertain"))
                {
                    terminal_bad = true;
                    break;
                }
            }
            if invalid || terminal_bad {
                retire.push(scene.clone());
            }
        }
        for scene in retire {
            self.records.remove(&scene);
        }
        Ok(())
    }
    fn view(store: &Store, r: &Record) -> Result<Option<View>, W> {
        let mut after = BTreeMap::new();
        if r.signature.is_none() {
            for ticket in &r.tickets {
                let Some(at) = r.written.get(&ticket.id) else {
                    return Ok(None);
                };
                if !store
                    .component_mode_ticket(r.generation.boot_id(), ticket.id)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .is_some_and(|(s, _)| matches!(s.as_str(), "written" | "observed"))
                {
                    return Ok(None);
                }
                after
                    .entry(ticket.device_id.clone())
                    .and_modify(|v: &mut u64| *v = (*v).max(*at))
                    .or_insert(*at);
            }
        }
        Ok(Some(View {
            id: r.id,
            generation: r.generation.clone(),
            policy: r.policy.clone(),
            targets: if r.signature.is_some() {
                vec![]
            } else {
                r.targets.clone()
            },
            after,
            confirmed: r.signature.is_some(),
            prior: if r.signature.is_some() {
                vec![]
            } else {
                r.prior.clone()
            },
        }))
    }
    pub(crate) fn views(&self, store: &Store) -> Result<Vec<View>, W> {
        self.records
            .values()
            .filter_map(|r| Self::view(store, r).transpose())
            .collect()
    }
    pub(crate) fn read_view(&self, store: &Store, scene: &str) -> Result<Option<Option<View>>, W> {
        self.records
            .get(scene)
            .map(|r| {
                if r.signature.is_none() {
                    Ok(None)
                } else {
                    Self::view(store, r)
                }
            })
            .transpose()
    }
}

fn primary_sample(
    config: &Configuration,
    conditions: &[Postcondition],
    feedback: &Feedback,
    now: u64,
    age: u64,
) -> Result<(RuleLease, Option<Vec<Actual>>), String> {
    let mut variables = BTreeMap::new();
    let mut nodes = vec![];
    for (i, c) in conditions.iter().enumerate() {
        let id = DeviceId::new(&c.device).map_err(|e| e.to_string())?;
        let on = format!("program:{i}:on");
        let level = format!("program:{i}:level");
        variables.insert(
            on.clone(),
            Variable {
                value_type: ValueType::Bool,
                lifetime: Lifetime::Temporary,
                value: None,
            },
        );
        nodes.push(Node {
            source: format!("program/{i}/on"),
            action: Action::Set {
                variable: on.clone(),
                value: Expr::DeviceOn(id.clone()),
            },
        });
        if config
            .binding(&id)
            .is_some_and(|(_, m)| m.kind() == ModuleKind::Dimmer)
        {
            variables.insert(
                level.clone(),
                Variable {
                    value_type: ValueType::Integer,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("program/{i}/level-when-on"),
                action: Action::If {
                    condition: Condition::Equal(
                        Expr::Variable(on),
                        Expr::Literal(Value::Bool(true)),
                    ),
                    then_actions: vec![Node {
                        source: format!("program/{i}/level"),
                        action: Action::Set {
                            variable: level,
                            value: Expr::DeviceLevel(id),
                        },
                    }],
                    else_actions: vec![],
                },
            });
        }
    }
    let (plan, lease) = feedback.plan_rule(&nodes, &variables, Budget::default(), now, age)?;
    let mut actual = vec![];
    for (i, c) in conditions.iter().enumerate() {
        let Some(Value::Bool(on)) = plan.variables[&format!("program:{i}:on")].value else {
            return Ok((lease, None));
        };
        let level = if on
            && config
                .binding(&DeviceId::new(&c.device).map_err(|e| e.to_string())?)
                .is_some_and(|(_, m)| m.kind() == ModuleKind::Dimmer)
        {
            match plan.variables[&format!("program:{i}:level")].value {
                Some(Value::Integer(v)) => u8::try_from(v).ok(),
                _ => return Ok((lease, None)),
            }
        } else {
            None
        };
        if on && c.level.is_some() && level.is_none() {
            return Ok((lease, None));
        }
        actual.push(Actual { on, level });
    }
    Ok((lease, Some(actual)))
}
pub(crate) struct Proof {
    id: u64,
    pub generation: TaskGeneration,
    confirmed: bool,
    lease: RuleLease,
    signature: Option<Vec<Actual>>,
    cohort: Option<bool>,
    valid_until: u64,
    negative_policy: Option<Policy>,
}
impl Proof {
    /// A fresh negative observation may disprove an unowned selection. It can
    /// never reconstruct Active or grant control from a saved intent label.
    pub(crate) fn prepare_negative(
        policy: &Policy, config: &Configuration, feedback: &Feedback,
        generation: TaskGeneration, now: u64,
    ) -> Result<Self, String> {
        let program = policy.program_confirmation.as_ref().ok_or("missing program")?;
        if program.recorded_selection.is_none() { return Err("not recorded selection".into()); }
        let age = feedback.effective_max_age_ms(program.max_age_ms);
        let (lease, signature) = primary_sample(config, &program.active_conditions, feedback, now, age)?;
        let valid_until = lease.observations.values().filter_map(|v| *v).min().unwrap_or(now).saturating_add(age);
        Ok(Self { id: 0, generation, confirmed: false, lease, signature,
            cohort: None, valid_until, negative_policy: Some(policy.clone()) })
    }
    pub(crate) fn prepare(
        view: &View,
        config: &Configuration,
        feedback: &Feedback,
        now: u64,
    ) -> Result<Self, String> {
        let program = view
            .policy
            .program_confirmation
            .as_ref()
            .ok_or("missing program policy")?;
        let requested=if view.confirmed {feedback.continuing_max_age_ms(program.max_age_ms)} else {program.max_age_ms};
        let age = feedback.effective_max_age_ms(requested);
        let (primary, signature) =
            primary_sample(config, &program.active_conditions, feedback, now, age)?;
        let valid_until = primary
            .observations
            .values()
            .filter_map(|v| *v)
            .min()
            .unwrap_or(now)
            .saturating_add(age);
        let (mut lease, mut cohort) = if view.targets.is_empty() {
            (primary, Some(true))
        } else {
            let refs = view.targets.iter().collect::<Vec<_>>();
            let (lease, values) =
                crate::component_modes::sample_conditions(config, &refs, feedback, now, age)?;
            let after = view.after.iter().all(|(d, w)| {
                lease
                    .observations
                    .get(d)
                    .is_some_and(|at| at.is_some_and(|at| at > *w))
            });
            let matched = if !after || values.iter().any(Option::is_none) {
                None
            } else {
                Some(values.iter().all(|v| *v == Some(true)))
            };
            (primary.merge_physical(&lease)?, matched)
        };
        if !view.prior.is_empty() {
            let refs = view.prior.iter().collect::<Vec<_>>();
            let (prior, values) =
                crate::component_modes::sample_conditions(config, &refs, feedback, now, age)?;
            cohort = match cohort {
                Some(true) if values.iter().all(|v| *v == Some(true)) => Some(true),
                _ => None,
            };
            lease = lease.merge_physical(&prior)?;
        }
        Ok(Self {
            id: view.id,
            generation: view.generation.clone(),
            confirmed: view.confirmed,
            lease,
            signature,
            cohort,
            valid_until,
            negative_policy: None,
        })
    }
}
impl Owner {
    fn validate_proof(store: &Store, r: &Record, proof: &Proof, now: u64) -> Result<bool, W> {
        if proof.negative_policy.is_some() || proof.id != r.id
            || proof.generation != r.generation
            || proof.confirmed != r.signature.is_some()
            || now >= r.deadline
            || now >= proof.valid_until
            || r.source
                .as_ref()
                .is_some_and(|s| !s.valid_for(r.controller, r.generation.epochs.configuration))
            || proof.cohort != Some(true)
            || proof.signature.is_none()
            || !guards_current(
                store,
                if r.signature.is_some() {
                    &r.continuing
                } else {
                    &r.decision
                },
            )?
        {
            return Ok(false);
        }
        let Ok(_lock) = proof.lease.lock(
            Uuid::parse_str(r.generation.boot_id())
                .map_err(|_| W::ServiceCommunicationFailure)?
                .as_u128(),
            r.generation.epochs.configuration,
            r.generation.epochs.transport,
            now,
        ) else {
            return Ok(false);
        };
        for (d, at) in &proof.lease.observations {
            if !store
                .component_mode_feedback_settled(
                    r.generation.boot_id(),
                    d,
                    *at,
                    r.generation.epochs.transport,
                    &r.written,
                )
                .map_err(|_| W::ServiceCommunicationFailure)?
            {
                return Ok(false);
            }
        }
        if r.signature.is_none() {
            for ticket in &r.tickets {
                if !r.written.contains_key(&ticket.id)
                    || !store
                        .component_mode_ticket(r.generation.boot_id(), ticket.id)
                        .map_err(|_| W::ServiceCommunicationFailure)?
                        .is_some_and(|(s, _)| matches!(s.as_str(), "written" | "observed"))
                {
                    return Ok(false);
                }
            }
        }
        let signature = proof.signature.as_ref().unwrap();
        if let Some(expected) = &r.signature {
            return Ok(signature == expected);
        }
        if r.update.value == Some(r.policy.active_value.value()) {
            let conditions = &r
                .policy
                .program_confirmation
                .as_ref()
                .unwrap()
                .active_conditions;
            if !conditions
                .iter()
                .zip(signature)
                .all(|(c, a)| a.on == c.on && c.level.is_none_or(|v| a.level == Some(v)))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(crate) fn apply(
        &mut self,
        store: &Store,
        config: &Configuration,
        proof: Proof,
        now: u64,
    ) -> Result<bool, W> {
        let Some(scene) = self
            .records
            .iter()
            .find(|(_, r)| r.id == proof.id)
            .map(|(s, _)| s.clone())
        else {
            return Ok(false);
        };
        let r = &self.records[&scene];
        if !Self::validate_proof(store, r, &proof, now)? {
            // A transient unreadable/stale lease cannot retire a newer fact.
            // A current changed physical signature does retire this lineage.
            if proof.generation == r.generation
                && proof.confirmed == r.signature.is_some()
                && r.signature.is_some()
                && proof.signature.is_some()
                && proof.signature != r.signature
                && proof
                    .lease
                    .lock(
                        Uuid::parse_str(r.generation.boot_id())
                            .map_err(|_| W::ServiceCommunicationFailure)?
                            .as_u128(),
                        r.generation.epochs.configuration,
                        r.generation.epochs.transport,
                        now,
                    )
                    .is_ok()
            {
                self.records.remove(&scene);
            }
            return Ok(false);
        }
        let Ok(_lock) = proof.lease.lock(
            Uuid::parse_str(r.generation.boot_id())
                .map_err(|_| W::ServiceCommunicationFailure)?
                .as_u128(),
            r.generation.epochs.configuration,
            r.generation.epochs.transport,
            now,
        ) else {
            return Ok(false);
        };
        let source = r.source.clone();
        Ok(crate::source_auth::commit(source.as_ref(), || {
            let r = self.records.get_mut(&scene).unwrap();
            r.signature = proof.signature;
            r.deadline = proof.valid_until;
            r.manual.retain(|d, _| r.primary.contains(d));
            r.tickets.retain(|t| r.primary.contains(&t.device_id));
            r.written.retain(|id, _| {
                r.write_devices
                    .get(id)
                    .is_some_and(|d| r.primary.contains(d))
            });
            r.write_devices.retain(|id, _| r.written.contains_key(id));
            let gateways = r
                .primary
                .iter()
                .filter_map(|d| config.devices().get(d).map(|d| d.gateway.as_str()))
                .collect::<BTreeSet<_>>();
            r.wires.retain(|w| gateways.contains(w.gateway()));
            true
        })
        .unwrap_or(false))
    }
    pub(crate) fn activity(
        &self,
        store: &Store,
        scene: &str,
        proof: Option<Proof>,
        now: u64,
    ) -> Result<Option<(crate::scene_activity::SceneActivity, Option<u64>)>, W> {
        use crate::scene_activity::SceneActivity as A;
        let Some(r) = self.records.get(scene) else {
            let Some(proof) = proof else { return Ok(None); };
            let Some(policy) = self.policies.get(scene).filter(|p| p.enabled) else { return Ok(None); };
            if proof.negative_policy.as_ref() != Some(policy) || now >= proof.valid_until {
                return Ok(None);
            }
            let program = policy.program_confirmation.as_ref().unwrap();
            if program.recorded_selection.is_none() { return Ok(None); }
            let Ok(_guard) = proof.lease.lock(
                Uuid::parse_str(proof.generation.boot_id()).map_err(|_| W::ServiceCommunicationFailure)?.as_u128(),
                proof.generation.epochs.configuration, proof.generation.epochs.transport, now,
            ) else { return Ok(Some((A::Unknown, None))); };
            for (device, at) in &proof.lease.observations {
                if !store.component_mode_feedback_settled(proof.generation.boot_id(), device, *at,
                    proof.generation.epochs.transport, &BTreeMap::new()).map_err(|_| W::ServiceCommunicationFailure)? {
                    return Ok(Some((A::Pending, None)));
                }
            }
            let Some(signature) = proof.signature else { return Ok(Some((A::Unknown, None))); };
            let mismatch = program.active_conditions.iter().zip(&signature)
                .any(|(c, a)| a.on != c.on || c.level.is_some_and(|v| a.level != Some(v)));
            let value = store.logical_variable(proof.generation.epochs.configuration, &policy.variable)
                .map_err(|_| W::ServiceCommunicationFailure)?.and_then(|v| v.value);
            let negative_intent = value.as_ref().is_some_and(|v| *v != policy.active_value.value()
                && program.known_values.iter().any(|known| known.value() == *v));
            return Ok(Some((if mismatch || negative_intent { A::Inactive } else { A::Unknown }, Some(proof.valid_until))));
        };
        if now >= r.deadline {
            return Ok(Some((A::Unknown, None)));
        }
        if r.signature.is_none() {
            return Ok(Some((A::Pending, None)));
        }
        let Some(proof) = proof else {
            return Ok(Some((A::Unknown, None)));
        };
        if !Self::validate_proof(store, r, &proof, now)? {
            return Ok(Some((A::Unknown, None)));
        }
        Ok(Some((
            if r.update.value == Some(r.policy.active_value.value()) {
                A::Active
            } else {
                A::Inactive
            },
            Some(r.deadline.min(proof.valid_until)),
        )))
    }
}
#[derive(Default)]
pub(crate) struct Consumer {
    next: usize,
}
impl Consumer {
    pub(crate) async fn tick(
        &mut self,
        admission: &Admission,
        controller: Uuid,
        document: &Document,
        feedback: &Feedback,
    ) -> Result<(), String> {
        if !document
            .scene_component_activity
            .values()
            .any(|p| p.enabled && p.program_confirmation.is_some())
        {
            return Ok(());
        }
        let generation = match admission.task_generation().await {
            Ok(g) => g,
            Err(_) => return Ok(()),
        };
        let views = match admission
            .program_activity_views(controller, generation.clone())
            .await
        {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        if views.is_empty() {
            return Ok(());
        }
        let bundle = match admission.automation_bundle(controller, generation).await {
            Ok(bundle) => bundle,
            Err(_) => return Ok(()),
        };
        for i in 0..views.len().min(4) {
            let view = &views[(self.next + i) % views.len()];
            let Ok(proof) = Proof::prepare(
                view,
                &bundle.configuration,
                feedback,
                admission.monotonic_ms(),
            ) else {
                continue;
            };
            let _ = admission.program_activity_proof(controller, proof).await;
        }
        self.next = (self.next + 4) % views.len();
        Ok(())
    }
}
