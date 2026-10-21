//! Actor-owned Away operation receipts. Never opens a connection or remints an
//! event/source capability. Canonical feedback is fenced by Feedback's lease.
use crate::{
    admission::TaskGeneration, away_auth::Authorization, away_off::Target, source_auth::Source,
};
use homesense_core::{DeviceId, away_execution as core};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) const MAX_OPERATIONS: usize = 64;
const RUN_NAMESPACE: u64 = 1 << 60;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Missing {
    pub module_id: String,
    pub channels: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct View {
    pub operation: String,
    pub boot: String,
    pub revision: String,
    pub state: String,
    pub rounds: u8,
    pub max_rounds: u8,
    pub total_outputs: usize,
    pub confirmed_outputs: usize,
    pub missing: Vec<Missing>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub operation: String,
    pub commands: Vec<u64>,
}
/// Both public entry points describe the policy used by the execution owner.
pub(crate) fn retry_metadata() -> serde_json::Value {
    let policy = core::Policy::default();
    serde_json::json!({"max_rounds":policy.max_rounds,"delay_ms":policy.delay_ms,
        "response_ms":policy.response_ms,"total_ms":policy.total_ms})
}
#[derive(Clone, Debug)]
pub(crate) struct ReadNeed {
    pub device: DeviceId,
    pub before_count: u64,
    pub after_ms: u64,
    pub deadline_ms: u64,
}
pub(crate) struct Observation {
    pub proof: crate::feedback::SceneObservationStart,
    pub counts: BTreeMap<DeviceId, u64>,
    pub queries: BTreeMap<DeviceId, core::ReadTurn>,
    pub last_frames: BTreeMap<String, u64>,
}
impl Observation {
    pub fn physical(
        &self,
        ids: impl Iterator<Item = DeviceId>,
    ) -> BTreeMap<DeviceId, core::Sample> {
        ids.filter_map(|id| {
            let state = self.proof.snapshot().devices.get(&id)?;
            let actual = if state.quality != homesense_state::Quality::Confirmed {
                core::Physical::Unknown
            } else {
                match state.observed {
                    Some(homesense_core::Observation::Switch(on)) => {
                        if on {
                            core::Physical::On
                        } else {
                            core::Physical::Off
                        }
                    }
                    Some(homesense_core::Observation::Level(level)) => {
                        if level.get() == 0 {
                            core::Physical::Off
                        } else {
                            core::Physical::On
                        }
                    }
                    _ => core::Physical::Unknown,
                }
            };
            Some((
                id.clone(),
                core::Sample {
                    actual,
                    received_ms: state.observed_at_ms.unwrap_or(0),
                    count: self.counts.get(&id).copied().unwrap_or(0),
                },
            ))
        })
        .collect()
    }
}
pub(crate) struct Job {
    pub serial: u64,
    pub generation: TaskGeneration,
    pub lease: Authorization,
    pub gate: crate::away_auth::OperationGate,
    pub source: Option<Source>,
    pub targets: Vec<Target>,
    pub wires: Vec<homesense_core::gateway_scope::WireScope>,
    pub manual: BTreeMap<DeviceId, u64>,
    pub engine: core::Operation,
    pub receipts: BTreeMap<String, core::Receipt>,
    written: BTreeMap<u64, u64>,
    last: Option<View>,
    last_evidence: core::Evidence,
    pub reads: BTreeSet<String>,
    indications: BTreeSet<u64>,
}
impl Job {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        serial: u64,
        generation: TaskGeneration,
        lease: Authorization,
        source: Option<Source>,
        targets: Vec<Target>,
        wires: Vec<homesense_core::gateway_scope::WireScope>,
        manual: &BTreeMap<DeviceId, u64>,
        now: u64,
    ) -> Result<Self, String> {
        if serial == 0 || serial >= (1 << 55) {
            return Err("Away operation identity exhausted".into());
        }
        let scope = targets
            .iter()
            .map(|target| {
                let m = &target.plan.modules()[target.index];
                core::Module {
                    id: m.module_id().into(),
                    outputs: m.devices().iter().cloned().collect(),
                }
            })
            .collect();
        let engine = core::Operation::new_serialized(scope, Default::default(), now)
            .map_err(|e| format!("Away scope: {e:?}"))?;
        let manual = targets
            .iter()
            .flat_map(|target| target.plan.modules()[target.index].devices())
            .map(|id| (id.clone(), manual.get(id).copied().unwrap_or(0)))
            .collect();
        Ok(Self {
            serial,
            generation,
            lease,
            gate: crate::away_auth::OperationGate::new(),
            source,
            targets,
            wires,
            manual,
            engine,
            receipts: BTreeMap::new(),
            written: BTreeMap::new(),
            last: None,
            last_evidence: Default::default(),
            reads: BTreeSet::new(),
            indications: BTreeSet::new(),
        })
    }
    pub fn retire(&mut self, cause: core::Cancel, now: u64) -> Result<(), String> {
        self.gate.retire();
        self.engine
            .poll(now, core::Owner::Retired(cause), &self.last_evidence)
            .map_err(|e| format!("Away cancellation: {e:?}"))?;
        self.reads.clear();
        self.update_view(&self.last_evidence.clone(), now)
    }
    pub fn id(&self) -> String {
        format!("{}-{}", self.generation.boot_id(), self.serial)
    }
    pub fn run(&self, round: u8) -> u64 {
        RUN_NAMESPACE + (self.serial << 4) + u64::from(round - 1)
    }
    pub fn outputs(&self) -> impl Iterator<Item = DeviceId> + '_ {
        self.targets.iter().flat_map(|target| {
            target.plan.modules()[target.index]
                .devices()
                .iter()
                .cloned()
        })
    }
    pub fn gateways(&self) -> BTreeSet<String> {
        self.targets
            .iter()
            .map(|target| target.plan.modules()[target.index].gateway_id().to_owned())
            .chain(self.wires.iter().map(|wire| wire.gateway().to_owned()))
            .collect()
    }
    pub fn record_written(&mut self, id: u64, at: u64) {
        if self.receipts.values().any(|r| r.command == id) {
            self.written.entry(id).or_insert(at);
        }
    }
    pub fn evidence(
        &self,
        store: &homesense_storage::Store,
        observation: &Observation,
    ) -> Result<core::Evidence, String> {
        let mut commands = BTreeMap::new();
        for r in self.receipts.values() {
            use homesense_storage::journal::Status;
            let state = match store
                .command_status(self.generation.boot_id(), r.command)
                .map_err(|e| e.to_string())?
            {
                Some(Status::Accepted) => Some(core::CommandState::Accepted),
                Some(Status::Writing) => Some(core::CommandState::Writing),
                Some(Status::Written | Status::Observed) => self
                    .written
                    .get(&r.command)
                    .map(|at| core::CommandState::Written { at_ms: *at }),
                Some(Status::Cancelled | Status::Failed | Status::Uncertain) => {
                    Some(core::CommandState::Retired)
                }
                None => None,
            };
            if let Some(state) = state {
                commands.insert(r.command, state);
            }
        }
        let evidence=core::Evidence {
            commands,
            physical: observation.physical(self.outputs()),
            queries: Some(observation.queries.clone()),
        };
        if self.engine.rounds()>1 && log::log_enabled!(log::Level::Debug) {
            let latest=self.receipts.values().map(|r|r.accepted_ms).max();
            for receipt in self.receipts.values().filter(|r|Some(r.accepted_ms)==latest) {
                let ports=receipt.before_counts.iter().map(|(id,before)|
                    (id.as_str(),*before,evidence.physical.get(id))).collect::<Vec<_>>();
                log::debug!("away_retry_evidence operation={} round={} command={} accepted_ms={} send_before_ms={} written={:?} ports={ports:?}",self.id(),self.engine.rounds(),receipt.command,receipt.accepted_ms,receipt.send_before_ms,evidence.commands.get(&receipt.command));
            }
        }
        Ok(evidence)
    }
    pub fn accepted(
        &mut self,
        attempt: &core::Attempt,
        ids: &[u64],
        observation: &Observation,
        manual: &BTreeMap<DeviceId, u64>,
        now: u64,
    ) -> Result<(), String> {
        let targets = self
            .targets
            .iter()
            .filter(|target| {
                attempt
                    .modules
                    .contains(target.plan.modules()[target.index].module_id())
            })
            .collect::<Vec<_>>();
        if ids.len() < targets.len() {
            return Err("Away accepted receipt set incomplete".into());
        }
        let receipts = targets
            .iter()
            .zip(ids)
            .map(|(target, id)| {
                let module = &target.plan.modules()[target.index];
                core::Receipt {
                    module: module.module_id().into(),
                    command: *id,
                    accepted_ms: now,
                    send_before_ms: attempt.send_before_ms,
                    before_counts: module
                        .devices()
                        .iter()
                        .map(|id| (id.clone(), observation.counts.get(id).copied().unwrap_or(0)))
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        self.engine
            .accepted(attempt.proposal, receipts.clone(), now)
            .map_err(|e| format!("Away acceptance evidence: {e:?}"))?;
        for receipt in receipts {
            self.receipts.insert(receipt.module.clone(), receipt);
        }
        self.indications.extend(ids.iter().skip(targets.len()).copied());
        self.manual = self
            .outputs()
            .map(|id| {
                let version = manual.get(&id).copied().unwrap_or(0);
                (id, version)
            })
            .collect();
        Ok(())
    }
    pub fn read_needs(&self) -> Vec<ReadNeed> {
        if self.engine.end().is_some() {
            return vec![];
        }
        self.receipts
            .iter()
            .filter(|(module, _)| self.reads.contains(*module))
            .flat_map(|(_, receipt)| {
                let written = self.written.get(&receipt.command).copied();
                receipt
                    .before_counts
                    .iter()
                    .filter_map(move |(device, before)| {
                        written.map(|after_ms| ReadNeed {
                            device: device.clone(),
                            before_count: *before,
                            after_ms,
                            deadline_ms: after_ms.saturating_add(core::RESPONSE_TIMEOUT_MS).min(self.engine.deadline_ms()),
                        })
                    })
            })
            .collect()
    }
    pub fn view(&self, evidence: &core::Evidence, now: u64) -> Result<View, String> {
        use core::{Cancel, End};
        let (state, reason) = match self.engine.end() {
            Some(End::Confirmed) => ("confirmed", None),
            Some(End::Exhausted) => ("failed", Some("attempts_exhausted")),
            Some(End::Deadline) => ("failed", Some("operation_deadline")),
            Some(End::Cancelled(cause)) => (
                "cancelled",
                Some(match cause {
                    Cancel::AuthorityChanged => "authority_changed",
                    Cancel::ConfigurationChanged => "configuration_changed",
                    Cancel::TransportChanged => "transport_changed",
                    Cancel::ManualOverride => "manual_override",
                    Cancel::Stopped => "stopped",
                    Cancel::AdmissionUnknown => "admission_unknown",
                }),
            ),
            None if self.engine.rounds() == 0 => ("pending", None),
            None if self.engine.rounds() > 1 => ("retrying", None),
            None => ("confirming", None),
        };
        let missing = match self.engine.terminal_missing() {
            Some(missing) => missing.clone(),
            None => self
                .engine
                .missing(evidence, now)
                .map_err(|e| format!("Away missing proof: {e:?}"))?,
        };
        let missing = self
            .targets
            .iter()
            .filter_map(|target| {
                let module = &target.plan.modules()[target.index];
                let ports = missing.get(module.module_id())?;
                Some(Missing {
                    module_id: module.module_id().into(),
                    channels: module
                        .devices()
                        .iter()
                        .enumerate()
                        .filter_map(|(i, id)| ports.contains(id).then_some(i as u8 + 1))
                        .collect(),
                })
            })
            .collect::<Vec<_>>();
        let total_outputs = self.outputs().count();
        Ok(View {
            operation: self.id(),
            boot: self.generation.boot_id().to_owned(),
            revision: self.generation.epochs.configuration.to_string(),
            state: state.into(),
            rounds: self.engine.rounds(),
            max_rounds: 3,
            total_outputs,
            confirmed_outputs: total_outputs
                - missing.iter().map(|m| m.channels.len()).sum::<usize>(),
            missing,
            reason: reason.map(str::to_owned),
        })
    }
    pub fn update_view(&mut self, evidence: &core::Evidence, now: u64) -> Result<(), String> {
        self.last_evidence = evidence.clone();
        if self.engine.end().is_some_and(|end|!matches!(end,core::End::Confirmed)) {
            self.gate.retire();
        }
        let next = self.view(evidence, now)?;
        if self.last.as_ref() != Some(&next) {
            if matches!(next.state.as_str(), "failed" | "cancelled") {
                log::warn!(
                    "away_operation operation={} state={} rounds={} missing={:?} reason={:?}",
                    next.operation,
                    next.state,
                    next.rounds,
                    next.missing,
                    next.reason
                );
            } else {
                log::info!(
                    "away_operation operation={} state={} rounds={} confirmed_outputs={}",
                    next.operation,
                    next.state,
                    next.rounds,
                    next.confirmed_outputs
                );
            }
            self.last = Some(next);
        }
        Ok(())
    }
}
#[derive(Default)]
pub(crate) struct Registry {
    pub jobs: VecDeque<Job>,
    serial: u64,
}
impl Registry {
    pub fn next_serial(&mut self) -> Result<u64, String> {
        while self.jobs.len() >= MAX_OPERATIONS {
            let Some(index) = self.jobs.iter().position(|job| job.engine.end().is_some()) else {
                return Err("Away operation capacity reached".into());
            };
            self.jobs.remove(index);
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or("Away operation identity exhausted")?;
        Ok(self.serial)
    }
    pub fn written(&mut self, id: u64, at: u64) {
        for job in &mut self.jobs {
            job.record_written(id, at);
        }
    }
    pub fn views(&self, revision: u64) -> Vec<View> {
        self.jobs
            .iter()
            .filter(|job| job.generation.epochs.configuration == revision)
            .filter_map(|job| job.last.clone())
            .collect()
    }
    pub fn pending_gateways(
        &self,
        generation: &TaskGeneration,
        now: u64,
        authorize: impl Fn(uuid::Uuid) -> bool,
        valid: impl Fn(&Job) -> bool,
    ) -> BTreeSet<String> {
        self.jobs
            .iter()
            .filter(|job| {
                job.generation == *generation
                    && job.engine.needs_admission(now)
                    && valid(job)
                    && job.lease.valid()
                    && authorize(job.lease.controller())
                    && job.source.as_ref().is_none_or(|source| {
                        source.valid_for(job.lease.controller(), generation.epochs.configuration)
                    })
            })
            .flat_map(|job| job.gateways())
            .collect()
    }
    /// No background read or unrelated LED may steal the passive reply window.
    /// Only the module explicitly requested by the active operation is readable.
    pub fn query_scopes(&self) -> BTreeMap<String, Option<u8>> {
        let mut scopes = BTreeMap::new();
        for job in self.jobs.iter().filter(|job|job.engine.end().is_none()) {
            for gateway in job.gateways() { scopes.insert(gateway,None); }
            for target in &job.targets {
                let module=&target.plan.modules()[target.index];
                if job.reads.contains(module.module_id()) {
                    scopes.insert(module.gateway_id().into(),Some(module.module().address().get()));
                    break;
                }
            }
        }
        scopes
    }
    pub fn read_allowed(&self,gateway:&str,address:Option<u8>)->bool {
        self.query_scopes().get(gateway).is_none_or(|allowed|allowed.is_some() && *allowed==address)
    }
    pub fn allows(&self,gateway:&str,address:u8,command:homesense_core::Command,id:u64)->bool {
        if matches!(command,homesense_core::Command::Motor(homesense_core::MotorMotion::Stop)) {return true;}
        if command==homesense_core::Command::Query {return self.read_allowed(gateway,Some(address));}
        self.jobs.iter().filter(|job|job.engine.end().is_none() && job.gateways().contains(gateway))
            .all(|job|command==homesense_core::Command::ModuleAllOff && job.receipts.values().any(|r|r.command==id)
                || job.source.as_ref().is_some_and(|source|source.keypad().is_some()) && job.indications.contains(&id))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.gate.retire();
    }
}
