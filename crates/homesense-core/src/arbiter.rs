//! Single-owner queue and configuration-publication barrier.
//! The caller owns immutable device mappings, authentication, encoding and the
//! durable journal. No method here performs I/O or proves database durability.
use crate::{
    admission::{Access, Rejection, RuntimePolicy},
    queue::{CancelReason, Cancellation, CommandQueue, QueueError, QueuedCommand},
};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbiterError {
    Admission(Rejection),
    Queue(QueueError),
    Busy,
    InvalidState,
    CounterExhausted,
    Expired,
    DrainFailed,
    NotSynchronized,
    RevisionMismatch,
    BatchTooLarge,
}
impl std::fmt::Display for ArbiterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ArbiterError {}

/// Queue validation and persistence failures are distinct; neither publishes the
/// staged queue. A persistence error may still require database recovery.
#[derive(Debug)]
pub enum BatchCommitError<E> {
    Admission(ArbiterError),
    Persistence(E),
}

#[derive(Debug, Default)]
pub struct Dispatch {
    pub command: Option<QueuedCommand>,
    pub cancelled: Vec<Cancellation>,
    pub rejected: Vec<(QueuedCommand, Rejection)>,
}
struct Publication {
    deadline: u64,
    stops: VecDeque<QueuedCommand>,
    failed: bool,
    committed: bool,
    transaction_started: bool,
}
/// Completion of a durable revision transaction, supplied by the storage owner.
#[derive(Debug, Clone, Copy)]
pub enum CommitResult {
    Committed(u64),
    RolledBack,
    Unknown,
}

