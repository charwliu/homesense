//! Boot-scoped physical writer phases. A phase is one already evaluated admission,
//! never inferred from two absent run IDs. Linked LEDs wait for genuine raw
//! post-write feedback. The authenticated origin-key cue shares its lighting
//! write cohort while retaining its original short lease;
//! this owner does not evaluate scenes, change variables, or query I/O.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, SceneObservationStart},
    source_auth::Source,
};
use hap::server::WriteStatus as W;
use homesense_config::Configuration;
use homesense_core::{Command, DeviceId, ModuleKind, queue::QueuedCommand};
use homesense_storage::{Store, journal::Status, variables::ReadCheck};
use std::collections::BTreeMap;
use uuid::Uuid;
pub(crate) const FEEDBACK_TOTAL_MS: u64 = 15_000;
const LED_DELIVERY_MS: u64 = 2_000;
pub(crate) const FEEDBACK_SAMPLE_MS: u64 = 5_000;

// A real post-write On is already a failed absolute-Off result. Its owner
// retries the module; re-querying that same failure is not an Off proof.
fn away_failed_sample(state:&homesense_state::DeviceState,after:u64,now:u64)->bool {
    state.quality==homesense_state::Quality::Confirmed
        && state.observed_at_ms.is_some_and(|at|at>after && at<=now)
        && state.observed.is_some_and(|observation|match observation {
            homesense_core::Observation::Switch(on)=>on,
            homesense_core::Observation::Level(level)=>level.get()>0,
            _=>false,
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetRole {
    Ordinary,
    Lighting,
    SequencedLighting,
    FeedbackLed,
}
/// Only a contiguous mixed lighting phase owns a feedback budget.
/// LED-only writes and non-lighting commands retain ordinary admission TTLs.
#[cfg(test)]
pub(crate) fn feedback_roles(
    config: &Configuration,
    intents: &[homesense_hap::requests::Intent],
) -> Vec<BudgetRole> {
    feedback_roles_local(config,intents,None)
}
pub(crate) fn feedback_roles_local(config:&Configuration,intents:&[homesense_hap::requests::Intent],local:Option<usize>)->Vec<BudgetRole> {
    let mut result = vec![BudgetRole::Ordinary; intents.len()];
    let mut start = 0;
    while start < intents.len() {
        let mut end = start;
        while end < intents.len()
            && class(config, &intents[end].device, intents[end].command).is_some()
        {
            end += 1;
        }
        if end == start {
            start += 1;
            continue;
        }
        if intents[start..end]
            .iter()
            .any(|i| !class(config, &i.device, i.command).unwrap().1)
            && intents[start..end]
                .iter()
                .enumerate().any(|(offset,i)|Some(start+offset)!=local && class(config, &i.device, i.command).unwrap().1)
        {
            let sequenced = sequence_modules(config,intents[start..end].iter().map(|i|(&i.device,i.command)));
            for (offset,(role, intent)) in result[start..end].iter_mut().zip(&intents[start..end]).enumerate() {
                *role = if Some(start+offset)==local {BudgetRole::Ordinary} else if class(config, &intent.device, intent.command).unwrap().1 {
                    BudgetRole::FeedbackLed
                } else if sequenced {BudgetRole::SequencedLighting} else {BudgetRole::Lighting};
            }
        }
        start = end;
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Phase {
    pub group: u64,
    pub gateway: String,
    pub led: bool,
    pub gate: Option<u64>,
}
pub(crate) fn class(
    config: &Configuration,
    id: &DeviceId,
    command: Command,
) -> Option<(String, bool)> {
    let (device, module) = config.binding(id)?;
    match command {
        Command::PanelLed(_) | Command::PanelLedOff => Some((device.gateway.clone(), true)),
        Command::Switch(_) if module.kind() == ModuleKind::Switch => {
            Some((device.gateway.clone(), false))
        }
        Command::SetLevel { .. } if module.kind() == ModuleKind::Dimmer => {
            Some((device.gateway.clone(), false))
        }
        _ => None,
    }
}
/// Only unambiguous absolute targets may acquire module confirmation stages.
/// An ordered program with different values for the same output must not gain
/// waits for intermediate values which it never promised to hold.
fn sequence_modules<'a>(config:&Configuration,commands:impl Iterator<Item=(&'a DeviceId,Command)>)->bool {
    let mut values=BTreeMap::new();let mut modules=BTreeMap::<String,std::collections::BTreeSet<_>>::new();let mut opening=false;
    for(id,command)in commands {
        let Some((gateway,false))=class(config,id,command) else{continue;};
        if matches!(command,Command::SetLevel{transition:homesense_core::LevelTransition::Ramp,..}) {return false;}
        if values.insert(id,command).is_some_and(|old|old!=command) {return false;}
        opening |= match command {Command::Switch(on)=>on,Command::SetLevel{level,..}=>level.get()>0,_=>false};
        if let Some((_,module))=config.binding(id) {modules.entry(gateway).or_default().insert(module.address());}
    }
    // Absolute all-Off must continue issuing every original Off even when an
    // earlier module is unavailable; its existing final-feedback gate remains.
    opening && modules.values().any(|modules|modules.len()>1)
}
/// Retain the final origin-key color in one evaluated immediate segment.
/// The caller keeps timeline step receipts outside this transformation.
pub(crate) fn coalesce_local_cue(
    config: &Configuration,
    intents: &mut Vec<homesense_hap::requests::Intent>,
    sources: &mut Option<Vec<String>>,
    local: Option<usize>,
) -> Option<usize> {
    let local = local?;
    let endpoint = intents.get(local)?.device.clone();
    if !class(config, &endpoint, intents[local].command).is_some_and(|(_, led)| led)
        || sources.as_ref().is_some_and(|labels| labels.len() != intents.len())
    {
        return Some(local);
    }
    // Promoting the final color must not leave an earlier color queued behind
    // it. Restrict last-value coalescing to this evaluated lighting segment;
    // queries, motor/STOP, parameters and other causal barriers stay intact.
    let start = (0..local).rev()
        .find(|i| class(config, &intents[*i].device, intents[*i].command).is_none())
        .map_or(0, |i| i + 1);
    let keep = intents.iter().enumerate().map(|(i, intent)| {
        !(start <= i && i < local && intent.device == endpoint
            && matches!(intent.command, Command::PanelLed(_) | Command::PanelLedOff))
    }).collect::<Vec<_>>();
    let next_local = keep[..local].iter().filter(|keep| **keep).count();
    let mut i = 0;
    intents.retain(|_| { let retained = keep[i]; i += 1; retained });
    if let Some(labels) = sources {
        let mut i = 0;
        labels.retain(|_| { let retained = keep[i]; i += 1; retained });
    }
    Some(next_local)
}

/// Stable partition within one already evaluated due phase. Motor/STOP,
/// parameters and queries remain causal barriers; gateway writes stay separate.
/// Sources are permuted before durable command IDs are allocated.
pub(crate) fn partition_local(config:&Configuration,intents:&mut [homesense_hap::requests::Intent],sources:&mut Option<Vec<String>>,local:Option<usize>)->Vec<usize> {
    let mut permutation = (0..intents.len()).collect::<Vec<_>>();
    let mut start = 0;
    while start < intents.len() {
        let Some(_) = class(config, &intents[start].device, intents[start].command) else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < intents.len()
            && class(config, &intents[end].device, intents[end].command).is_some()
        {
            end += 1;
        }
        let mut indices = (start..end).collect::<Vec<_>>();
        indices.sort_by_key(|i| {
            if Some(*i)==local {0} else if class(config, &intents[*i].device, intents[*i].command).unwrap().1 {2} else {1}
        });
        let old = indices.iter().map(|i| permutation[*i]).collect::<Vec<_>>();
        for (i, value) in (start..end).zip(old) {
            permutation[i] = value;
        }
        let values = indices
            .iter()
            .map(|i| homesense_hap::requests::Intent {
                device: intents[*i].device.clone(),
                command: intents[*i].command,
            })
            .collect::<Vec<_>>();
        for (i, intent) in (start..end).zip(values) {
            intents[i] = intent;
        }
        if let Some(labels) = sources {
            let values = indices
                .iter()
                .map(|i| labels[*i].clone())
                .collect::<Vec<_>>();
            for (i, value) in (start..end).zip(values) {
                labels[i] = value;
            }
        }
        start = end;
    }
    permutation
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    id: u64,
    generation: TaskGeneration,
    targets: BTreeMap<DeviceId, (Command, u64)>,
    deadline: u64,
    condition: Option<Condition>,
    managed_away: bool,
    stage: Option<usize>,
}
/// A retained predicate, rather than a frozen dimmer value. Refreshing its
/// witness never queries a device and never changes the accepted LED budget.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Condition {
    postconditions: Vec<homesense_config::component_modes::Postcondition>,
    max_age_ms: u64,
    after: BTreeMap<DeviceId, u64>,
    command_fence: u64,
    owned: std::collections::BTreeSet<u64>,
}
impl View {
    // Keep genuine early replies for the owning operation's bounded window.
    // The separate periodic-query freshness policy does not shorten this proof.
    fn feedback_max_age_ms(&self) -> u64 {
        if self.managed_away {
            homesense_core::away_execution::Policy::default().feedback_ms
        } else {
            FEEDBACK_TOTAL_MS
        }
    }
    /// Pending LED proof may outlive an early relay reply while an AD8 ramps.
    /// Refresh only that proof's physical dependencies; never extend its lease.
    pub(crate) fn query_requirements(&self, config:&Configuration, feedback:&Feedback, targets:&[crate::physical_queries::Target], now:u64)->Vec<(usize,u64)> {
        if now>=self.deadline || self.condition.is_some() {return vec![];}
        let mut ports=BTreeMap::new();
        for (id,(command,after)) in &self.targets {
            let Some((binding,module))=config.binding(id) else {continue;};
            ports.insert((binding.gateway.clone(),module.address().get(),binding.channel.get()),(*command,*after));
            if let Command::SetLevel {level,..}=command && let Some(supply)=&binding.supply_relay
                && let Some((relay,relay_module))=config.binding(supply) {
                if level.get()==0 && !self.managed_away {ports.remove(&(binding.gateway.clone(),module.address().get(),binding.channel.get()));}
                ports.entry((relay.gateway.clone(),relay_module.address().get(),relay.channel.get())).or_insert((Command::Switch(level.get()>0),*after));
            }
        }
        targets.iter().enumerate().filter_map(|(index,target)| {
            let needed=target.channels.iter().any(|channel| {
                let key=(target.gateway.clone(),target.module.address().get(),*channel);
                let Some((_,after))=ports.get(&key) else{return false;};
                feedback.module_truth().received_port_state(&key.0,key.1,key.2).is_none_or(|state| {
                    if self.managed_away && away_failed_sample(&state,*after,now) {return false;}
                    state.quality!=homesense_state::Quality::Confirmed
                    ||state.observed.is_none()
                    // A fresh mismatching reply is still a completed read.
                    // The proof below retains exact target matching; querying
                    // this module every turn cannot repair a failed write and
                    // consumes other modules' confirmation slots. Revisit it
                    // when its sample ages, without renewing the LED budget.
                    ||!state.observed_at_ms.is_some_and(|at|at>*after && at<=now && now-at<self.feedback_max_age_ms()/2)
                })
            });
            needed.then_some((index,self.deadline))
        }).collect()
    }
}

pub(crate) async fn query_requirements(admission:&Admission,feedback:&Feedback,config:&Configuration,targets:&[crate::physical_queries::Target],controller:Uuid)->Result<BTreeMap<usize,u64>,String> {
    let generation=match admission.task_generation().await {Ok(value)=>value,Err(W::ResourceBusy|W::ServiceCommunicationFailure)=>return Ok(BTreeMap::new()),Err(e)=>return Err(format!("LED query generation: {e:?}"))};
    let views=match admission.control_batch_views(controller,generation.clone()).await {Ok(views)=>views,Err(W::ResourceBusy)=>return Ok(BTreeMap::new()),Err(e)=>return Err(format!("LED query requirements: {e:?}"))};
    let mut needed=BTreeMap::<usize,u64>::new();
    let components=match admission.component_mode_views(controller,generation).await {
        Ok(views)=>views,Err(W::ResourceBusy)=>vec![],Err(e)=>return Err(format!("component confirmation reads: {e:?}")),
    };
    for view in components {for (index,deadline) in view.query_requirements(config,feedback,targets,admission.monotonic_ms()) {
        needed.entry(index).and_modify(|old|*old=(*old).max(deadline)).or_insert(deadline);
    }}

    if admission.away_operations_active() || admission.household_operations_active() {
        let generation=match admission.task_generation().await {Ok(value)=>value,Err(_)=>return Ok(needed)};
        let reads=admission.away_read_needs(controller,generation).await.map_err(|e|format!("Away confirmation reads: {e:?}"))?;
        let now=admission.monotonic_ms();
        for read in reads {
            let Some((device,_))=config.binding(&read.device) else {continue;};
            let state=feedback.module_truth().located_state(&device.gateway,device.address.get(),device.channel.get());
            let count=feedback.module_truth().port_observation_count(&device.gateway,device.address.get(),device.channel.get()).unwrap_or(0);
            let received=count>read.before_count && state.as_ref().is_some_and(|state|state.quality==homesense_state::Quality::Confirmed
                && state.observed.is_some() && state.observed_at_ms.is_some_and(|at|at>=read.after_ms && now.checked_sub(at).is_some_and(|age|age<homesense_core::away_execution::Policy::default().feedback_ms || away_failed_sample(state,read.after_ms,now))));
            if received || now>=read.deadline_ms {continue;}
            for(index,target)in targets.iter().enumerate(){if target.gateway==device.gateway && target.module.address()==device.address && target.channels.contains(&device.channel.get()) {
                needed.entry(index).and_modify(|old|*old=(*old).max(read.deadline_ms)).or_insert(read.deadline_ms);
            }}
        }
    }
    for view in views {for (index,deadline) in view.query_requirements(config,feedback,targets,admission.monotonic_ms()) {
        needed.entry(index).and_modify(|old|*old=(*old).max(deadline)).or_insert(deadline);
    }}
    Ok(needed)
}
struct Stage {
    group: u64,
    commands: Vec<QueuedCommand>,
    opened: Option<u64>,
    confirmed: bool,
}
struct Gate {
    generation: TaskGeneration,
    controller: Uuid,
    source: Option<Source>,
    away: Option<crate::away_auth::Authorization>,
    lights: Vec<QueuedCommand>,
    leds: Vec<u64>,
    targets: BTreeMap<DeviceId, Command>,
    target_ids: BTreeMap<DeviceId, u64>,
    written: BTreeMap<u64, u64>,
    guards: Vec<ReadCheck>,
    manual: BTreeMap<DeviceId, u64>,
    deadline: u64,
    delivery_deadline: Option<u64>,
    ready: Option<BarrierLease>,
    invalid: bool,
    cancel_reason: homesense_core::queue::CancelReason,
    condition: Option<Condition>,
    stages: Vec<Stage>,
    write_window_ms: u64,
}
impl Gate {
    fn view(&self, id: u64) -> Option<View> {
        if self.invalid {return None;}
        if let Some((index,stage))=self.stages.iter().enumerate().find(|(_,stage)|!stage.confirmed) {
            if stage.opened.is_none() || stage.commands.iter().any(|command|!self.written.contains_key(&command.id)) {return None;}
            return Some(View {
                id,generation:self.generation.clone(),
                targets:stage.commands.iter().map(|command|(command.device_id.clone(),(command.command,self.written[&command.id]))).collect(),
                deadline:self.deadline,condition:None,managed_away:false,stage:Some(index),
            });
        }
        if self.lights.iter().any(|command| !self.written.contains_key(&command.id))
            || self.targets.keys().any(|device|self.target_ids.get(device).is_none_or(|id|!self.written.contains_key(id))) {
            return None;
        }
        Some(View {
            id, generation: self.generation.clone(),
            targets: self.targets.iter().map(|(device, target)|
                (device.clone(), (*target, self.written[&self.target_ids[device]]))).collect(),
            deadline: self.delivery_deadline.unwrap_or(self.deadline).min(self.deadline),
            condition: self.condition.clone(),
            managed_away: self.away.as_ref().is_some_and(crate::away_auth::Authorization::guarded_operation),
            stage:None,
        })
    }
}
/// Prepared before the atomic variable/queue commit, installed only after it.
pub(crate) struct Completion {
    id: u64,
    gate: Gate,
    phases: BTreeMap<u64, Phase>,
}
#[derive(Default)]
pub(crate) struct Owner {
    phases: BTreeMap<u64, Phase>,
    gates: BTreeMap<u64, Gate>,
    local: BTreeMap<u64,u64>,
    power_leds: std::collections::BTreeSet<DeviceId>,
}
impl Owner {
    /// Retain actual original writes as evidence; do not enqueue them again.
    /// The caller must validate Source/Origin and persist the variable CAS and
    /// LED queue together before installing this prepared gate.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_completion(
        &self,
        config: &Configuration,
        proof: &crate::keypad_completion::FeedbackProof,
        source: Source,
        controller: Uuid,
        originals: Vec<QueuedCommand>,
        leds: &[QueuedCommand],
        manual: &BTreeMap<DeviceId, u64>,
        send_before: u64,
        now: u64,
    ) -> Result<Completion, W> {
        let evidence = proof.evidence();
        let generation = &evidence.generation;
        if proof.decision() != crate::keypad_completion::Decision::Apply
            || !source.valid_for(controller, generation.epochs.configuration)
            || now >= send_before || send_before > proof.send_before()
            || send_before > evidence.deadline || send_before.saturating_sub(now) > LED_DELIVERY_MS
            || originals.len() != evidence.written.len() || originals.is_empty()
            || originals.len() > 512 || leds.len() != evidence.effect.indicators.len()
            || leds.is_empty() || leds.len() > 16
        { return Err(W::InvalidValueInRequest); }
        let mut owned = std::collections::BTreeSet::new();
        for command in &originals {
            if command.epochs != generation.epochs || !owned.insert(command.id)
                || evidence.written.get(&command.id).is_none_or(|at| *at >= command.deadline_ms || *at > now)
            { return Err(W::InvalidValueInRequest); }
        }
        let mut phases = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        let id = leds[0].id;
        let mut previous_gateway = None;
        let mut group = id;
        for (command, expected) in leds.iter().zip(&evidence.effect.indicators) {
            let (device, module) = config.binding(&command.device_id).ok_or(W::ResourceDoesNotExist)?;
            if command.device_id.as_str() != expected.device || command.command != Command::PanelLed(expected.on)
                || !device.enabled || module.kind() != ModuleKind::Keypad
                || module.led_mode() != homesense_core::PanelLedMode::Manual
                || command.epochs != generation.epochs || command.deadline_ms != send_before
                || !source.permits_device(command.device_id.as_str()) || command.id == 0
                || owned.contains(&command.id) || self.phases.contains_key(&command.id)
                || self.gates.contains_key(&command.id) || !seen.insert(command.id)
            { return Err(W::InvalidValueInRequest); }
            if previous_gateway.as_ref() != Some(&device.gateway) {
                group = command.id;
                previous_gateway = Some(device.gateway.clone());
            }
            phases.insert(command.id, Phase { group, gateway: device.gateway.clone(), led: true, gate: Some(id) });
        }
        if proof.lease().observations.keys().any(|device| !source.permits_device(device.as_str())) {
            return Err(W::InsufficientPrivileges);
        }
        let boot = Uuid::parse_str(generation.boot_id()).map_err(|_| W::InvalidValueInRequest)?.as_u128();
        drop(proof.lease().lock(boot, generation.epochs.configuration, generation.epochs.transport, now)
            .map_err(|_| W::ResourceBusy)?);
        Ok(Completion { id, phases, gate: Gate {
            generation: generation.clone(), controller, source: Some(source), away: None,
            lights: originals, leds: leds.iter().map(|c| c.id).collect(), targets: BTreeMap::new(),
            target_ids: BTreeMap::new(), written: evidence.written.clone(),
            guards: vec![ReadCheck { revision: evidence.variable.revision, name: evidence.variable.name.clone(),
                expected_version: evidence.variable.expected_version.checked_add(1).ok_or(W::InvalidValueInRequest)? }],
            manual: proof.lease().observations.keys().map(|device|
                (device.clone(), manual.get(device).copied().unwrap_or(0))).collect(),
            deadline: evidence.deadline, delivery_deadline: Some(send_before),
            ready: Some(BarrierLease { lease: proof.lease().clone(), boot, generation: generation.clone(), deadline: send_before }),
            invalid: false, cancel_reason: homesense_core::queue::CancelReason::RunCancelled,
            stages:vec![],write_window_ms:0,
            condition: Some(Condition { postconditions: std::iter::once(evidence.effect.after.clone())
                    .chain(evidence.effect.when.clone()).collect(), max_age_ms: evidence.effect.max_age_ms,
                after: evidence.writes_by_device.clone(), command_fence: evidence.command_fence, owned }),
        } })
    }
    pub(crate) fn install_completion(&mut self, prepared: Completion) {
        self.phases.extend(prepared.phases);
        self.gates.insert(prepared.id, prepared.gate);
    }
    pub(crate) fn configure(&mut self,document:Option<&homesense_config::rule_document::Document>) {
        self.power_leds.clear();
        let Some(document)=document else{return;};
        for link in document.indicator_links.iter().filter(|link|link.enabled) {
            let power_only=match &link.scene {
                None=>true,
                Some(scene)=>document.scene_switch_groups.iter().find(|g|g.members.iter().any(|m|&m.scene==scene))
                    .filter(|g|g.reconciliation.as_ref().is_some_and(|p|p.enabled))
                    .and_then(|g|g.light_on_patterns(&document.timed_scenes).ok().map(|patterns|(patterns,g.pattern_scope())))
                    .is_some_and(|(patterns,scope)|homesense_core::scene_pattern::power_patterns_disjoint(&patterns,scope)),
            };
            if power_only {self.power_leds.extend(link.leds.iter().cloned());}
        }
    }
    pub(crate) fn stages_active(&self)->bool {
        self.gates.values().any(|gate|!gate.invalid && !gate.stages.is_empty())
    }
    pub(crate) fn write_deadline(&self, id: u64) -> Option<u64> {
        self.local.get(&id).copied().or_else(|| {
            let phase=self.phases.get(&id)?;
            let gate=self.gates.get(&phase.gate?)?;
            if !phase.led {
                gate.stages.iter().find(|stage|stage.group==phase.group)?
                    .opened.map(|at|at.saturating_add(gate.write_window_ms).min(gate.deadline))
            } else {gate.delivery_deadline}
        })
    }
    pub(crate) fn discarded(
        &mut self,
        ids: &[(u64, u64)],
        reason: homesense_core::queue::CancelReason,
    ) {
        for (id, _) in ids {
            if let Some(gate) = self
                .phases
                .get(id)
                .and_then(|p| p.gate)
                .and_then(|id| self.gates.get_mut(&id))
            {
                gate.invalid = true;
                gate.cancel_reason = reason;
            }
        }
    }
    pub(crate) fn invalidated_leds(
        &self,
    ) -> Vec<(
        homesense_core::queue::CancelReason,
        std::collections::BTreeSet<u64>,
    )> {
        use homesense_core::queue::CancelReason;
        [CancelReason::Expired, CancelReason::RunCancelled]
            .into_iter()
            .filter_map(|reason| {
                let ids = self
                    .gates
                    .values()
                    .filter(|g| g.invalid && g.cancel_reason == reason)
                    .flat_map(|g| g.leds.iter().copied().chain(g.stages.iter().flat_map(|stage|stage.commands.iter().map(|command|command.id))))
                    .collect::<std::collections::BTreeSet<_>>();
                (!ids.is_empty()).then_some((reason, ids))
            })
            .collect()
    }
    pub(crate) fn phase(&self, id: u64) -> Option<&Phase> {
        self.phases.get(&id)
    }
    pub(crate) fn control_reply_channels(&self,id:u64,config:&Configuration)->Option<Vec<u8>> {
        if self.local.contains_key(&id) {return None;}
        let phase=self.phases.get(&id)?;
        if phase.led {return None;}
        let gate=self.gates.get(&phase.gate?)?;
        let stage=gate.stages.iter().find(|stage|stage.group==phase.group)?;
        let ports=stage.commands.iter().filter_map(|command|config.binding(&command.device_id)).collect::<Vec<_>>();
        if ports.is_empty() || ports.iter().any(|(_,m)|m.profile()!=homesense_core::ProtocolProfile::R8i8DinPro) {return None;}
        Some(ports.iter().map(|(d,_)|d.channel.get()).collect())
    }
    pub(crate) fn staged_light(&self,id:u64)->bool {
        self.phases.get(&id).is_some_and(|phase|!phase.led && phase.gate.is_some()) && !self.local.contains_key(&id)
    }
    pub(crate) fn feedback_obligation(&self,id:u64)->bool {
        self.gates.values().any(|gate|gate.lights.iter().any(|command|command.id==id))
    }
    pub(crate) fn eligible(&self, id: u64, now: u64) -> bool {
        self.phases.get(&id).is_none_or(|p| {
            p.gate.is_none_or(|id| {
                self.gates.get(&id).is_some_and(|g| {
                    !g.invalid && if p.led {g.ready.as_ref().is_some_and(|lease|lease.valid(now))}
                    else {g.stages.iter().find(|stage|stage.group==p.group).is_some_and(|stage|stage.opened.is_some()&&!stage.confirmed)}
                })
            })
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(
        &mut self,
        config: &Configuration,
        commands: &[QueuedCommand],
        generation: TaskGeneration,
        controller: Uuid,
        source: Option<Source>,
        guards: Vec<ReadCheck>,
        manual: &BTreeMap<DeviceId, u64>,
        now: u64,
        write_window_ms: u64,
    ) {
        let local=source.as_ref().and_then(|source|source.keypad()).and_then(|origin|origin.cue()).map(|cue|cue.0);
        if let Some(command)=commands.iter().find(|command|Some(command.id)==local) {self.local.insert(command.id,command.deadline_ms);}
        let mut start = 0;
        while start < commands.len() {
            if class(config, &commands[start].device_id, commands[start].command).is_none() {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < commands.len()
                && class(config, &commands[end].device_id, commands[end].command).is_some()
            {
                end += 1;
            }
            let id = commands[start].id;
            let lights = commands[start..end]
                .iter()
                .filter(|c| !class(config, &c.device_id, c.command).unwrap().1)
                .cloned()
                .collect::<Vec<_>>();
            let leds = commands[start..end]
                .iter()
                .filter(|c| Some(c.id)!=local && class(config, &c.device_id, c.command).unwrap().1)
                .map(|c| c.id)
                .collect::<Vec<_>>();
            let gated = !lights.is_empty() && !leds.is_empty();
            let sequenced=gated && write_window_ms>0 && sequence_modules(config,lights.iter().map(|c|(&c.device_id,c.command)));
            let first_module=lights.first().and_then(|command|config.binding(&command.device_id)).map(|(_,module)|module.address());
            let mut stages=vec![];
            let mut phase_start = start;
            while phase_start < end {
                let writer_class=|command:&QueuedCommand|class(config,&command.device_id,command.command)
                    .map(|(gateway,led)| {
                        let led=led && Some(command.id)!=local;
                        let module=if sequenced && !led {if Some(command.id)==local {first_module}else{config.binding(&command.device_id).map(|(_,m)|m.address())}}else{None};
                        (gateway,led,module)
                    });
                let (gateway, led, module) = writer_class(&commands[phase_start]).unwrap();
                let mut phase_end = phase_start + 1;
                while phase_end < end
                    && writer_class(&commands[phase_end]) == Some((gateway.clone(), led,module))
                {
                    phase_end += 1;
                }
                for command in &commands[phase_start..phase_end] {
                    self.phases.insert(
                        command.id,
                        Phase {
                            group: commands[phase_start].id,
                            gateway: gateway.clone(),
                            led,
                            gate: (gated && (led || sequenced)).then_some(id),
                        },
                    );
                }
                if sequenced && !led {
                    stages.push(Stage {group:commands[phase_start].id,
                        commands:commands[phase_start..phase_end].iter().filter(|c|Some(c.id)!=local).cloned().collect(),
                        opened:stages.is_empty().then_some(now),confirmed:false});
                }
                phase_start = phase_end;
            }
            if gated {
                let mut targets = lights
                    .iter()
                    .map(|c| (c.device_id.clone(), c.command))
                    .collect::<BTreeMap<_, _>>();
                let redundant = targets
                    .keys()
                    .filter(|device| {
                        config
                            .devices()
                            .get(*device)
                            .and_then(|d| d.supply_relay.as_ref())
                            .is_some_and(|s| targets.get(s) == Some(&Command::Switch(false)))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for device in redundant {
                    targets.remove(&device);
                }
                // A reviewed unique power-pattern LED need not wait for an
                // unrelated brightness ramp. Colliding scene profiles and
                // unreviewed LED mappings keep exact level confirmation.
                let power_only=commands[start..end].iter().filter(|c|leds.contains(&c.id))
                    .all(|c|self.power_leds.contains(&c.device_id));
                if power_only {
                    let replacements=targets.iter().filter_map(|(id,command)| {
                        let Command::SetLevel{level,..}=command else{return None;};
                        let supply=config.devices().get(id)?.supply_relay.as_ref()?;
                        Some((id.clone(),supply.clone(),Command::Switch(level.get()>0)))
                    }).collect::<Vec<_>>();
                    for (id,supply,command) in replacements {
                        // Only an actual supply command in this same phase
                        // can establish the post-write power witness.
                        if targets.contains_key(&supply) {targets.remove(&id);targets.insert(supply,command);}
                    }
                }
                let target_ids = targets
                    .keys()
                    .map(|d| {
                        (
                            d.clone(),
                            lights.iter().rfind(|c| &c.device_id == d).unwrap().id,
                        )
                    })
                    .collect();
                let manual = lights
                    .iter()
                    .map(|c| {
                        (
                            c.device_id.clone(),
                            manual.get(&c.device_id).copied().unwrap_or(0),
                        )
                    })
                    .collect();
                self.gates.insert(
                    id,
                    Gate {
                        generation: generation.clone(),
                        controller,
                        source: source.clone(),
                        away: None,
                        lights,
                        leds,
                        targets,
                        target_ids,
                        written: BTreeMap::new(),
                        guards: guards.clone(),
                        manual,
                        deadline: commands[start..end]
                            .iter()
                            .filter(|c| Some(c.id)!=local && class(config, &c.device_id, c.command).unwrap().1)
                            .map(|c| c.deadline_ms)
                            .min()
                            .unwrap(),
                        ready: None,
                        delivery_deadline: None,
                        invalid: false,
                        cancel_reason: homesense_core::queue::CancelReason::RunCancelled,
                        condition: None,
                        stages,write_window_ms,
                    },
                );
            }
            start = end;
        }
        if source.as_ref().is_some_and(|source|source.keypad().is_some()) {
            for command in commands.iter().filter(|command|matches!(command.command,Command::PanelLed(_)|Command::PanelLedOff)) {
                if self.phases.get(&command.id).is_some_and(|phase|phase.gate.is_none()){self.local.insert(command.id,command.deadline_ms);}
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn away_accepted(
        &mut self,
        config: &Configuration,
        commands: Vec<QueuedCommand>,
        targets: &[crate::away_off::Target],
        reset: Option<QueuedCommand>,
        local_reset: bool,
        generation: TaskGeneration,
        controller: Uuid,
        manual: &BTreeMap<DeviceId, u64>,
    ) {
        let group = commands[0].id;
        let mut current = None;
        let mut phase = group;
        let mut expected = BTreeMap::new();
        let mut target_ids = BTreeMap::new();
        for (command, target) in commands.iter().zip(targets) {
            let module = &target.plan.modules()[target.index];
            let gateway = module.gateway_id().to_owned();
            if current.as_ref() != Some(&gateway) {
                phase = command.id;
                current = Some(gateway.clone());
            }
            self.phases.insert(
                command.id,
                Phase {
                    group: phase,
                    gateway,
                    led: false,
                    gate: None,
                },
            );
            for device in module.devices() {
                expected.insert(
                    device.clone(),
                    match module.module().kind() {
                        ModuleKind::Dimmer => Command::SetLevel {
                            level: homesense_core::Percent::new(0).unwrap(),
                            transition: homesense_core::LevelTransition::Immediate,
                        },
                        _ => Command::Switch(false),
                    },
                );
                target_ids.insert(device.clone(), command.id);
            }
        }
        // Only an unarmed completion owned by this exact logical operation
        // follows its newly committed retry receipts. External/manual changes
        // retire the job before admission; an invalid or already armed gate is
        // never revived and its original total deadline is never extended.
        if targets[0].lease.guarded_operation() {
            for gate in self.gates.values_mut().filter(|gate|
                !gate.invalid && gate.delivery_deadline.is_none()
                && gate.generation == generation && gate.controller == controller
                && gate.away.as_ref().is_some_and(|lease|lease.same_authority(&targets[0].lease)))
            {
                for (device, id) in &target_ids {
                    if gate.targets.contains_key(device) {
                        gate.target_ids.insert(device.clone(),*id);
                        if let Some(version) = gate.manual.get_mut(device) {
                            *version = manual.get(device).copied().unwrap_or(0);
                        }
                    }
                }
                gate.lights.retain(|command|gate.target_ids.values().any(|id|*id==command.id));
                gate.written.retain(|id,_|gate.target_ids.values().any(|current|current==id));
                gate.lights.extend(commands.iter().cloned());
                gate.ready = None;
            }
        }
        if let Some(reset) = reset {
            // A serial operation initially has only one accepted module. Its
            // legacy completion must still prove the entire reviewed scope;
            // future module receipts are filled in as they are committed.
            if targets[0].lease.guarded_operation() && !local_reset {
                for module in targets[0].plan.modules() {
                    for device in module.devices() {
                        expected.insert(device.clone(),match module.module().kind() {
                            ModuleKind::Dimmer=>Command::SetLevel {level:homesense_core::Percent::new(0).unwrap(),transition:homesense_core::LevelTransition::Immediate},
                            _=>Command::Switch(false),
                        });
                    }
                }
            }
            let gateway = config
                .binding(&reset.device_id)
                .expect("validated Away reset binding")
                .0
                .gateway
                .clone();
            if local_reset {
                let group=commands.iter().filter_map(|command|self.phases.get(&command.id)).find(|phase|phase.gateway==gateway).map_or(reset.id,|phase|phase.group);
                self.phases.insert(reset.id,Phase{group,gateway,led:false,gate:None});
                self.local.insert(reset.id,reset.deadline_ms);
                return;
            }
            self.phases.insert(
                reset.id,
                Phase {
                    group: reset.id,
                    gateway,
                    led: true,
                    gate: Some(group),
                },
            );
            let redundant = expected
                .keys()
                .filter(|device| {
                    !targets[0].lease.guarded_operation() && config
                        .devices()
                        .get(*device)
                        .and_then(|d| d.supply_relay.as_ref())
                        .is_some_and(|s| expected.get(s) == Some(&Command::Switch(false)))
                })
                .cloned()
                .collect::<Vec<_>>();
            for device in redundant {
                expected.remove(&device);
                // Retain every module's receipt fence even when a supply-Off
                // observation proves a legacy combined light is dark. Managed
                // Away requires every raw output Off, including its AD8 zero.
            }
            let manual = expected
                .keys()
                .map(|device| (device.clone(), manual.get(device).copied().unwrap_or(0)))
                .collect();
            self.gates.insert(
                group,
                Gate {
                    generation,
                    controller,
                    source: None,
                    away: Some(targets[0].lease.clone()),
                    lights: commands,
                    leds: vec![reset.id],
                    targets: expected,
                    target_ids,
                    written: BTreeMap::new(),
                    guards: vec![],
                    manual,
                    deadline: reset.deadline_ms,
                    delivery_deadline: None,
                    ready: None,
                    invalid: false,
                    cancel_reason: homesense_core::queue::CancelReason::RunCancelled,
                        condition: None,
                        stages:vec![],write_window_ms:0,
                },
            );
        }
    }
    pub(crate) fn authorization(&self, id: u64) -> Option<BarrierLease> {
        let p = self.phases.get(&id)?;
        if !p.led {
            return None;
        }
        self.gates.get(&p.gate?)?.ready.clone()
    }
    pub(crate) fn written(&mut self, id: u64, at: u64) {
        for g in self.gates.values_mut() {
            if g.lights.iter().any(|c| c.id == id) {
                g.written.insert(id, at);
            }
        }
    }
    pub(crate) fn audit(
        &mut self,
        store: &Store,
        generation: Option<&TaskGeneration>,
        now: u64,
        authorized: impl Fn(Uuid) -> bool,
        manual: &BTreeMap<DeviceId, u64>,
    ) -> Result<(), W> {
        for gate in self.gates.values_mut() {
            if gate.invalid {
                continue;
            }
            let stage_expired=gate.stages.iter().find(|stage|!stage.confirmed).is_some_and(|stage|
                stage.opened.is_some_and(|at|now>=at.saturating_add(gate.write_window_ms))
                && stage.commands.iter().any(|command|!gate.written.contains_key(&command.id)));
            let expired = now >= gate.deadline || stage_expired || gate.delivery_deadline.is_some_and(|deadline| now >= deadline);
            if generation != Some(&gate.generation)
                || expired
                || !authorized(gate.controller)
                || gate.source.as_ref().is_some_and(|s| !s.valid() || !s.indication_current())
                || gate.away.as_ref().is_some_and(|a| !a.valid())
                || gate
                    .manual
                    .iter()
                    .any(|(d, v)| manual.get(d).copied().unwrap_or(0) != *v)
            {
                gate.invalid = true;
                if expired { gate.cancel_reason = homesense_core::queue::CancelReason::Expired; }
                continue;
            }
            for guard in &gate.guards {
                if store
                    .logical_variable(guard.revision, &guard.name)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .is_none_or(|v| v.version != guard.expected_version)
                {
                    gate.invalid = true;
                    break;
                }
            }
            if !gate.stages.is_empty() && !store.keypad_completion_dependencies_current(
                gate.generation.boot_id(),gate.generation.epochs.configuration,
                gate.lights.iter().map(|c|c.id).min().unwrap(),
                &gate.lights.iter().map(|c|c.id).collect(),
                &gate.lights.iter().map(|c|c.device_id.clone()).collect(),
            ).map_err(|_|W::ServiceCommunicationFailure)? {gate.invalid=true;}
            if let Some(condition) = &gate.condition
                && !store.keypad_completion_dependencies_current(
                    gate.generation.boot_id(), gate.generation.epochs.configuration,
                    condition.command_fence, &condition.owned,
                    &gate.manual.keys().cloned().collect(),
                ).map_err(|_| W::ServiceCommunicationFailure)?
            {
                gate.invalid = true;
            }
            for command in &gate.lights {
                if !matches!(
                    store
                        .command_status(gate.generation.boot_id(), command.id)
                        .map_err(|_| W::ServiceCommunicationFailure)?,
                    Some(Status::Accepted | Status::Writing | Status::Written | Status::Observed)
                ) {
                    gate.invalid = true;
                    break;
                }
            }
        }
        self.gates.retain(|_, gate| {
            gate.leds.iter().any(|id| {
                matches!(
                    store.command_status(gate.generation.boot_id(), *id),
                    Ok(Some(Status::Accepted | Status::Writing))
                )
            })
        });
        // Keep a bounded mapping only while original tickets remain unsettled.
        self.phases.retain(|id, phase| {
            phase.gate.is_some_and(|id| self.gates.contains_key(&id))
                || matches!(
                    store.command_status(generation.map_or("", TaskGeneration::boot_id), *id),
                    Ok(Some(Status::Accepted | Status::Writing))
                )
        });
        self.local.retain(|id,_|matches!(store.command_status(generation.map_or("",TaskGeneration::boot_id),*id),Ok(Some(Status::Accepted|Status::Writing))));
        Ok(())
    }
    pub(crate) fn views(&self) -> Vec<View> {
        self.gates
            .iter()
            .filter_map(|(id, gate)| gate.view(*id))
            .collect()
    }
    pub(crate) fn apply(&mut self, proof: Proof, observer_boot: u128, now: u64) -> Result<(), W> {
        // A caller cannot alter the predicate, write fence or deadline of a
        // previously fetched view, including using an ordinary empty vector.
        if self.gates.get(&proof.view.id).and_then(|gate| gate.view(proof.view.id)).as_ref() != Some(&proof.view) {
            return Err(W::ResourceBusy);
        }
        let feedback_max_age = proof.view.feedback_max_age_ms();
        let gate = self.gates.get_mut(&proof.view.id).ok_or(W::ResourceBusy)?;
        if gate.invalid || gate.generation != proof.view.generation || now >= gate.deadline
            || gate.delivery_deadline.is_some_and(|deadline| now >= deadline) {
            return Err(W::ResourceBusy);
        }
        let _guard = proof
            .sample
            .lease()
            .lock(
                observer_boot,
                gate.generation.epochs.configuration,
                gate.generation.epochs.transport,
                now,
            )
            .map_err(|_| W::ResourceBusy)?;
        let (matched, valid_until) = match (&gate.condition, &proof.sample) {
            (None, Sample::Targets(sample)) => (
                proof.view.targets.iter().all(|(device, (command, after))| {
                    sample.snapshot().devices.get(device).is_some_and(|s|
                        s.quality == homesense_state::Quality::Confirmed
                        && s.observed_at_ms.is_some_and(|at| at > *after && at <= now && now-at <= feedback_max_age)
                        && s.observed.is_some_and(|o| command.target_matches(o)))
                }),
                proof.view.targets.keys().filter_map(|device|
                    sample.snapshot().devices.get(device).and_then(|state| state.observed_at_ms)
                        .map(|at| at.saturating_add(feedback_max_age))).min().unwrap_or(gate.deadline),
            ),
            (Some(_), Sample::Conditions { matched, valid_until, .. }) => {
                if *matched == Some(false) {
                    gate.invalid = true;
                    gate.ready = None;
                    return Ok(());
                }
                (*matched == Some(true), *valid_until)
            },
            _ => return Err(W::InvalidValueInRequest),
        };
        if let Some(index)=proof.view.stage {
            if matched {
                let stage=gate.stages.get_mut(index).ok_or(W::ResourceBusy)?;
                stage.confirmed=true;
                if let Some(next)=gate.stages.get_mut(index+1) {next.opened=Some(now);}
                log::debug!("batch_module_confirmed group={} stage={} at_ms={} total_deadline_ms={}",proof.view.id,index,now,gate.deadline);
            }
            return Ok(());
        }
        if matched {
            // Arm once, at the first complete physical proof. Refreshing a
            // witness never restarts this delivery budget.
            let delivery_deadline = match gate.delivery_deadline {
                Some(deadline) => deadline,
                None => {
                    let deadline=now.saturating_add(LED_DELIVERY_MS).min(gate.deadline);
                    gate.delivery_deadline=Some(deadline);
                    log::debug!("batch_feedback_ready group={} opened_ms={} total_deadline_ms={} delivery_deadline_ms={}",proof.view.id,now,gate.deadline,deadline);
                    deadline
                }
            };
            gate.ready = Some(BarrierLease {
                lease: proof.sample.lease().clone(),
                boot: observer_boot,
                generation: gate.generation.clone(),
                deadline: valid_until.min(delivery_deadline),
            });
        } else {
            gate.ready = None;
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct BarrierLease {
    lease: crate::feedback::RuleLease,
    boot: u128,
    generation: TaskGeneration,
    deadline: u64,
}
impl BarrierLease {
    fn valid(&self, now: u64) -> bool {
        now < self.deadline
            && self
                .lease
                .lock(
                    self.boot,
                    self.generation.epochs.configuration,
                    self.generation.epochs.transport,
                    now,
                )
                .is_ok()
    }
    pub(crate) fn with_valid<T>(&self, now: u64, operation: impl FnOnce() -> T) -> Option<T> {
        if now >= self.deadline {
            return None;
        }
        let _guard = self
            .lease
            .lock(
                self.boot,
                self.generation.epochs.configuration,
                self.generation.epochs.transport,
                now,
            )
            .ok()?;
        Some(operation())
    }
}
pub(crate) struct Proof {
    view: View,
    sample: Sample,
}
enum Sample {
    Targets(SceneObservationStart),
    Conditions { lease: crate::feedback::RuleLease, matched: Option<bool>, valid_until: u64 },
}
impl Sample {
    fn lease(&self) -> &crate::feedback::RuleLease {
        match self { Self::Targets(sample) => sample.lease(), Self::Conditions { lease, .. } => lease }
    }
}
impl From<SceneObservationStart> for Sample {
    fn from(sample: SceneObservationStart) -> Self { Self::Targets(sample) }
}
impl Proof {
    fn prepare(view: View, config: Option<&Configuration>, feedback: &Feedback, now: u64) -> Result<Self, String> {
        if now >= view.deadline { return Err("LED delivery deadline expired".into()); }
        let sample = if let Some(condition) = &view.condition {
            let config = config.filter(|config| config.revision() == view.generation.epochs.configuration)
                .ok_or("conditional LED configuration missing or changed")?;
            let conditions = condition.postconditions.iter().collect::<Vec<_>>();
            let (lease, values) = crate::component_modes::sample_conditions(
                config, &conditions, feedback, now, condition.max_age_ms)?;
            let current = condition.after.iter().all(|(id, written)|
                lease.observations.get(id).is_none_or(|at| at.is_some_and(|at| at > *written)));
            let matched = if !current || values.iter().any(Option::is_none) { None }
                else { Some(values.iter().all(|value| *value == Some(true))) };
            let valid_until = lease.observations.values().filter_map(|at| *at).min().unwrap_or(now)
                .saturating_add(feedback.effective_max_age_ms(condition.max_age_ms)).min(view.deadline);
            Sample::Conditions { lease, matched, valid_until }
        } else {
            Sample::Targets(feedback.scene_observation_start(now)?)
        };
        Ok(Self { view, sample })
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
        feedback: &Feedback,
    ) -> Result<(), String> {
        tick_round(admission, controller, feedback, &mut self.next).await
    }
}
#[cfg(test)]
pub(crate) async fn tick(
    admission: &Admission,
    controller: Uuid,
    feedback: &Feedback,
) -> Result<(), String> {
    Consumer::default()
        .tick(admission, controller, feedback)
        .await
}
async fn tick_round(
    admission: &Admission,
    controller: Uuid,
    feedback: &Feedback,
    next: &mut usize,
) -> Result<(), String> {
    let generation = match admission.task_generation().await {
        Ok(g) => g,
        Err(W::ResourceBusy | W::ServiceCommunicationFailure) => return Ok(()),
        Err(e) => return Err(format!("batch generation: {e:?}")),
    };
    let views = admission
        .control_batch_views(controller, generation)
        .await
        .map_err(|e| format!("batch views: {e:?}"))?;
    if views.is_empty() {
        return Ok(());
    }
    let config = if views.iter().any(|view| view.condition.is_some()) {
        match admission.automation_bundle(controller, views[0].generation.clone()).await {
            Ok(bundle) => Some(bundle.configuration),
            Err(W::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("conditional LED configuration: {e:?}")),
        }
    } else { None };
    let count = views.len();
    let begin = *next % count;
    *next = (begin + 8.min(count)) % count;
    for view in views.into_iter().cycle().skip(begin).take(8.min(count)) {
        if admission.monotonic_ms() >= view.deadline {
            continue;
        }
        let proof = match Proof::prepare(view, config.as_ref(), feedback, admission.monotonic_ms()) {
            Ok(proof) => proof,
            Err(_) => continue,
        };
        match admission
            .control_batch_proof(controller, proof)
            .await
        {
            Ok(()) | Err(W::ResourceBusy) => {}
            Err(e) => return Err(format!("batch feedback: {e:?}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::{admission::Options, transport_owner::GatewayRead};
    use homesense_core::{ExecutionContext, LevelTransition, Percent, queue::Origin};

    pub(super) async fn fixture() -> (Owner, Feedback, Admission, std::path::PathBuf, u128) {
        let root = std::env::temp_dir().join(format!("batch-budget-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("state.sqlite");
        let bytes = include_bytes!("../../../config/test/native-app-keypad/devices.json");
        Store::open(&path)
            .unwrap()
            .publish(None, 1, bytes, &[])
            .unwrap();
        let config = Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
        let admission = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let generation = admission.task_generation().await.unwrap();
        let boot = Uuid::parse_str(admission.boot_id()).unwrap().as_u128();
        let feedback = Feedback::new(&config, ExecutionContext::Simulation, boot, 1).unwrap();
        let commands = vec![
            QueuedCommand {
                id: 1,
                device_id: DeviceId::new("fixture-light").unwrap(),
                command: Command::SetLevel {
                    level: Percent::new(40).unwrap(),
                    transition: LevelTransition::Immediate,
                },
                origin: Origin::Rule,
                epochs: generation.epochs,
                deadline_ms: 2000,
                run_id: Some(1),
            },
            QueuedCommand {
                id: 2,
                device_id: DeviceId::new("legacy-key-1").unwrap(),
                command: Command::PanelLed(true),
                origin: Origin::Rule,
                epochs: generation.epochs,
                deadline_ms: 15000,
                run_id: Some(1),
            },
        ];
        let mut owner = Owner::default();
        owner.accepted(
            &config,
            &commands,
            generation,
            Uuid::new_v4(),
            None,
            vec![],
            &Default::default(),
            0,2000,
        );
        owner.written(1, 10);
        (owner, feedback, admission, root, boot)
    }
    fn observation_sample(
        feedback: &mut Feedback,
        received: u64,
        now: u64,
        value: u8,
    ) -> SceneObservationStart {
        feedback
            .apply_at(
                &GatewayRead {
                    gateway_id: "bus".into(),
                    transport_epoch: 1,
                    received_at_ms: received,
                    result: Receive::Frames {
                        transport_epoch: 1,
                        frames: vec![hb_protocol::RawFrame {
                            address: 0x15,
                            command: 0xd1,
                            value,
                        }],
                    },
                },
                received,
            )
            .unwrap();
        feedback.scene_observation_start(now).unwrap()
    }
    use hb_transport::Receive;

    #[test]
    fn only_a_contiguous_mixed_lighting_phase_gets_the_feedback_budget() {
        use homesense_hap::requests::Intent;
        let config = Configuration::parse(
            include_bytes!("../../../config/test/native-app-keypad/devices.json"),
            ExecutionContext::Simulation,
        )
        .unwrap();
        let light = || Intent {
            device: DeviceId::new("fixture-light").unwrap(),
            command: Command::SetLevel {
                level: Percent::new(40).unwrap(),
                transition: LevelTransition::Immediate,
            },
        };
        let led = || Intent {
            device: DeviceId::new("legacy-key-1").unwrap(),
            command: Command::PanelLed(true),
        };
        let query = Intent {
            device: DeviceId::new("fixture-light").unwrap(),
            command: Command::Query,
        };
        assert_eq!(
            feedback_roles(&config, &[led(), light()]),
            vec![BudgetRole::FeedbackLed, BudgetRole::Lighting]
        );
        assert_eq!(
            feedback_roles(&config, &[led(), query, light()]),
            vec![BudgetRole::Ordinary; 3]
        );
        assert_eq!(
            feedback_roles(&config, &[led()]),
            vec![BudgetRole::Ordinary]
        );
        assert_eq!(
            feedback_roles(&config, &[light()]),
            vec![BudgetRole::Ordinary]
        );
    }
    #[tokio::test]
    async fn fresh_mismatching_reply_defers_requery_without_authorizing_linked_leds() {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        let config = Configuration::parse(
            include_bytes!("../../../config/test/native-app-keypad/devices.json"),
            ExecutionContext::Simulation,
        ).unwrap();
        let targets = crate::physical_queries::plan(&config, ExecutionContext::Simulation).unwrap();
        let view = owner.views().remove(0);
        assert!(!view.query_requirements(&config, &feedback, &targets, 100).is_empty());
        observation_sample(&mut feedback, 9, 100, 20);
        assert!(!view.query_requirements(&config, &feedback, &targets, 100).is_empty(),
            "a pre-write reply cannot suppress the required confirmation read");
        let sample = observation_sample(&mut feedback, 100, 100, 20); // requested40, received20
        owner.apply(Proof { view, sample: sample.into() }, boot, 100).unwrap();
        assert!(!owner.eligible(2, 100), "a real reply at the wrong level is not target confirmation");
        assert_eq!(owner.write_deadline(2), None);
        let view = owner.views().remove(0);
        assert!(view.query_requirements(&config, &feedback, &targets, 100).is_empty(),
            "fresh mismatching feedback must not trigger the same module query every writer turn");
        assert!(!view.query_requirements(&config, &feedback, &targets, 99).is_empty(),
            "future-dated feedback cannot suppress a read");
        assert!(view.query_requirements(&config, &feedback, &targets, 7_599).is_empty());
        assert!(!view.query_requirements(&config, &feedback, &targets, 7_600).is_empty(),
            "an aging sample still receives a bounded refresh turn inside the original budget");
        assert!(!owner.eligible(2, 7_600));
        let sample = observation_sample(&mut feedback, 7_800, 7_800, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 7_800).unwrap();
        assert!(owner.eligible(2, 7_800));
        assert_eq!(owner.write_deadline(2), Some(9_800));
        assert!(!owner.eligible(2, 9_800));
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn managed_away_known_failure_yields_reads_but_off_proof_still_expires() {
        let (owner, mut feedback, admission, root, _) = fixture().await;
        let config=Configuration::parse(include_bytes!("../../../config/test/native-app-keypad/devices.json"),ExecutionContext::Simulation).unwrap();
        let targets=crate::physical_queries::plan(&config,ExecutionContext::Simulation).unwrap();
        let mut view=owner.views().remove(0);
        observation_sample(&mut feedback,100,100,20); // target is40; actual mismatch remains real
        assert!(view.query_requirements(&config,&feedback,&targets,100).is_empty(),"ordinary mismatching replies also defer repeated reads without granting target proof");
        view.managed_away=true;
        for (command,_) in view.targets.values_mut() {
            *command=Command::SetLevel{level:homesense_core::Percent::new(0).unwrap(),transition:homesense_core::LevelTransition::Immediate};
        }
        assert!(view.query_requirements(&config,&feedback,&targets,100).is_empty(),"fresh failure is for the retry engine, not a hot query loop");
        assert!(view.query_requirements(&config,&feedback,&targets,2600).is_empty(),"a known failure waits for its owner's new attempt instead of consuming read slots");
        assert!(!view.query_requirements(&config,&feedback,&targets,99).is_empty(),"future timestamps cannot suppress a read");
        assert!(view.query_requirements(&config,&feedback,&targets,15000).is_empty(),"the original hard deadline cannot extend");
        observation_sample(&mut feedback,2700,2700,0);
        assert!(view.query_requirements(&config,&feedback,&targets,2700).is_empty());
        assert!(!view.query_requirements(&config,&feedback,&targets,2700+view.feedback_max_age_ms()/2+1).is_empty(),"Off feedback refresh remains bounded by its operation policy");
        admission.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn serialized_led_feedback_retains_early_postwrite_replies_past_five_seconds() {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        let config = Configuration::parse(
            include_bytes!("../../../config/test/native-app-keypad/devices.json"),
            ExecutionContext::Simulation,
        ).unwrap();
        let targets = crate::physical_queries::plan(&config, ExecutionContext::Simulation).unwrap();
        let view = owner.views().remove(0);
        // A different module may complete the serialized confirmation at6s.
        // This output already supplied genuine feedback after its write at10ms.
        let sample = observation_sample(&mut feedback, 100, 6_000, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 6_000).unwrap();
        assert!(owner.eligible(2, 6_000), "an early own-write reply must survive the existing15s phase budget");
        assert_eq!(owner.write_deadline(2), Some(8_000));
        let view = owner.views().remove(0);
        assert!(view.query_requirements(&config, &feedback, &targets, 6_000).is_empty(),
            "do not re-query a confirmed early output while later modules complete");
        let sample = observation_sample(&mut feedback, 6_100, 6_100, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 6_100).unwrap();
        assert_eq!(owner.write_deadline(2), Some(8_000), "new feedback must not renew LED delivery");
        assert!(!owner.eligible(2, 8_000));
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn first_complete_postwrite_proof_arms_delivery_once_and_refresh_cannot_extend_it() {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        for (at, value) in [(10, 40), (2000, 20)] {
            let view = owner.views().remove(0);
            let sample = observation_sample(&mut feedback, at, 2500, value);
            owner.apply(Proof { view, sample: sample.into() }, boot, 2500).unwrap();
            assert!(!owner.eligible(2, 2500));
            assert_eq!(owner.write_deadline(2), None);
        }
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 2500, 2500, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 2500).unwrap();
        assert!(owner.eligible(2, 2500));
        assert_eq!(owner.write_deadline(2), Some(4500));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 4400, 4400, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 4400).unwrap();
        assert_eq!(owner.write_deadline(2), Some(4500));
        assert!(owner.eligible(2, 4499));
        assert!(!owner.eligible(2, 4500));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 4500, 4500, 40);
        assert!(owner.apply(Proof { view, sample: sample.into() }, boot, 4500).is_err());
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn last_moment_proof_is_capped_by_total_deadline_and_expiry_never_becomes_standalone_led()
    {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 14900, 14900, 40);
        owner.apply(Proof { view, sample: sample.into() }, boot, 14900).unwrap();
        assert_eq!(owner.write_deadline(2), Some(15000));
        assert!(owner.eligible(2, 14999));
        assert!(!owner.eligible(2, 15000));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 15000, 15000, 40);
        assert!(owner.apply(Proof { view, sample: sample.into() }, boot, 15000).is_err());
        assert!(!owner.eligible(2, 15001));
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[path = "control_batch_completion_tests.rs"]
mod completion_tests;

#[cfg(test)]
#[path = "control_batch_cue_tests.rs"]
mod cue_tests;
