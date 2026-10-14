//! Current-owner motor evidence leases for separately reviewed guarded rules.
//! No command, authority grant, model installation or actual feedback is made.
use crate::{
    admission::{Admission, TaskGeneration},
    curtain_owner::Owner,
};
use hap::server::WriteStatus;
use homesense_core::{
    curtain_state::{Provenance, Scope, Snapshot},
    rule_motor::{Evidence, Reading, Request},
};
use uuid::Uuid;
#[derive(Clone)]
struct Witness {
    request: Request,
    snapshot: Option<Snapshot>,
    pending_identity: Vec<(u64, Uuid, homesense_core::MotorAction)>,
}
/// Opaque, in-process proof; only the admission thread can construct it.
#[derive(Clone)]
pub(crate) struct Lease {
    controller: Uuid,
    scope: Scope,
    captured_ms: u64,
    readings: Vec<Reading>,
    witnesses: Vec<Witness>,
}
fn equivalent(a: Option<Snapshot>, b: Option<Snapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.scope == b.scope
                && a.observed_movement == b.observed_movement
                && a.inferred_movement == b.inferred_movement
                && a.last_written == b.last_written
                && a.pending == b.pending
        }
        _ => false,
    }
}
impl Lease {
    pub fn capture(
        controller: Uuid,
        scope: Scope,
        requests: Vec<Request>,
        allowed: &[Request],
        owner: &mut Owner,
        now: u64,
    ) -> Result<Self, WriteStatus> {
        if requests.is_empty() || requests.len() > 256 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let mut readings = vec![];
        let mut witnesses = vec![];
        for request in requests {
            if !allowed.contains(&request) {
                return Err(WriteStatus::InsufficientPrivileges);
            }
            let snapshot = owner.snapshot(&request.predicate.device, now);
            let chosen = snapshot.and_then(|s| {
                s.observed_movement.or_else(|| {
                    (request.predicate.evidence == Evidence::ReviewedCommandEstimate)
                        .then_some(s.inferred_movement)
                        .flatten()
                })
            });
            let known = chosen.filter(|f| {
                f.source == Provenance::Observed
                    || (request.predicate.evidence == Evidence::ReviewedCommandEstimate
                        && snapshot.is_some_and(|s| s.last_written.is_some()))
            });
            readings.push(Reading {
                request: request.clone(),
                scope,
                captured_ms: known
                    .filter(|f| f.source == Provenance::Observed)
                    .map_or(now, |f| f.at_ms),
                movement: known.map(|f| f.value),
                provenance: known.map(|f| f.source),
                pending: snapshot.is_some_and(|s| s.pending.is_some()),
            });
            let pending_identity = owner.pending_identity(&request.predicate.device);
            witnesses.push(Witness {
                request,
                snapshot,
                pending_identity,
            });
        }
        Ok(Self {
            controller,
            scope,
            captured_ms: now,
            readings,
            witnesses,
        })
    }
    pub fn input(&self, now: u64) -> homesense_core::rule_motor::Snapshot<'_> {
        homesense_core::rule_motor::Snapshot {
            readings: &self.readings,
            scope: Some(self.scope),
            now_ms: now,
        }
    }
    /// Retain unknown as well as known consulted reads; a later change cannot
    /// revive a skipped branch or commit outside-node actions from an old view.
    pub fn used(&self, reads: &[homesense_core::rule_motor::Read]) -> Option<Self> {
        let mut lease = self.clone();
        lease
            .readings
            .retain(|r| reads.iter().any(|read| read.request == r.request));
        lease
            .witnesses
            .retain(|w| reads.iter().any(|read| read.request == w.request));
        (!lease.witnesses.is_empty()).then_some(lease)
    }
    pub fn validate(
        &self,
        controller: Uuid,
        scope: Scope,
        allowed: &[Request],
        owner: &mut Owner,
        now: u64,
    ) -> Result<(), WriteStatus> {
        if self.controller != controller || self.scope != scope || now < self.captured_ms {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        for witness in &self.witnesses {
            if !allowed.contains(&witness.request)
                || now - self.captured_ms >= witness.request.predicate.max_age_ms
                || !equivalent(
                    witness.snapshot,
                    owner.snapshot(&witness.request.predicate.device, now),
                )
                || witness.pending_identity
                    != owner.pending_identity(&witness.request.predicate.device)
            {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            let reading = self
                .readings
                .iter()
                .find(|r| r.request == witness.request)
                .ok_or(WriteStatus::InvalidValueInRequest)?;
            if reading.movement.is_some()
                && now
                    .checked_sub(reading.captured_ms)
                    .is_none_or(|age| age >= witness.request.predicate.max_age_ms)
            {
                return Err(WriteStatus::InvalidValueInRequest);
            }
        }
        Ok(())
    }
}
pub(crate) async fn capture(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    nodes: &[homesense_core::rule::Node],
) -> Result<Option<Lease>, String> {
    let requests = homesense_core::rule_motor::requests(nodes).map_err(str::to_owned)?;
    if requests.is_empty() {
        return Ok(None);
    }
    admission
        .motor_rule_lease(controller, generation, requests)
        .await
        .map(Some)
        .map_err(|e| format!("motor evidence scope: {e:?}"))
}

/// One admitted batch owns this witness. Only its own accepted / durable Written
/// effects may advance it; the original predicate deadline is never extended.
pub(crate) struct QueuedLease {
    lease: Lease,
    valid: bool,
}
impl QueuedLease {
    pub fn admitted(mut lease: Lease, owner: &mut Owner, now: u64) -> Self {
        lease.rebase(owner, now);
        Self { lease, valid: true }
    }
    pub fn check(
        &mut self,
        scope: Scope,
        allowed: &[Request],
        owner: &mut Owner,
        now: u64,
        authorized: bool,
    ) -> bool {
        self.valid &= authorized
            && self
                .lease
                .validate(self.lease.controller, scope, allowed, owner, now)
                .is_ok();
        self.valid
    }
    pub fn controller(&self) -> Uuid {
        self.lease.controller
    }
    pub fn own_written(&mut self, owner: &mut Owner, now: u64) {
        if self.valid {
            self.lease.rebase(owner, now);
        }
    }
}
impl Lease {
    fn rebase(&mut self, owner: &mut Owner, now: u64) {
        for witness in &mut self.witnesses {
            witness.snapshot = owner.snapshot(&witness.request.predicate.device, now);
            witness.pending_identity = owner.pending_identity(&witness.request.predicate.device);
        }
    }
}
