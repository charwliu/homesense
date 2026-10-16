//! Boot-scoped physical writer phases. A phase is one already evaluated admission,
//! never inferred from two absent run IDs. LEDs wait for genuine post-write raw
//! feedback; this owner does not evaluate scenes, change variables, or query I/O.
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
/// Stable partition within one already evaluated due phase. Motor/STOP,
/// parameters and queries remain causal barriers; gateway writes stay separate. Sources
/// are permuted with their commands before durable command IDs are allocated.
pub(crate) fn partition(
    config: &Configuration,
    intents: &mut [homesense_hap::requests::Intent],
    sources: &mut Option<Vec<String>>,
) -> Vec<usize> {
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
            class(config, &intents[*i].device, intents[*i].command)
                .unwrap()
                .1
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
#[derive(Clone)]
pub(crate) struct View {
    id: u64,
    generation: TaskGeneration,
    targets: BTreeMap<DeviceId, (Command, u64)>,
    deadline: u64,
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
    ready: Option<BarrierLease>,
    invalid: bool,
}
#[derive(Default)]
pub(crate) struct Owner {
    phases: BTreeMap<u64, Phase>,
    gates: BTreeMap<u64, Gate>,
}
impl Owner {
    pub(crate) fn phase(&self, id: u64) -> Option<&Phase> {
        self.phases.get(&id)
    }
    pub(crate) fn eligible(&self, id: u64, now: u64) -> bool {
        self.phases.get(&id).is_none_or(|p| {
            p.gate.is_none_or(|id| {
                self.gates.get(&id).is_some_and(|g| {
                    g.ready.as_ref().is_some_and(|lease| lease.valid(now)) && !g.invalid
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
    ) {
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
                .filter(|c| class(config, &c.device_id, c.command).unwrap().1)
                .map(|c| c.id)
                .collect::<Vec<_>>();
            let gated = !lights.is_empty() && !leds.is_empty();
            let mut phase_start = start;
            while phase_start < end {
                let (gateway, led) = class(
                    config,
                    &commands[phase_start].device_id,
                    commands[phase_start].command,
                )
                .unwrap();
                let mut phase_end = phase_start + 1;
                while phase_end < end
                    && class(
                        config,
                        &commands[phase_end].device_id,
                        commands[phase_end].command,
                    ) == Some((gateway.clone(), led))
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
                            gate: (gated && led).then_some(id),
                        },
                    );
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
                            .map(|c| c.deadline_ms)
                            .min()
                            .unwrap(),
                        ready: None,
                        invalid: false,
                    },
                );
            }
            start = end;
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn away_accepted(
        &mut self,
        config: &Configuration,
        commands: Vec<QueuedCommand>,
        targets: &[crate::away_off::Target],
        reset: Option<QueuedCommand>,
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
        if let Some(reset) = reset {
            let gateway = config
                .binding(&reset.device_id)
                .expect("validated Away reset binding")
                .0
                .gateway
                .clone();
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
                    config
                        .devices()
                        .get(*device)
                        .and_then(|d| d.supply_relay.as_ref())
                        .is_some_and(|s| expected.get(s) == Some(&Command::Switch(false)))
                })
                .cloned()
                .collect::<Vec<_>>();
            for device in redundant {
                expected.remove(&device);
                target_ids.remove(&device);
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
                    ready: None,
                    invalid: false,
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
            if generation != Some(&gate.generation)
                || now >= gate.deadline
                || !authorized(gate.controller)
                || gate.source.as_ref().is_some_and(|s| !s.valid())
                || gate.away.as_ref().is_some_and(|a| !a.valid())
                || gate
                    .manual
                    .iter()
                    .any(|(d, v)| manual.get(d).copied().unwrap_or(0) != *v)
            {
                gate.invalid = true;
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
        Ok(())
    }
    pub(crate) fn views(&self) -> Vec<View> {
        self.gates
            .iter()
            .filter_map(|(id, g)| {
                if g.invalid || g.lights.iter().any(|c| !g.written.contains_key(&c.id)) {
                    return None;
                }
                let targets = g
                    .targets
                    .iter()
                    .map(|(device, target)| {
                        let at = g.written[&g.target_ids[device]];
                        (device.clone(), (*target, at))
                    })
                    .collect();
                Some(View {
                    id: *id,
                    generation: g.generation.clone(),
                    targets,
                    deadline: g.deadline,
                })
            })
            .collect()
    }
    pub(crate) fn apply(&mut self, proof: Proof, observer_boot: u128, now: u64) -> Result<(), W> {
        let gate = self.gates.get_mut(&proof.view.id).ok_or(W::ResourceBusy)?;
        if gate.invalid || gate.generation != proof.view.generation || now >= gate.deadline {
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
        if proof.view.targets.iter().all(|(device, (command, after))| {
            proof
                .sample
                .snapshot()
                .devices
                .get(device)
                .is_some_and(|s| {
                    s.quality == homesense_state::Quality::Confirmed
                        && s.observed_at_ms
                            .is_some_and(|at| at > *after && at <= now && now - at <= 5000)
                        && s.observed.is_some_and(|o| command.target_matches(o))
                })
        }) {
            gate.ready = Some(BarrierLease {
                lease: proof.sample.lease().clone(),
                boot: observer_boot,
                generation: gate.generation.clone(),
                deadline: proof
                    .view
                    .targets
                    .keys()
                    .filter_map(|device| {
                        proof
                            .sample
                            .snapshot()
                            .devices
                            .get(device)
                            .and_then(|state| state.observed_at_ms)
                            .map(|at| at.saturating_add(5000))
                    })
                    .min()
                    .unwrap_or(gate.deadline)
                    .min(gate.deadline),
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
    sample: SceneObservationStart,
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
    let count = views.len();
    let begin = *next % count;
    *next = (begin + 8.min(count)) % count;
    for view in views.into_iter().cycle().skip(begin).take(8.min(count)) {
        if admission.monotonic_ms() >= view.deadline {
            continue;
        }
        let sample = feedback.scene_observation_start(admission.monotonic_ms())?;
        match admission
            .control_batch_proof(controller, Proof { view, sample })
            .await
        {
            Ok(()) | Err(W::ResourceBusy) => {}
            Err(e) => return Err(format!("batch feedback: {e:?}")),
        }
    }
    Ok(())
}
