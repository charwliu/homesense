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
pub(crate) const FEEDBACK_TOTAL_MS: u64 = 15_000;
const LED_DELIVERY_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetRole {
    Ordinary,
    Lighting,
    FeedbackLed,
}
/// Only a contiguous mixed lighting phase owns a feedback budget.
/// LED-only writes and non-lighting commands retain ordinary admission TTLs.
pub(crate) fn feedback_roles(
    config: &Configuration,
    intents: &[homesense_hap::requests::Intent],
) -> Vec<BudgetRole> {
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
                .any(|i| class(config, &i.device, i.command).unwrap().1)
        {
            for (role, intent) in result[start..end].iter_mut().zip(&intents[start..end]) {
                *role = if class(config, &intent.device, intent.command).unwrap().1 {
                    BudgetRole::FeedbackLed
                } else {
                    BudgetRole::Lighting
                };
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
    delivery_deadline: Option<u64>,
    ready: Option<BarrierLease>,
    invalid: bool,
    cancel_reason: homesense_core::queue::CancelReason,
}
#[derive(Default)]
pub(crate) struct Owner {
    phases: BTreeMap<u64, Phase>,
    gates: BTreeMap<u64, Gate>,
}
impl Owner {
    pub(crate) fn write_deadline(&self, id: u64) -> Option<u64> {
        self.gates
            .get(&self.phases.get(&id)?.gate?)?
            .delivery_deadline
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
                    .flat_map(|g| g.leds.iter().copied())
                    .collect::<std::collections::BTreeSet<_>>();
                (!ids.is_empty()).then_some((reason, ids))
            })
            .collect()
    }
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
                            .filter(|c| class(config, &c.device_id, c.command).unwrap().1)
                            .map(|c| c.deadline_ms)
                            .min()
                            .unwrap(),
                        ready: None,
                        delivery_deadline: None,
                        invalid: false,
                        cancel_reason: homesense_core::queue::CancelReason::RunCancelled,
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
                    delivery_deadline: None,
                    ready: None,
                    invalid: false,
                    cancel_reason: homesense_core::queue::CancelReason::RunCancelled,
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
            let expired = now >= gate.deadline || gate.delivery_deadline.is_some_and(|deadline| now >= deadline);
            if generation != Some(&gate.generation)
                || expired
                || !authorized(gate.controller)
                || gate.source.as_ref().is_some_and(|s| !s.valid())
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
                    deadline: g.delivery_deadline.unwrap_or(g.deadline).min(g.deadline),
                })
            })
            .collect()
    }
    pub(crate) fn apply(&mut self, proof: Proof, observer_boot: u128, now: u64) -> Result<(), W> {
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
                    .min(delivery_deadline),
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::{admission::Options, transport_owner::GatewayRead};
    use homesense_core::{ExecutionContext, LevelTransition, Percent, queue::Origin};

    async fn fixture() -> (Owner, Feedback, Admission, std::path::PathBuf, u128) {
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
    async fn first_complete_postwrite_proof_arms_delivery_once_and_refresh_cannot_extend_it() {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        for (at, value) in [(10, 40), (2000, 20)] {
            let view = owner.views().remove(0);
            let sample = observation_sample(&mut feedback, at, 2500, value);
            owner.apply(Proof { view, sample }, boot, 2500).unwrap();
            assert!(!owner.eligible(2, 2500));
            assert_eq!(owner.write_deadline(2), None);
        }
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 2500, 2500, 40);
        owner.apply(Proof { view, sample }, boot, 2500).unwrap();
        assert!(owner.eligible(2, 2500));
        assert_eq!(owner.write_deadline(2), Some(4500));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 4400, 4400, 40);
        owner.apply(Proof { view, sample }, boot, 4400).unwrap();
        assert_eq!(owner.write_deadline(2), Some(4500));
        assert!(owner.eligible(2, 4499));
        assert!(!owner.eligible(2, 4500));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 4500, 4500, 40);
        assert!(owner.apply(Proof { view, sample }, boot, 4500).is_err());
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn last_moment_proof_is_capped_by_total_deadline_and_expiry_never_becomes_standalone_led()
    {
        let (mut owner, mut feedback, admission, root, boot) = fixture().await;
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 14900, 14900, 40);
        owner.apply(Proof { view, sample }, boot, 14900).unwrap();
        assert_eq!(owner.write_deadline(2), Some(15000));
        assert!(owner.eligible(2, 14999));
        assert!(!owner.eligible(2, 15000));
        let view = owner.views().remove(0);
        let sample = observation_sample(&mut feedback, 15000, 15000, 40);
        assert!(owner.apply(Proof { view, sample }, boot, 15000).is_err());
        assert!(!owner.eligible(2, 15001));
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
