//! A bounded whole-house sequence advances only after one module is confirmed.
//!
//! This planner issues no commands. Every attempt needs new durable receipts;
//! future modules have no queued commands or send leases yet. The live owner
//! must revalidate source, manual versions, configuration and transport epochs
//! on every poll and acceptance, and serialize the requested reads on the bus.
use crate::{Command, DeviceId, Observation};
use std::collections::{BTreeMap, BTreeSet};

pub const TOTAL_MS: u64 = 10_000;
pub const SEND_MS: u64 = 1_000;
pub const PASSIVE_REPLY_MS: u64 = 100;
pub const RESPONSE_MS: u64 = 500;
pub const RETRY_DELAY_MS: u64 = 200;
pub const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub id: String,
    pub targets: BTreeMap<DeviceId, Command>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub observation: Option<Observation>,
    pub confirmed: bool,
    pub count: u64,
    pub received_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Accepted,
    Writing,
    Written {
        at_ms: u64,
    },
    /// Includes uncertain I/O. Recovery must retire this sequence; it cannot replay.
    Retired,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub proposal: u64,
    pub module: String,
    pub attempt: u8,
    pub send_before_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub commands: BTreeSet<u64>,
    pub before_counts: BTreeMap<DeviceId, u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Confirmed,
    Cancelled,
    Deadline,
    DeliveryNotProven {
        module: String,
    },
    NoConfirmation {
        module: String,
        missing: BTreeSet<DeviceId>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Send(Attempt),
    /// Query only these missing ports of the current module. No other module
    /// may be queried or activated under this sequence's bus reservation.
    Read {
        module: String,
        missing: BTreeSet<DeviceId>,
    },
    Wait,
    Finished(End),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidScope,
    ClockRegression,
    InvalidReceipt,
    ExhaustedCounter,
}
#[derive(Clone, Debug)]
enum Phase {
    Ready,
    Proposed(Attempt),
    Waiting {
        attempt: Attempt,
        receipt: Receipt,
        accepted_ms: u64,
        read_requested: bool,
    },
    Delay {
        until: u64,
    },
    End(End),
}
pub struct Sequence {
    modules: Vec<Module>,
    index: usize,
    attempt: u8,
    proposal: u64,
    used_commands: BTreeSet<u64>,
    last_ms: u64,
    deadline: u64,
    phase: Phase,
}
impl Sequence {
    pub fn new(modules: Vec<Module>, now: u64) -> Result<Self, Error> {
        let mut names = BTreeSet::new();
        let mut ports = BTreeSet::new();
        let mut direction = None;
        if modules.is_empty() || modules.len() > 16 {
            return Err(Error::InvalidScope);
        }
        for module in &modules {
            if module.id.trim().is_empty() || !names.insert(&module.id) || module.targets.len() != 8
            {
                return Err(Error::InvalidScope);
            }
            for (id, command) in &module.targets {
                let on = match command {
                    Command::Switch(on) => *on,
                    Command::SetLevel {
                        level,
                        transition: crate::LevelTransition::Immediate,
                    } if matches!(level.get(), 0 | 100) => level.get() == 100,
                    _ => return Err(Error::InvalidScope),
                };
                if !ports.insert(id) || direction.is_some_and(|previous| previous != on) {
                    return Err(Error::InvalidScope);
                }
                direction = Some(on);
            }
        }
        Ok(Self {
            modules,
            index: 0,
            attempt: 0,
            proposal: 0,
            used_commands: BTreeSet::new(),
            last_ms: now,
            deadline: now.checked_add(TOTAL_MS).ok_or(Error::ExhaustedCounter)?,
            phase: Phase::Ready,
        })
    }
    fn clock(&mut self, now: u64) -> Result<(), Error> {
        if now < self.last_ms {
            return Err(Error::ClockRegression);
        }
        self.last_ms = now;
        Ok(())
    }
    fn finish(&mut self, end: End) -> Decision {
        self.phase = Phase::End(end.clone());
        Decision::Finished(end)
    }
    pub fn deadline_ms(&self) -> u64 {
        self.deadline
    }
    pub fn current(&self) -> Option<&Module> {
        self.modules.get(self.index)
    }
    pub fn confirmed_modules(&self) -> usize {
        self.index
    }
    pub fn end(&self) -> Option<&End> {
        if let Phase::End(end) = &self.phase {
            Some(end)
        } else {
            None
        }
    }
    pub fn accepted(&mut self, proposal: u64, receipt: Receipt, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let Phase::Proposed(attempt) = &self.phase else {
            return Err(Error::InvalidReceipt);
        };
        if attempt.proposal != proposal
            || now >= attempt.send_before_ms
            || now >= self.deadline
            || !matches!(receipt.commands.len(), 1 | 8)
            || receipt.commands.contains(&0)
            || !receipt.commands.is_disjoint(&self.used_commands)
            || receipt
                .before_counts
                .keys()
                .ne(self.modules[self.index].targets.keys())
        {
            return Err(Error::InvalidReceipt);
        }
        self.attempt = attempt.attempt;
        self.used_commands.extend(receipt.commands.iter().copied());
        self.phase = Phase::Waiting {
            attempt: attempt.clone(),
            receipt,
            accepted_ms: now,
            read_requested: false,
        };
        Ok(())
    }
    /// A rejected, definitely unaccepted proposal may be offered again. A late
    /// reply for the old proposal is rejected even when it names the same module.
    pub fn defer(&mut self, proposal: u64, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        if !matches!(&self.phase, Phase::Proposed(a) if a.proposal == proposal) {
            return Err(Error::InvalidReceipt);
        }
        self.phase = Phase::Ready;
        Ok(())
    }
    pub fn poll(
        &mut self,
        now: u64,
        owner_valid: bool,
        bus_quiet: bool,
        deliveries: &BTreeMap<u64, Delivery>,
        samples: &BTreeMap<DeviceId, Sample>,
    ) -> Result<Decision, Error> {
        self.clock(now)?;
        if let Phase::End(end) = &self.phase {
            return Ok(Decision::Finished(end.clone()));
        }
        if !owner_valid {
            return Ok(self.finish(End::Cancelled));
        }
        if now >= self.deadline {
            return Ok(self.finish(End::Deadline));
        }
        loop {
            match &mut self.phase {
                Phase::Ready => {
                    if !bus_quiet {
                        return Ok(Decision::Wait);
                    }
                    self.proposal = self
                        .proposal
                        .checked_add(1)
                        .ok_or(Error::ExhaustedCounter)?;
                    let attempt = Attempt {
                        proposal: self.proposal,
                        module: self.modules[self.index].id.clone(),
                        attempt: self.attempt + 1,
                        send_before_ms: now.saturating_add(SEND_MS).min(self.deadline),
                    };
                    self.phase = Phase::Proposed(attempt.clone());
                    return Ok(Decision::Send(attempt));
                }
                Phase::Proposed(attempt) => {
                    if now >= attempt.send_before_ms {
                        let module = attempt.module.clone();
                        return Ok(self.finish(End::DeliveryNotProven { module }));
                    }
                    return Ok(Decision::Wait);
                }
                Phase::Waiting {
                    attempt,
                    receipt,
                    accepted_ms,
                    read_requested,
                } => {
                    let mut written = 0;
                    let mut all_written = true;
                    for id in &receipt.commands {
                        match deliveries.get(id) {
                            Some(Delivery::Written { at_ms })
                                if *at_ms < *accepted_ms || *at_ms >= attempt.send_before_ms =>
                            {
                                let module = attempt.module.clone();
                                return Ok(self.finish(End::DeliveryNotProven { module }));
                            }
                            Some(Delivery::Written { at_ms }) if *at_ms <= now => {
                                written = written.max(*at_ms)
                            }
                            Some(Delivery::Retired) => {
                                let module = attempt.module.clone();
                                return Ok(self.finish(End::DeliveryNotProven { module }));
                            }
                            _ => all_written = false,
                        }
                    }
                    if !all_written {
                        if now >= attempt.send_before_ms {
                            let module = attempt.module.clone();
                            return Ok(self.finish(End::DeliveryNotProven { module }));
                        }
                        return Ok(Decision::Wait);
                    }
                    let module = &self.modules[self.index];
                    let missing = module
                        .targets
                        .iter()
                        .filter_map(|(id, target)| {
                            let matches = samples.get(id).is_some_and(|sample| {
                                sample.confirmed
                                    && sample.count > receipt.before_counts[id]
                                    && sample.received_ms >= written
                                    && sample.received_ms <= written.saturating_add(RESPONSE_MS)
                                    && sample.received_ms <= now
                                    && sample
                                        .observation
                                        .is_some_and(|actual| target.target_matches(actual))
                            });
                            (!matches).then(|| id.clone())
                        })
                        .collect::<BTreeSet<_>>();
                    if missing.is_empty() {
                        // Receiving all ports is not itself permission to write
                        // over the final reply's remaining serial turnaround.
                        if !bus_quiet {
                            return Ok(Decision::Wait);
                        }
                        self.index += 1;
                        if self.index == self.modules.len() {
                            return Ok(self.finish(End::Confirmed));
                        }
                        self.attempt = 0;
                        self.phase = Phase::Ready;
                        continue;
                    }
                    if now >= written.saturating_add(RESPONSE_MS) {
                        if self.attempt >= MAX_ATTEMPTS {
                            let module = module.id.clone();
                            return Ok(self.finish(End::NoConfirmation { module, missing }));
                        }
                        self.phase = Phase::Delay {
                            until: now.saturating_add(RETRY_DELAY_MS),
                        };
                        return Ok(Decision::Wait);
                    }
                    if !*read_requested
                        && bus_quiet
                        && now >= written.saturating_add(PASSIVE_REPLY_MS)
                    {
                        *read_requested = true;
                        return Ok(Decision::Read {
                            module: module.id.clone(),
                            missing,
                        });
                    }
                    return Ok(Decision::Wait);
                }
                Phase::Delay { until } if now >= *until && bus_quiet => {
                    self.phase = Phase::Ready;
                }
                Phase::Delay { .. } => return Ok(Decision::Wait),
                Phase::End(end) => return Ok(Decision::Finished(end.clone())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn modules(n: usize) -> Vec<Module> {
        (0..n)
            .map(|m| Module {
                id: format!("m{m}"),
                targets: (1..=8)
                    .map(|ch| {
                        (
                            DeviceId::new(format!("m{m}-{ch}")).unwrap(),
                            Command::Switch(true),
                        )
                    })
                    .collect(),
            })
            .collect()
    }
    fn send(s: &mut Sequence, now: u64) -> Attempt {
        let Decision::Send(a) = s
            .poll(now, true, true, &BTreeMap::new(), &BTreeMap::new())
            .unwrap()
        else {
            panic!("expected a single current-module proposal");
        };
        a
    }
    fn receipt(s: &Sequence, id: u64) -> Receipt {
        Receipt {
            commands: [id].into(),
            before_counts: s
                .current()
                .unwrap()
                .targets
                .keys()
                .map(|p| (p.clone(), 4))
                .collect(),
        }
    }
    fn samples(s: &Sequence, at: u64) -> BTreeMap<DeviceId, Sample> {
        s.current()
            .unwrap()
            .targets
            .keys()
            .map(|p| {
                (
                    p.clone(),
                    Sample {
                        observation: Some(Observation::Switch(true)),
                        confirmed: true,
                        count: 5,
                        received_ms: at,
                    },
                )
            })
            .collect()
    }
    #[test]
    fn twelve_modules_wait_for_all_eight_ports_and_turnaround_before_advancing() {
        let mut s = Sequence::new(modules(12), 0).unwrap();
        let mut now = 1;
        let mut next = send(&mut s, now);
        for index in 0..12 {
            assert_eq!(next.module, format!("m{index}"));
            s.accepted(next.proposal, receipt(&s, index + 1), now)
                .unwrap();
            now += 10;
            let delivery = [(index + 1, Delivery::Written { at_ms: now })].into();
            assert_eq!(
                s.poll(now, true, true, &delivery, &BTreeMap::new())
                    .unwrap(),
                Decision::Wait
            );
            now += 20;
            let all = samples(&s, now);
            let mut partial = all.clone();
            partial.pop_last();
            assert_eq!(
                s.poll(now, true, true, &delivery, &partial).unwrap(),
                Decision::Wait
            );
            assert_eq!(
                s.poll(now, true, false, &delivery, &all).unwrap(),
                Decision::Wait
            );
            now += 20;
            let decision = s.poll(now, true, true, &delivery, &all).unwrap();
            if index == 11 {
                assert_eq!(decision, Decision::Finished(End::Confirmed));
            } else {
                let Decision::Send(a) = decision else {
                    panic!("next module missing");
                };
                next = a;
            }
        }
        assert_eq!(s.confirmed_modules(), 12);
    }
    #[test]
    fn delayed_processing_accepts_timely_feedback_but_not_late_feedback() {
        for late in [false,true] {
            let mut s=Sequence::new(modules(1),0).unwrap();
            let a=send(&mut s,1);
            s.accepted(a.proposal,receipt(&s,1),1).unwrap();
            let deliveries=[(1,Delivery::Written {at_ms:10})].into();
            let states=samples(&s,10+RESPONSE_MS+u64::from(late));
            let decision=s.poll(10+RESPONSE_MS+20,true,true,&deliveries,&states).unwrap();
            assert_eq!(s.confirmed_modules(),usize::from(!late));
            assert_eq!(decision,if late {Decision::Wait}else{Decision::Finished(End::Confirmed)});
        }
    }
    #[test]
    fn missing_module_queries_only_missing_ports_retries_same_module_and_never_starts_next() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let mut now = 1;
        for attempt in 1..=3 {
            let a = send(&mut s, now);
            assert_eq!((a.module.as_str(), a.attempt), ("m0", attempt));
            s.accepted(a.proposal, receipt(&s, u64::from(attempt)), now)
                .unwrap();
            let written = now + 10;
            let delivery = [(u64::from(attempt), Delivery::Written { at_ms: written })].into();
            let mut partial = samples(&s, written + 1);
            let silent = partial.pop_last().unwrap().0;
            let read = s
                .poll(written + PASSIVE_REPLY_MS, true, true, &delivery, &partial)
                .unwrap();
            assert_eq!(
                read,
                Decision::Read {
                    module: "m0".into(),
                    missing: [silent.clone()].into()
                }
            );
            assert_eq!(
                s.poll(
                    written + PASSIVE_REPLY_MS + 1,
                    true,
                    true,
                    &delivery,
                    &partial
                )
                .unwrap(),
                Decision::Wait
            );
            let timed_out = s
                .poll(written + RESPONSE_MS, true, true, &delivery, &partial)
                .unwrap();
            if attempt == 3 {
                assert_eq!(
                    timed_out,
                    Decision::Finished(End::NoConfirmation {
                        module: "m0".into(),
                        missing: [silent].into()
                    })
                );
            } else {
                assert_eq!(timed_out, Decision::Wait);
                now = written + RESPONSE_MS + RETRY_DELAY_MS;
                assert_eq!(
                    s.poll(now - 1, true, true, &delivery, &partial).unwrap(),
                    Decision::Wait
                );
            }
        }
        assert_eq!(s.confirmed_modules(), 0);
    }
    #[test]
    fn stale_wrong_target_unconfirmed_and_prewrite_samples_cannot_advance() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
        let delivery = [(1, Delivery::Written { at_ms: 10 })].into();
        for variant in 0..4 {
            let mut states = samples(&s, 11);
            for state in states.values_mut() {
                match variant {
                    0 => state.count = 4,
                    1 => state.confirmed = false,
                    2 => state.received_ms = 9,
                    _ => state.observation = Some(Observation::Switch(false)),
                }
            }
            assert_eq!(
                s.poll(12, true, true, &delivery, &states).unwrap(),
                Decision::Wait
            );
        }
        assert_eq!(s.confirmed_modules(), 0);
    }
    #[test]
    fn future_module_send_lease_starts_only_when_previous_module_confirms() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        assert_eq!(a.send_before_ms, 1001);
        s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
        let d = [(1, Delivery::Written { at_ms: 900 })].into();
        let states = samples(&s, 1200);
        let Decision::Send(next) = s.poll(1201, true, true, &d, &states).unwrap() else {
            panic!();
        };
        assert_eq!(next.send_before_ms, 2201);
        assert_eq!(s.deadline_ms(), TOTAL_MS);
    }
    #[test]
    fn cancellation_during_feedback_wait_or_retry_never_replays() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
        assert_eq!(
            s.poll(2, false, true, &BTreeMap::new(), &BTreeMap::new())
                .unwrap(),
            Decision::Finished(End::Cancelled)
        );
        assert_eq!(
            s.poll(3, true, true, &BTreeMap::new(), &BTreeMap::new())
                .unwrap(),
            Decision::Finished(End::Cancelled)
        );
    }
    #[test]
    fn uncertain_delivery_cannot_be_retried_as_a_missing_feedback_attempt() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
        assert_eq!(
            s.poll(
                2,
                true,
                true,
                &[(1, Delivery::Retired)].into(),
                &BTreeMap::new()
            )
            .unwrap(),
            Decision::Finished(End::DeliveryNotProven {
                module: "m0".into()
            })
        );
    }
    #[test]
    fn late_reused_or_partial_receipts_are_rejected() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        let r = receipt(&s, 1);
        s.defer(a.proposal, 2).unwrap();
        let newer = send(&mut s, 3);
        assert_eq!(
            s.accepted(a.proposal, r.clone(), 3),
            Err(Error::InvalidReceipt)
        );
        let mut wrong = r.clone();
        wrong.before_counts.pop_last();
        assert_eq!(
            s.accepted(newer.proposal, wrong, 3),
            Err(Error::InvalidReceipt)
        );
        s.accepted(newer.proposal, r.clone(), 3).unwrap();
        assert_eq!(s.accepted(newer.proposal, r, 3), Err(Error::InvalidReceipt));
    }
    #[test]
    fn absolute_operation_deadline_does_not_slide_with_retries() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 9999);
        assert_eq!(a.send_before_ms, TOTAL_MS);
        assert_eq!(
            s.poll(TOTAL_MS, true, true, &BTreeMap::new(), &BTreeMap::new())
                .unwrap(),
            Decision::Finished(End::Deadline)
        );
    }
    #[test]
    fn exact_dimmer_target_is_required_and_old_on_feedback_cannot_confirm_off() {
        for on in [true, false] {
            let mut group = modules(1);
            for target in group[0].targets.values_mut() {
                *target = Command::SetLevel {
                    level: crate::Percent::new(if on { 100 } else { 0 }).unwrap(),
                    transition: crate::LevelTransition::Immediate,
                };
            }
            let mut s = Sequence::new(group, 0).unwrap();
            let a = send(&mut s, 1);
            s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
            let d = [(1, Delivery::Written { at_ms: 10 })].into();
            let mut states = samples(&s, 11);
            for state in states.values_mut() {
                state.observation = Some(Observation::Level(crate::Percent::new(10).unwrap()));
            }
            assert_eq!(s.poll(12, true, true, &d, &states).unwrap(), Decision::Wait);
            for state in states.values_mut() {
                state.observation = Some(Observation::Level(
                    crate::Percent::new(if on { 100 } else { 0 }).unwrap(),
                ));
            }
            assert_eq!(
                s.poll(13, true, true, &d, &states).unwrap(),
                Decision::Finished(End::Confirmed)
            );
        }
    }
    #[test]
    fn retired_source_during_retry_delay_cancels_before_a_new_attempt() {
        let mut s = Sequence::new(modules(2), 0).unwrap();
        let a = send(&mut s, 1);
        s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
        let d = [(1, Delivery::Written { at_ms: 10 })].into();
        assert_eq!(
            s.poll(510, true, true, &d, &BTreeMap::new()).unwrap(),
            Decision::Wait
        );
        assert_eq!(
            s.poll(711, false, true, &d, &BTreeMap::new()).unwrap(),
            Decision::Finished(End::Cancelled)
        );
    }
    #[test]
    fn invalid_physical_module_scopes_are_rejected() {
        assert!(matches!(Sequence::new(vec![], 0), Err(Error::InvalidScope)));
        let mut group = modules(1);
        group[0].targets.pop_last();
        assert!(matches!(Sequence::new(group, 0), Err(Error::InvalidScope)));
        let mut group = modules(2);
        group[1].targets = group[0].targets.clone();
        assert!(matches!(Sequence::new(group, 0), Err(Error::InvalidScope)));
        let mut group = modules(2);
        *group[1].targets.values_mut().next().unwrap() = Command::Switch(false);
        assert!(matches!(Sequence::new(group, 0), Err(Error::InvalidScope)));
    }
    #[test]
    fn out_of_lease_or_preacceptance_write_receipt_is_not_a_confirmation() {
        for at_ms in [0, 1001] {
            let mut s = Sequence::new(modules(1), 0).unwrap();
            let a = send(&mut s, 1);
            s.accepted(a.proposal, receipt(&s, 1), 1).unwrap();
            assert_eq!(
                s.poll(
                    1002,
                    true,
                    true,
                    &[(1, Delivery::Written { at_ms })].into(),
                    &samples(&s, 1002)
                )
                .unwrap(),
                Decision::Finished(End::DeliveryNotProven {
                    module: "m0".into()
                })
            );
        }
    }
}
