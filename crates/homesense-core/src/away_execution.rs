//! Bounded Away operation state, separate from per-attempt command leases.
//!
//! This model performs no I/O and grants no authority. The execution owner must
//! validate the original source/configuration/wires, durably accept each new
//! absolute module-Off attempt, and supply canonical physical feedback. A plan
//! returned here is neither a reusable command ticket nor permission to retry a
//! Writing command. There is deliberately no restart/replay constructor.
use crate::DeviceId;
use std::collections::{BTreeMap, BTreeSet};

pub const RESPONSE_TIMEOUT_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub max_rounds: u8,
    pub delay_ms: u64,
    pub send_ms: u64,
    /// Reply timeout measured from the actual completed module write. This is
    /// independent of the age allowed for an already received observation.
    pub response_ms: u64,
    pub feedback_ms: u64,
    pub total_ms: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_rounds: 3,
            delay_ms: 200,
            send_ms: 2000,
            response_ms: RESPONSE_TIMEOUT_MS,
            feedback_ms: 5000,
            total_ms: 5_000,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub id: String,
    /// Exact eight physical outputs, not logical combined-light projections.
    pub outputs: BTreeSet<DeviceId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub proposal: u64,
    pub round: u8,
    pub proposed_ms: u64,
    pub modules: BTreeSet<String>,
    pub send_before_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub module: String,
    pub command: u64,
    pub accepted_ms: u64,
    pub send_before_ms: u64,
    pub before_counts: BTreeMap<DeviceId, u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandState {
    Accepted,
    Writing,
    /// Full physical write; retain this timestamp if journal status advances.
    Written {
        at_ms: u64,
    },
    /// The owner has definitively retired this attempt's ticket. For uncertain
    /// I/O the owner must first recover/revalidate before allowing a new attempt.
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Physical {
    Unknown,
    On,
    Off,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub actual: Physical,
    pub received_ms: u64,
    pub count: u64,
}
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub commands: BTreeMap<u64, CommandState>,
    pub physical: BTreeMap<DeviceId, Sample>,
    /// A queued query is not a device timeout. When a serialized query owner
    /// exists, an unanswered output gets its own bounded read turn before a
    /// retry. This never counts as physical confirmation.
    pub queries: Option<BTreeMap<DeviceId, ReadTurn>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadTurn {
    pub written_ms: u64,
    pub finished_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cancel {
    AuthorityChanged,
    ConfigurationChanged,
    TransportChanged,
    ManualOverride,
    Stopped,
    AdmissionUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Current,
    /// Existing receive/recovery work has not finished. This is not a new round.
    Waiting,
    Retired(Cancel),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Confirmed,
    Exhausted,
    Deadline,
    Cancelled(Cancel),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Wait,
    Submit(Attempt),
    /// Read-only confirmation can continue while a round is settling. Reads
    /// still use the existing bus owner, never one query per scene/client.
    Read(BTreeSet<String>),
    Finished(End),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidScope,
    InvalidPolicy,
    ClockRegression,
    InvalidReceipt,
    InvalidEvidence,
    CounterExhausted,
}
#[derive(Clone, Debug)]
enum Phase {
    Ready,
    Proposed(Attempt),
    Settling,
    Delay { until: u64 },
    Confirming,
    /// No more writes. Refresh previously confirmed modules before freezing
    /// the failure scope, without extending the original operation deadline.
    Exhausting,
    End(End),
}
#[derive(Clone, Debug)]
struct Tracked {
    outputs: BTreeSet<DeviceId>,
    receipt: Option<Receipt>,
    verified: bool,
}
/// One original operation. Retries cannot add modules or change any output.
#[derive(Clone, Debug)]
pub struct Operation {
    policy: Policy,
    modules: BTreeMap<String, Tracked>,
    phase: Phase,
    deadline: u64,
    last_ms: u64,
    rounds: u8,
    proposals: u64,
    used_commands: BTreeSet<u64>,
    terminal_missing: Option<BTreeMap<String, BTreeSet<DeviceId>>>,
}
impl Operation {
    pub fn new(modules: Vec<Module>, policy: Policy, now: u64) -> Result<Self, Error> {
        if modules.is_empty() || modules.len() > 128 {
            return Err(Error::InvalidScope);
        }
        if policy.max_rounds == 0
            || policy.max_rounds > 16
            || policy.delay_ms == 0
            || policy.send_ms == 0
            || policy.response_ms == 0
            || policy.feedback_ms == 0
            || policy.total_ms == 0
            || policy.total_ms > 300_000
            || policy.send_ms >= policy.total_ms
        {
            return Err(Error::InvalidPolicy);
        }
        let deadline = now
            .checked_add(policy.total_ms)
            .ok_or(Error::InvalidPolicy)?;
        let mut tracked = BTreeMap::new();
        let mut all = BTreeSet::new();
        for module in modules {
            if DeviceId::new(&module.id).is_err()
                || module.outputs.len() != 8
                || module.outputs.iter().any(|id| !all.insert(id.clone()))
                || tracked
                    .insert(
                        module.id,
                        Tracked {
                            outputs: module.outputs,
                            receipt: None,
                            verified: false,
                        },
                    )
                    .is_some()
            {
                return Err(Error::InvalidScope);
            }
        }
        Ok(Self {
            policy,
            modules: tracked,
            phase: Phase::Ready,
            deadline,
            last_ms: now,
            rounds: 0,
            proposals: 0,
            used_commands: BTreeSet::new(),
            terminal_missing: None,
        })
    }
    fn clock(&mut self, now: u64) -> Result<(), Error> {
        if now < self.last_ms {
            return Err(Error::ClockRegression);
        }
        self.last_ms = now;
        Ok(())
    }
    pub fn rounds(&self) -> u8 {
        self.rounds
    }
    pub fn deadline_ms(&self) -> u64 {
        self.deadline
    }
    pub fn end(&self) -> Option<&End> {
        match &self.phase {
            Phase::End(end) => Some(end),
            _ => None,
        }
    }
    /// Frozen unresolved scope at an exhausted/deadline result. Later feedback
    /// may change live state but must not rewrite a historical failure report.
    pub fn terminal_missing(&self) -> Option<&BTreeMap<String, BTreeSet<DeviceId>>> {
        self.terminal_missing.as_ref()
    }
    fn finish_failure(
        &mut self,
        end: End,
        evidence: &Evidence,
        now: u64,
    ) -> Result<Decision, Error> {
        self.terminal_missing = Some(self.missing(evidence, now)?);
        self.phase = Phase::End(end.clone());
        Ok(Decision::Finished(end))
    }
    fn exhaust_or_refresh(
        &mut self,
        evidence: &Evidence,
        now: u64,
        fresh: &BTreeSet<String>,
    ) -> Result<Decision, Error> {
        let mut ageing=BTreeSet::new();
        for (id,module) in &self.modules {
            if fresh.contains(id) {continue;}
            // A partially responding module may never have reached verified.
            // Its known-Off ports still need a fresh read before the missing
            // scope can distinguish one silent channel from all eight.
            let partial=if let Some(receipt)=&module.receipt
                && let Some(written)=Self::written(receipt,evidence,now)? {
                module.outputs.iter().any(|output|evidence.physical.get(output).is_some_and(|sample|
                    sample.actual==Physical::Off && sample.count>receipt.before_counts[output]
                    && sample.received_ms>=written && sample.received_ms<=now
                    && now-sample.received_ms>=self.policy.feedback_ms))
            }else{false};
            if module.verified || partial {ageing.insert(id.clone());}
        }
        if ageing.is_empty() {return self.finish_failure(End::Exhausted,evidence,now);}
        self.phase=Phase::Exhausting;
        Ok(Decision::Read(ageing))
    }
    /// Fence new background reads only when an attempt is ready to enter the
    /// command queue. Confirmation reads must remain possible while settling.
    pub fn needs_admission(&self, now: u64) -> bool {
        now < self.deadline
            && match self.phase {
                Phase::Ready | Phase::Proposed(_) => true,
                Phase::Delay { until } => now >= until,
                _ => false,
            }
    }
    /// A definite no-acceptance Busy may return a proposal to Ready. Ambiguous
    /// admission must cancel instead; an old delayed acceptance is rejected by
    /// the next proposal's distinct identity.
    pub fn defer(&mut self, proposal: u64, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        if !matches!(&self.phase, Phase::Proposed(p) if p.proposal == proposal) {
            return Err(Error::InvalidReceipt);
        }
        self.phase = Phase::Ready;
        Ok(())
    }
    /// Install exact durable receipts once. Never install an earlier attempt's
    /// command ID, a subset, an extra module, or a renewed old deadline.
    pub fn accepted(
        &mut self,
        proposal: u64,
        receipts: Vec<Receipt>,
        now: u64,
    ) -> Result<(), Error> {
        self.clock(now)?;
        let Phase::Proposed(attempt) = &self.phase else {
            return Err(Error::InvalidReceipt);
        };
        if attempt.proposal != proposal
            || now >= attempt.send_before_ms
            || now >= self.deadline
            || receipts.len() != attempt.modules.len()
        {
            return Err(Error::InvalidReceipt);
        }
        let mut seen_modules = BTreeSet::new();
        let mut seen_commands = BTreeSet::new();
        for receipt in &receipts {
            let Some(module) = self.modules.get(&receipt.module) else {
                return Err(Error::InvalidReceipt);
            };
            if !attempt.modules.contains(&receipt.module)
                || !seen_modules.insert(receipt.module.clone())
                || receipt.command == 0
                || self.used_commands.contains(&receipt.command)
                || !seen_commands.insert(receipt.command)
                || receipt.accepted_ms > now
                || receipt.accepted_ms < attempt.proposed_ms
                || receipt.send_before_ms != attempt.send_before_ms
                || receipt.before_counts.keys().ne(module.outputs.iter())
            {
                return Err(Error::InvalidReceipt);
            }
        }
        for receipt in receipts {
            self.used_commands.insert(receipt.command);
            let module = self
                .modules
                .get_mut(&receipt.module)
                .expect("validated original module");
            module.receipt = Some(receipt);
        }
        self.rounds = attempt.round;
        self.phase = Phase::Settling;
        Ok(())
    }
    fn written(receipt: &Receipt, evidence: &Evidence, now: u64) -> Result<Option<u64>, Error> {
        match evidence.commands.get(&receipt.command) {
            Some(CommandState::Written { at_ms }) => {
                if *at_ms < receipt.accepted_ms || *at_ms >= receipt.send_before_ms || *at_ms > now
                {
                    return Err(Error::InvalidEvidence);
                }
                Ok(Some(*at_ms))
            }
            _ => Ok(None),
        }
    }
    fn fresh_off(
        module: &Tracked,
        evidence: &Evidence,
        now: u64,
        max_age: u64,
    ) -> Result<bool, Error> {
        let Some(receipt) = &module.receipt else {
            return Ok(false);
        };
        let Some(written) = Self::written(receipt, evidence, now)? else {
            return Ok(false);
        };
        Ok(module.outputs.iter().all(|output| {
            evidence.physical.get(output).is_some_and(|sample| {
                sample.actual == Physical::Off
                    && sample.count > receipt.before_counts[output]
                    && sample.received_ms >= written
                    && sample.received_ms <= now
                    && now - sample.received_ms < max_age
            })
        }))
    }
    /// Exact unresolved physical channels, for scoped terminal reporting.
    pub fn missing(
        &self,
        evidence: &Evidence,
        now: u64,
    ) -> Result<BTreeMap<String, BTreeSet<DeviceId>>, Error> {
        let mut missing = BTreeMap::new();
        for (id, module) in &self.modules {
            let written = module
                .receipt
                .as_ref()
                .map(|receipt| Self::written(receipt, evidence, now))
                .transpose()?
                .flatten();
            let outputs = module
                .outputs
                .iter()
                .filter(|output| {
                    let Some(written) = written else {
                        return true;
                    };
                    let receipt = module.receipt.as_ref().expect("write has receipt");
                    !evidence.physical.get(*output).is_some_and(|sample| {
                        sample.actual == Physical::Off
                            && sample.count > receipt.before_counts[*output]
                            && sample.received_ms >= written
                            && sample.received_ms <= now
                            && now - sample.received_ms < self.policy.feedback_ms
                    })
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            if !outputs.is_empty() {
                missing.insert(id.clone(), outputs);
            }
        }
        Ok(missing)
    }
    pub fn poll(&mut self, now: u64, owner: Owner, evidence: &Evidence) -> Result<Decision, Error> {
        self.clock(now)?;
        if let Phase::End(end) = &self.phase {
            return Ok(Decision::Finished(end.clone()));
        }
        if let Owner::Retired(reason) = owner {
            self.phase = Phase::End(End::Cancelled(reason));
            return Ok(Decision::Finished(End::Cancelled(reason)));
        }
        if now >= self.deadline {
            return self.finish_failure(End::Deadline, evidence, now);
        }
        if matches!(self.phase, Phase::Proposed(_)) {
            return Ok(Decision::Wait);
        }
        let mut fresh = BTreeSet::new();
        for (id, module) in &mut self.modules {
            if Self::fresh_off(module, evidence, now, self.policy.feedback_ms)? {
                module.verified = true;
                fresh.insert(id.clone());
            } else if let Some(receipt) = &module.receipt {
                // A newer actual On invalidates a previous successful module;
                // ageing an unchanged Off only requests another observation.
                if let Some(written) = Self::written(receipt, evidence, now)?
                    && module.outputs.iter().any(|output| {
                        evidence.physical.get(output).is_some_and(|sample| {
                            sample.actual == Physical::On
                                && sample.count > receipt.before_counts[output]
                                && sample.received_ms >= written
                                && sample.received_ms <= now
                        })
                    })
                {
                    module.verified = false;
                }
            }
        }
        if fresh.len() == self.modules.len() {
            if owner != Owner::Current {
                return Ok(Decision::Wait);
            }
            self.terminal_missing = Some(BTreeMap::new());
            self.phase = Phase::End(End::Confirmed);
            return Ok(Decision::Finished(End::Confirmed));
        }
        match self.phase {
            Phase::Proposed(_) => return Ok(Decision::Wait),
            Phase::Settling => {
                // Absence of a reply is not proof that an accepted/Writing
                // ticket has retired. Never overlap it with another attempt.
                if self
                    .modules
                    .values()
                    .filter_map(|module| module.receipt.as_ref())
                    .any(|receipt| {
                        !matches!(
                            evidence.commands.get(&receipt.command),
                            Some(CommandState::Written { .. } | CommandState::Retired)
                        )
                    })
                {
                    return Ok(Decision::Wait);
                }
                let queries_pending = if let Some(queries) = &evidence.queries {
                    self.modules.values().filter(|module| !module.verified).any(|module| {
                        let Some(receipt) = &module.receipt else { return false; };
                        let Some(CommandState::Written { at_ms }) = evidence.commands.get(&receipt.command) else { return false; };
                        let replied = |output:&DeviceId| evidence.physical.get(output).is_some_and(|sample|
                                sample.count > receipt.before_counts[output]
                                    && sample.received_ms >= *at_ms && sample.received_ms <= now);
                        let queried = |output:&DeviceId| queries.get(output).is_some_and(|turn|
                                turn.written_ms >= *at_ms && turn.finished_ms >= turn.written_ms
                                    && turn.finished_ms <= now);
                        // One genuinely failed output query is already enough
                        // to retry this atomic module-Off. The other seven
                        // unsent reads remain unsent and cannot confirm Off.
                        let failed=module.outputs.iter().any(|output|
                            (!replied(output) && queried(output))
                                || (replied(output) && evidence.physical[output].actual==Physical::On));
                        !failed && module.outputs.iter().any(|output|!replied(output) && !queried(output))
                    })
                } else { false };
                let waiting_for_reply = self.modules.values().filter(|module| !module.verified)
                    .filter_map(|module| module.receipt.as_ref())
                    .any(|receipt| matches!(evidence.commands.get(&receipt.command),
                        Some(CommandState::Written { at_ms })
                        if now.saturating_sub(*at_ms) < self.policy.response_ms));
                // A completed bounded read, including timeout, has already
                // answered whether this output replied. Do not stack another
                // unsolicited-response wait on top of that same read turn.
                if (waiting_for_reply && evidence.queries.is_none()) || queries_pending {
                    return Ok(Decision::Read(
                        self.modules
                            .keys()
                            .filter(|id| !fresh.contains(*id))
                            .cloned()
                            .collect(),
                    ));
                }
                let needed = self.modules.values().any(|module| !module.verified);
                if needed && self.rounds >= self.policy.max_rounds {
                    return self.exhaust_or_refresh(evidence,now,&fresh);
                }
                if needed {
                    let until = now
                        .checked_add(self.policy.delay_ms)
                        .ok_or(Error::InvalidPolicy)?
                        .min(self.deadline);
                    self.phase = Phase::Delay { until };
                    return Ok(Decision::Wait);
                }
                self.phase = Phase::Confirming;
                return Ok(Decision::Read(
                    self.modules
                        .keys()
                        .filter(|id| !fresh.contains(*id))
                        .cloned()
                        .collect(),
                ));
            }
            Phase::Confirming => {
                if self.modules.values().any(|module| !module.verified) {
                    if self.rounds >= self.policy.max_rounds {
                        return self.exhaust_or_refresh(evidence,now,&fresh);
                    }
                    self.phase = Phase::Delay {
                        until: now
                            .checked_add(self.policy.delay_ms)
                            .ok_or(Error::InvalidPolicy)?
                            .min(self.deadline),
                    };
                    return Ok(Decision::Wait);
                }
                return Ok(Decision::Read(
                    self.modules
                        .keys()
                        .filter(|id| !fresh.contains(*id))
                        .cloned()
                        .collect(),
                ));
            }
            Phase::Exhausting => return self.exhaust_or_refresh(evidence,now,&fresh),
            Phase::Delay { until } if now < until => return Ok(Decision::Wait),
            Phase::End(_) => unreachable!(),
            _ => {}
        }
        let modules = self
            .modules
            .iter()
            .filter(|(_, module)| !module.verified)
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        if modules.is_empty() {
            self.phase = Phase::Confirming;
            return Ok(Decision::Read(
                self.modules
                    .keys()
                    .filter(|id| !fresh.contains(*id))
                    .cloned()
                    .collect(),
            ));
        }
        if owner != Owner::Current {
            return Ok(Decision::Wait);
        }
        if self.rounds >= self.policy.max_rounds {
            return self.exhaust_or_refresh(evidence,now,&fresh);
        }
        self.proposals = self
            .proposals
            .checked_add(1)
            .ok_or(Error::CounterExhausted)?;
        let attempt = Attempt {
            proposal: self.proposals,
            round: self.rounds + 1,
            proposed_ms: now,
            modules,
            send_before_ms: now
                .checked_add(self.policy.send_ms)
                .ok_or(Error::InvalidPolicy)?
                .min(self.deadline),
        };
        self.phase = Phase::Proposed(attempt.clone());
        Ok(Decision::Submit(attempt))
    }
}