pub struct Arbiter {
    policy: RuntimePolicy,
    queue: CommandQueue,
    writing: Vec<QueuedCommand>,
    publication: Option<Publication>,
}
impl Arbiter {
    pub fn new(
        policy: RuntimePolicy,
        normal_capacity: usize,
        stop_capacity: usize,
    ) -> Result<Self, ArbiterError> {
        if policy.publishing {
            return Err(ArbiterError::InvalidState);
        }
        Ok(Self {
            policy,
            queue: CommandQueue::new(normal_capacity, stop_capacity)
                .map_err(ArbiterError::Queue)?,
            writing: Vec::new(),
            publication: None,
        })
    }
    pub fn policy(&self) -> RuntimePolicy {
        self.policy
    }
    pub fn has_pending_matching(&self, predicate: impl Fn(&QueuedCommand) -> bool) -> bool {
        self.queue.any(&predicate)
            || self.writing.iter().any(&predicate)
            || self
                .publication
                .as_ref()
                .is_some_and(|p| p.stops.iter().any(predicate))
    }
    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }
    /// The admission owner uses this during the same turn as queue acceptance.
    /// Include a selected writer, not just work still waiting in the queue.
    pub fn has_pending_motor(&self, device: &crate::DeviceId) -> bool {
        self.queue.has_motor_for(device)
            || self
                .writing
                .iter()
                .any(|c| &c.device_id == device && matches!(c.command, crate::Command::Motor(_)))
    }
    /// Invalidate old producers immediately. Returned cancellations include STOP:
    /// the owner may explicitly submit a NEW degraded STOP after revalidating its
    /// original mapping, authorization and deadline. Nothing is silently replayed.
    /// An in-flight frame remains tracked until finish_write reports its boundary.
    pub fn storage_failed(&mut self) -> Result<Vec<Cancellation>, ArbiterError> {
        if !self.policy.storage_healthy {
            return Ok(Vec::new());
        }
        self.policy.storage_healthy = false;
        let Some(control) = self.policy.epochs.control.checked_add(1) else {
            self.policy.valid_configuration_loaded = false;
            return Err(ArbiterError::CounterExhausted);
        };
        self.policy.epochs.control = control;
        let mut cancelled = self.queue.clear(CancelReason::ControlChanged);
        if let Some(publication) = &mut self.publication {
            publication.failed = true;
            cancelled.extend(publication.stops.drain(..).map(|command| Cancellation {
                command,
                reason: CancelReason::ControlChanged,
            }));
        }
        Ok(cancelled)
    }
    /// Call after database read/write health checks and device synchronization.
    /// STOP must finish under the existing degraded mapping before recovery;
    /// ordinary queued degraded actions are cancelled, never upgraded in place.
    /// A revision mismatch enters maintenance and requires configuration reload.
    pub fn storage_recovered(
        &mut self,
        durable_revision: u64,
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        if self.policy.storage_healthy || self.publication.as_ref().is_some_and(|p| !p.committed) {
            return Err(ArbiterError::InvalidState);
        }
        if durable_revision != self.policy.epochs.configuration {
            self.policy.valid_configuration_loaded = false;
            self.policy.gateway_ready = false;
            return Err(ArbiterError::RevisionMismatch);
        }
        if !self.policy.valid_configuration_loaded
            || (!self.policy.gateway_ready && self.publication.is_none())
        {
            return Err(ArbiterError::NotSynchronized);
        }
        if !self.writing.is_empty() || self.queue.has_stop() {
            return Err(ArbiterError::Busy);
        }
        let control = self
            .policy
            .epochs
            .control
            .checked_add(1)
            .ok_or(ArbiterError::CounterExhausted)?;
        self.policy.epochs.control = control;
        let cancelled = self.queue.clear(CancelReason::ControlChanged);
        self.policy.storage_healthy = true;
        Ok(cancelled)
    }
    /// Install a strictly newer connection generation while control is closed.
    /// The owner must persist returned cancellations and query the new mapping
    /// before calling gateway_resynchronized and storage_recovered. This method
    /// alone never reopens admission or asserts that a socket is healthy.
    pub fn transport_reconnected(
        &mut self,
        transport_epoch: u64,
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        if self.policy.storage_healthy || self.publication.is_some() {
            return Err(ArbiterError::InvalidState);
        }
        if !self.writing.is_empty() {
            return Err(ArbiterError::Busy);
        }
        if transport_epoch <= self.policy.epochs.transport {
            return Err(ArbiterError::NotSynchronized);
        }
        self.policy.epochs.transport = transport_epoch;
        self.policy.gateway_ready = false;
        Ok(self.queue.clear(CancelReason::TransportChanged))
    }

    /// Evidence from the connection owner after querying/synchronizing the active
    /// mapping. Does not permit a new connection to reuse an old transport epoch.
    /// Reconnection lifecycle is handled separately from this readiness update.
    pub fn gateway_resynchronized(
        &mut self,
        transport_epoch: u64,
        revision: u64,
    ) -> Result<(), ArbiterError> {
        if self.publication.is_some()
            || !self.policy.valid_configuration_loaded
            || transport_epoch != self.policy.epochs.transport
            || revision != self.policy.epochs.configuration
        {
            return Err(ArbiterError::NotSynchronized);
        }
        self.policy.gateway_ready = true;
        Ok(())
    }
    pub fn submit(
        &mut self,
        command: QueuedCommand,
        access: Access,
        now: u64,
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        self.policy
            .check(&command, access, now)
            .map_err(ArbiterError::Admission)?;
        self.queue
            .enqueue(command, now, self.policy.epochs)
            .map_err(ArbiterError::Queue)
    }
    /// All-or-nothing admission for one normalized request. No earlier member,
    /// coalescing, STOP cancellation, or ID consumption survives a later rejection.
    /// Durable acceptance must still be coordinated by the runtime owner.
    pub fn submit_batch(
        &mut self,
        commands: Vec<(QueuedCommand, Access)>,
        now: u64,
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        match self
            .submit_batch_committed(commands, now, |_, _| Ok::<(), std::convert::Infallible>(()))
        {
            Ok(result) => Ok(result),
            Err(BatchCommitError::Admission(error)) => Err(error),
            Err(BatchCommitError::Persistence(never)) => match never {},
        }
    }
    /// Stage all queue effects, then commit their audit before publishing them.
    /// The synchronous callback runs under exclusive arbiter ownership. It must
    /// commit all commands AND cancellations together, and must not dispatch I/O.
    pub fn submit_batch_committed<E>(
        &mut self,
        commands: Vec<(QueuedCommand, Access)>,
        now: u64,
        commit: impl FnOnce(&[QueuedCommand], &[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        self.submit_batch_with_order(commands, now, commit, false, None)
    }
    /// Trusted runtime rule nodes retain every ordered output. This is not an
    /// authentication API: the runtime must supply its durable rule audit.
    pub fn submit_ordered_rule_batch_committed<E>(
        &mut self,
        commands: Vec<(QueuedCommand, Access)>,
        now: u64,
        commit: impl FnOnce(&[QueuedCommand], &[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        let run = commands.first().and_then(|(command, _)| command.run_id);
        if run.is_none_or(|run| run == 0)
            || commands.iter().any(|(command, _)| {
                command.origin != crate::queue::Origin::Rule || command.run_id != run
            })
        {
            return Err(BatchCommitError::Admission(ArbiterError::InvalidState));
        }
        self.submit_batch_with_order(commands, now, commit, true, None)
    }
    /// Trusted module operation: cancellation and module effects commit in one
    /// transaction. An in-flight write is not undone; queued unrelated outputs
    /// and every STOP remain available to the sole transport owner.
    pub fn submit_module_off_batch_committed<E>(
        &mut self,
        commands: Vec<(QueuedCommand, Access)>,
        affected: &std::collections::BTreeSet<crate::DeviceId>,
        now: u64,
        commit: impl FnOnce(&[QueuedCommand], &[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        self.submit_module_off_with_reset_committed(commands, None, affected, now, commit)
    }
    /// The reviewed native key may reserve one green reset after the module
    /// batch. Its dispatch dependency is enforced by the execution owner.
    pub fn submit_module_off_with_reset_committed<E>(
        &mut self,
        mut modules: Vec<(QueuedCommand, Access)>,
        reset: Option<(QueuedCommand, Access)>,
        affected: &std::collections::BTreeSet<crate::DeviceId>,
        now: u64,
        commit: impl FnOnce(&[QueuedCommand], &[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        if modules.is_empty()
            || affected.is_empty()
            || modules
                .iter()
                .any(|(c, _)| c.command != crate::Command::ModuleAllOff)
            || reset.as_ref().is_some_and(|(c, _)| {
                c.command != crate::Command::PanelLed(false) || affected.contains(&c.device_id)
            })
        {
            return Err(BatchCommitError::Admission(ArbiterError::InvalidState));
        }
        modules.extend(reset);
        self.submit_batch_with_order(modules, now, commit, false, Some(affected))
    }
    fn submit_batch_with_order<E>(
        &mut self,
        commands: Vec<(QueuedCommand, Access)>,
        now: u64,
        commit: impl FnOnce(&[QueuedCommand], &[Cancellation]) -> Result<(), E>,
        ordered_rule: bool,
        supersede_devices: Option<&std::collections::BTreeSet<crate::DeviceId>>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        if commands.len() > 128 {
            return Err(BatchCommitError::Admission(ArbiterError::BatchTooLarge));
        }
        let mut staged = self.queue.clone();
        let mut cancelled = supersede_devices
            .map(|devices| staged.cancel_devices(devices))
            .unwrap_or_default();
        let mut accepted = Vec::with_capacity(commands.len());
        for (command, access) in commands {
            self.policy
                .check(&command, access, now)
                .map_err(|e| BatchCommitError::Admission(ArbiterError::Admission(e)))?;
            let effects = if ordered_rule {
                staged.enqueue_ordered_rule(command.clone(), now, self.policy.epochs)
            } else {
                staged.enqueue(command.clone(), now, self.policy.epochs)
            };
            cancelled
                .extend(effects.map_err(|e| BatchCommitError::Admission(ArbiterError::Queue(e)))?);
            accepted.push(command);
        }
        commit(&accepted, &cancelled).map_err(BatchCommitError::Persistence)?;
        self.queue = staged;
        Ok(cancelled)
    }
    /// Persist cancellation before replacing the queue. In-flight writes are not undone.
    pub fn cancel_run_committed<E>(
        &mut self,
        run: u64,
        commit: impl FnOnce(&[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, E> {
        let mut staged = self.queue.clone();
        let cancelled = staged.cancel_run(run);
        commit(&cancelled)?;
        self.queue = staged;
        Ok(cancelled)
    }
    /// Cancel one logical member's exact expanded IDs, retaining peers from
    /// the same admitted due phase. A selected writer cannot be recalled.
    pub fn cancel_run_members_committed<E>(
        &mut self,
        run: u64,
        ids: &std::collections::BTreeSet<u64>,
        commit: impl FnOnce(&[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, BatchCommitError<E>> {
        if ids.is_empty()
            || ids.len() > 128
            || self
                .queue
                .any(|c| ids.contains(&c.id) && c.run_id != Some(run))
        {
            return Err(BatchCommitError::Admission(ArbiterError::InvalidState));
        }
        let mut staged = self.queue.clone();
        let cancelled = staged.cancel_ids_with_reason(ids, CancelReason::RunCancelled);
        commit(&cancelled).map_err(BatchCommitError::Persistence)?;
        self.queue = staged;
        Ok(cancelled)
    }
    /// Cancel exact still-queued tickets after the audit commits. A writer
    /// already selected from the queue retains its separate write obligation.
    pub fn cancel_queued_ids_committed<E>(
        &mut self,
        ids: &std::collections::BTreeSet<u64>,
        reason: CancelReason,
        commit: impl FnOnce(&[Cancellation]) -> Result<(), E>,
    ) -> Result<Vec<Cancellation>, E> {
        let mut staged = self.queue.clone();
        let cancelled = staged.cancel_ids_with_reason(ids, reason);
        commit(&cancelled)?;
        self.queue = staged;
        Ok(cancelled)
    }
    /// Resolve current access for every candidate. Only one write may be in flight.
    /// A returned command must be encoded against its unchanged configuration.
    pub fn dispatch(
        &mut self,
        now: u64,
        mut access: impl FnMut(&QueuedCommand) -> Access,
    ) -> Result<Dispatch, ArbiterError> {
        self.dispatch_eligible(now, &mut access, |_| true)
    }
    pub fn dispatch_eligible(
        &mut self,
        now: u64,
        mut access: impl FnMut(&QueuedCommand) -> Access,
        mut eligible: impl FnMut(&QueuedCommand) -> bool,
    ) -> Result<Dispatch, ArbiterError> {
        if !self.writing.is_empty() {
            return Err(ArbiterError::Busy);
        }
        if self.publication.is_some() {
            return Err(ArbiterError::InvalidState);
        }
        let mut result = Dispatch::default();
        loop {
            let polled = self
                .queue
                .poll_eligible(now, self.policy.epochs, &mut eligible);
            result.cancelled.extend(polled.cancelled);
            let Some(command) = polled.next else {
                return Ok(result);
            };
            if let Err(error) = self.policy.check(&command, access(&command), now) {
                result.rejected.push((command, error));
                continue;
            }
            self.writing.push(command.clone());
            result.command = Some(command);
            return Ok(result);
        }
    }
    /// Extend a selected writer without releasing its ownership. The caller
    /// supplies the exact admitted request/phase identity and byte-capacity
    /// predicate. Inspect the next normal-priority candidate before consuming:
    /// an independent STOP or request is never skipped to fill a batch.
    pub fn extend_write(
        &mut self,
        now: u64,
        mut access: impl FnMut(&QueuedCommand) -> Access,
        mut same_phase: impl FnMut(&QueuedCommand) -> bool,
        mut eligible: impl FnMut(&QueuedCommand) -> bool,
    ) -> Result<Dispatch, ArbiterError> {
        if self.writing.is_empty() || self.writing.len() >= 32 || self.publication.is_some() {
            return Err(ArbiterError::InvalidState);
        }
        let mut candidate = self.queue.clone();
        let preview = candidate.poll_eligible(now, self.policy.epochs, &mut eligible);
        let Some(next) = preview.next else {
            return Ok(Dispatch::default());
        };
        if next.is_stop() || !same_phase(&next) {
            return Ok(Dispatch::default());
        }
        let polled = self
            .queue
            .poll_eligible(now, self.policy.epochs, &mut eligible);
        let mut result = Dispatch {
            cancelled: polled.cancelled,
            ..Default::default()
        };
        let command = polled.next.ok_or(ArbiterError::InvalidState)?;
        if command.id != next.id {
            return Err(ArbiterError::InvalidState);
        }
        if let Err(error) = self.policy.check(&command, access(&command), now) {
            result.rejected.push((command, error));
        } else {
            self.writing.push(command.clone());
            result.command = Some(command);
        }
        Ok(result)
    }
    /// Apply exact IDs returned by a committed local gateway fault transaction.
    /// Missing owned rows are an invariant failure; callers fail the service.
    /// No global generation/readiness or unrelated B writer is changed.
    pub fn apply_audited_gateway_fault(
        &mut self,
        cancelled: &[u64],
        uncertain: &[u64],
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        let ids = cancelled
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if ids.len() != cancelled.len()
            || !self.queue.contains_ids(&ids)
            || uncertain.len() > 32
            || uncertain
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != uncertain.len()
            || uncertain
                .iter()
                .any(|id| !self.writing.iter().any(|command| command.id == *id))
        {
            return Err(ArbiterError::InvalidState);
        }
        let rows = self.queue.cancel_ids(&ids);
        self.writing
            .retain(|command| !uncertain.contains(&command.id));
        if let Some(publication) = &mut self.publication {
            publication.failed = true;
        }
        Ok(rows)
    }
    /// Release a selected command rejected before any transport write.
    /// The caller must first durably record its cancellation.
    pub fn cancel_selected_before_write(&mut self, id: u64) -> Result<(), ArbiterError> {
        if !self.writing.iter().any(|c| c.id == id) {
            return Err(ArbiterError::InvalidState);
        }
        self.writing.retain(|command| command.id != id);
        Ok(())
    }
    /// false means the frame boundary was not completed; publication must fail.
    /// Execution outcome/partial byte count must also be recorded by the caller.
    pub fn finish_write(&mut self, id: u64, complete_frame: bool) -> Result<(), ArbiterError> {
        if !self.writing.iter().any(|c| c.id == id) {
            return Err(ArbiterError::InvalidState);
        }
        self.writing.retain(|command| command.id != id);
        if !complete_frame {
            self.policy.gateway_ready = false;
            if let Some(publication) = &mut self.publication {
                publication.failed = true;
            }
        }
        Ok(())
    }
    /// Call only after full candidate validation; return cancellations for journal
    /// persistence. Existing asynchronous producers must retain their old epochs.
    pub fn begin_publication(
        &mut self,
        now: u64,
        deadline: u64,
    ) -> Result<Vec<Cancellation>, ArbiterError> {
        if self.publication.is_some() {
            return Err(ArbiterError::InvalidState);
        }
        if deadline <= now {
            return Err(ArbiterError::Expired);
        }
        if !self.policy.storage_healthy {
            return Err(ArbiterError::Admission(Rejection::PersistenceUnavailable));
        }
        let control = self
            .policy
            .epochs
            .control
            .checked_add(1)
            .ok_or(ArbiterError::CounterExhausted)?;
        let (cancelled, stops) = self.queue.publication_barrier();
        self.policy.epochs.control = control;
        self.policy.publishing = true;
        self.publication = Some(Publication {
            deadline,
            stops: stops.into(),
            failed: false,
            committed: false,
            transaction_started: false,
        });
        Ok(cancelled)
    }
    /// Immutable reserved STOPs only. The admission owner may bind these to the
    /// accepted publication before draining; it cannot add work through this view.
    pub fn publication_stops(&self) -> impl Iterator<Item = &QueuedCommand> {
        self.publication
            .iter()
            .flat_map(|publication| publication.stops.iter())
    }
    /// STOP retains the old configuration revision. Caller resolves its original
    /// mapping and authority; only this explicit drain can adopt the new control
    /// epoch. New external STOP requests are still rejected during publication.
    pub fn drain_stop(
        &mut self,
        now: u64,
        access: impl FnOnce(&QueuedCommand) -> Access,
    ) -> Result<Option<QueuedCommand>, ArbiterError> {
        if !self.writing.is_empty() {
            return Err(ArbiterError::Busy);
        }
        let publication = self
            .publication
            .as_mut()
            .ok_or(ArbiterError::InvalidState)?;
        if now >= publication.deadline {
            publication.failed = true;
            return Err(ArbiterError::Expired);
        }
        if publication.failed || publication.committed || publication.transaction_started {
            return Err(ArbiterError::DrainFailed);
        }
        let Some(original) = publication.stops.front() else {
            return Ok(None);
        };
        let mut command = original.clone();
        command.epochs.control = self.policy.epochs.control;
        let mut drain_policy = self.policy;
        drain_policy.publishing = false;
        let checked = if !drain_policy.storage_healthy {
            Err(Rejection::PersistenceUnavailable)
        } else {
            drain_policy.check(&command, access(original), now)
        };
        if let Err(error) = checked {
            publication.failed = true;
            return Err(ArbiterError::Admission(error));
        }
        publication.stops.pop_front();
        self.writing.push(command.clone());
        Ok(Some(command))
    }
    /// Must succeed before the storage owner starts its commit transaction.
    pub fn begin_commit(&mut self, now: u64) -> Result<(), ArbiterError> {
        if !self.policy.storage_healthy {
            return Err(ArbiterError::Admission(Rejection::PersistenceUnavailable));
        }
        let publication = self
            .publication
            .as_mut()
            .ok_or(ArbiterError::InvalidState)?;
        if now >= publication.deadline {
            publication.failed = true;
            return Err(ArbiterError::Expired);
        }
        if publication.failed || publication.committed || publication.transaction_started {
            return Err(ArbiterError::DrainFailed);
        }
        if !self.writing.is_empty() || !publication.stops.is_empty() {
            return Err(ArbiterError::Busy);
        }
        publication.transaction_started = true;
        Ok(())
    }
    /// Once a transaction starts, keep the owner serialized. An unknown result
    /// holds the barrier until the durable active pointer is read and resolved.
    /// Even a commit completed after the deadline must install the durable truth.
    pub fn record_commit(&mut self, result: CommitResult) -> Result<(), ArbiterError> {
        let publication = self
            .publication
            .as_mut()
            .ok_or(ArbiterError::InvalidState)?;
        if !self.writing.is_empty()
            || !publication.stops.is_empty()
            || publication.committed
            || !publication.transaction_started
        {
            return Err(ArbiterError::InvalidState);
        }
        match result {
            CommitResult::Unknown => {
                publication.failed = true;
                self.policy.gateway_ready = false;
            }
            CommitResult::RolledBack => {
                self.publication = None;
                self.policy.publishing = false;
            }
            CommitResult::Committed(revision) => {
                if revision <= self.policy.epochs.configuration {
                    return Err(ArbiterError::InvalidState);
                }
                self.policy.epochs.configuration = revision;
                self.policy.gateway_ready = false;
                publication.committed = true;
            }
        }
        Ok(())
    }
    /// Complete a committed publication after querying its mapping on fresh streams.
    /// No dispatch becomes possible between installing the epoch and readiness.
    pub fn publication_recovered(&mut self, epoch: u64, revision: u64) -> Result<(), ArbiterError> {
        if self.policy.storage_healthy
            || !self.publication.as_ref().is_some_and(|p| p.committed)
            || !self.policy.valid_configuration_loaded
            || revision != self.policy.epochs.configuration
            || epoch <= self.policy.epochs.transport
        {
            return Err(ArbiterError::NotSynchronized);
        }
        if !self.writing.is_empty() || self.queue.has_stop() {
            return Err(ArbiterError::Busy);
        }
        let control = self
            .policy
            .epochs
            .control
            .checked_add(1)
            .ok_or(ArbiterError::CounterExhausted)?;
        self.policy.epochs.control = control;
        self.policy.epochs.transport = epoch;
        self.policy.storage_healthy = true;
        self.policy.gateway_ready = true;
        self.policy.publishing = false;
        self.publication = None;
        Ok(())
    }
    /// Caller installs immutable mappings and completes synchronization first.
    pub fn synchronized(&mut self, revision: u64) -> Result<(), ArbiterError> {
        if !self.policy.storage_healthy {
            return Err(ArbiterError::Admission(Rejection::PersistenceUnavailable));
        }
        let publication = self
            .publication
            .as_ref()
            .ok_or(ArbiterError::InvalidState)?;
        if !publication.committed || revision != self.policy.epochs.configuration {
            return Err(ArbiterError::NotSynchronized);
        }
        self.policy.gateway_ready = true;
        self.policy.publishing = false;
        self.publication = None;
        Ok(())
    }
    /// Abort before starting the database transaction. Never call this for an
    /// unknown commit result; resolve the durable pointer via record_commit.
    pub fn abort_before_commit(&mut self) -> Result<Vec<Cancellation>, ArbiterError> {
        if !self.writing.is_empty() {
            return Err(ArbiterError::Busy);
        }
        let p = self
            .publication
            .as_ref()
            .ok_or(ArbiterError::InvalidState)?;
        if p.committed || p.transaction_started {
            return Err(ArbiterError::InvalidState);
        }
        let p = self.publication.take().unwrap();
        self.policy.publishing = false;
        Ok(p.stops
            .into_iter()
            .map(|command| Cancellation {
                command,
                reason: CancelReason::ConfigurationChanged,
            })
            .collect())
    }
}
