//! Bounded candidate attribution, never an acknowledgement or observed state.
//! Only completed local writes may be recorded. Callers retain the corresponding
//! chain budget separately and must reject ambiguous/overflowed attribution.
use crate::{Command, DeviceId, Observation, queue::Epochs, rule_chain::Cause};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub cause: Cause,
    pub command_id: u64,
    pub written_ms: u64,
}
#[derive(Debug)]
pub enum Attribution {
    Independent,
    /// Inferred from target and timing; not evidence of the actual sender.
    Inferred(Candidate),
    Ambiguous,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    ForeignGeneration,
    Clock,
    Capacity,
    InvalidWrite,
}
struct Entry {
    device: DeviceId,
    target: Command,
    candidate: Candidate,
}
pub struct Candidates {
    epochs: Epochs,
    window_ms: u64,
    capacity: usize,
    now: u64,
    entries: Vec<Entry>,
    overflow_until: Option<u64>,
}
impl Candidates {
    pub fn new(epochs: Epochs, window_ms: u64, capacity: usize) -> Result<Self, Error> {
        if window_ms == 0 || window_ms > 60_000 || capacity == 0 || capacity > 4096 {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            epochs,
            window_ms,
            capacity,
            now: 0,
            entries: vec![],
            overflow_until: None,
        })
    }
    fn advance(&mut self, epochs: Epochs, now: u64) -> Result<(), Error> {
        if epochs != self.epochs {
            return Err(Error::ForeignGeneration);
        }
        if now < self.now {
            return Err(Error::Clock);
        }
        self.now = now;
        self.entries
            .retain(|e| now - e.candidate.written_ms < self.window_ms);
        if self.overflow_until.is_some_and(|end| now >= end) {
            self.overflow_until = None;
        }
        Ok(())
    }
    pub fn record(
        &mut self,
        epochs: Epochs,
        device: DeviceId,
        target: Command,
        candidate: Candidate,
    ) -> Result<(), Error> {
        if candidate.command_id == 0 || candidate.cause.event_id() == 0 {
            return Err(Error::InvalidWrite);
        }
        self.advance(epochs, candidate.written_ms)?;
        if !matches!(target, Command::Switch(_) | Command::SetLevel { .. }) {
            return Ok(());
        }
        if self
            .entries
            .iter()
            .any(|e| e.candidate.command_id == candidate.command_id)
        {
            return Err(Error::InvalidWrite);
        }
        if self.entries.len() == self.capacity {
            // Never silently evict a live constraint and restart its budget.
            self.overflow_until = Some(candidate.written_ms.saturating_add(self.window_ms));
            return Err(Error::Capacity);
        }
        self.entries.push(Entry {
            device,
            target,
            candidate,
        });
        Ok(())
    }
    pub fn attribute(
        &mut self,
        epochs: Epochs,
        device: &DeviceId,
        observed: Observation,
        observed_ms: u64,
    ) -> Result<Attribution, Error> {
        self.advance(epochs, observed_ms)?;
        if self.overflow_until.is_some() {
            return Err(Error::Capacity);
        }
        let mut matches = self
            .entries
            .iter()
            .filter(|e| &e.device == device && e.target.target_matches(observed));
        let Some(first) = matches.next() else {
            return Ok(Attribution::Independent);
        };
        // Even two events of the same root have different ancestry. Do not pick
        // whichever happens to appear first and weaken cycle detection.
        if matches.any(|e| {
            e.candidate.cause.correlation_id() != first.candidate.cause.correlation_id()
                || e.candidate.cause.event_id() != first.candidate.cause.event_id()
        }) {
            return Ok(Attribution::Ambiguous);
        }
        Ok(Attribution::Inferred(first.candidate.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rule::{Budget, Node},
        rule_chain::Chain,
    };
    fn epoch() -> Epochs {
        Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        }
    }
    fn cause(root: u128) -> Cause {
        let mut chain = Chain::new(root, 0, 1000, Budget::default(), 32).unwrap();
        chain
            .plan(
                &chain.root(),
                "test",
                &[] as &[Node],
                &Default::default(),
                0,
            )
            .unwrap()
            .cause
    }
    fn record(c: &mut Candidates, id: u64, root: u128, at: u64) -> Result<(), Error> {
        c.record(
            epoch(),
            DeviceId::new("lamp").unwrap(),
            Command::Switch(true),
            Candidate {
                cause: cause(root),
                command_id: id,
                written_ms: at,
            },
        )
    }
    #[test]
    fn exact_target_device_generation_and_half_open_window() {
        let mut c = Candidates::new(epoch(), 10, 4).unwrap();
        record(&mut c, 1, 1, 5).unwrap();
        let id = DeviceId::new("lamp").unwrap();
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Switch(false), 5)
                .unwrap(),
            Attribution::Independent
        ));
        assert!(matches!(
            c.attribute(
                epoch(),
                &DeviceId::new("other").unwrap(),
                Observation::Switch(true),
                5
            )
            .unwrap(),
            Attribution::Independent
        ));
        let mut foreign = epoch();
        foreign.transport += 1;
        assert_eq!(
            c.attribute(foreign, &id, Observation::Switch(true), 6)
                .unwrap_err(),
            Error::ForeignGeneration
        );
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Switch(true), 14)
                .unwrap(),
            Attribution::Inferred(_)
        ));
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Switch(true), 15)
                .unwrap(),
            Attribution::Independent
        ));
        assert_eq!(
            c.attribute(epoch(), &id, Observation::Switch(true), 14)
                .unwrap_err(),
            Error::Clock
        );
    }
    #[test]
    fn ambiguity_and_overflow_do_not_silently_create_independent_events() {
        let mut c = Candidates::new(epoch(), 10, 2).unwrap();
        let id = DeviceId::new("lamp").unwrap();
        record(&mut c, 1, 1, 0).unwrap();
        record(&mut c, 2, 2, 1).unwrap();
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Switch(true), 2)
                .unwrap(),
            Attribution::Ambiguous
        ));
        assert_eq!(record(&mut c, 3, 3, 2), Err(Error::Capacity));
        assert_eq!(
            c.attribute(epoch(), &id, Observation::Switch(true), 11)
                .unwrap_err(),
            Error::Capacity
        );
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Switch(true), 12)
                .unwrap(),
            Attribution::Independent
        ));
    }
    #[test]
    fn non_targets_are_excluded_and_duplicate_writes_are_rejected() {
        let mut c = Candidates::new(epoch(), 10, 1).unwrap();
        let id = DeviceId::new("lamp").unwrap();
        for target in [
            Command::Query,
            Command::PauseRamp,
            Command::PanelLed(true),
            Command::Motor(crate::MotorMotion::Stop),
        ] {
            c.record(
                epoch(),
                id.clone(),
                target,
                Candidate {
                    cause: cause(1),
                    command_id: 1,
                    written_ms: 0,
                },
            )
            .unwrap();
        }
        let level = crate::Percent::new(40).unwrap();
        c.record(
            epoch(),
            id.clone(),
            Command::SetLevel {
                level,
                transition: crate::LevelTransition::Ramp,
            },
            Candidate {
                cause: cause(1),
                command_id: 2,
                written_ms: 0,
            },
        )
        .unwrap();
        assert_eq!(record(&mut c, 2, 1, 0), Err(Error::InvalidWrite));
        assert!(matches!(
            c.attribute(
                epoch(),
                &id,
                Observation::Level(crate::Percent::new(36).unwrap()),
                1
            )
            .unwrap(),
            Attribution::Independent
        ));
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Level(level), 1)
                .unwrap(),
            Attribution::Inferred(_)
        ));
        // A repeated observation does not consume or refresh the constraint.
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Level(level), 9)
                .unwrap(),
            Attribution::Inferred(_)
        ));
        assert!(matches!(
            c.attribute(epoch(), &id, Observation::Level(level), 10)
                .unwrap(),
            Attribution::Independent
        ));
    }
    #[test]
    fn inferred_parent_preserves_cycle_rejection_without_changing_observation() {
        let mut chain = Chain::new(2, 0, 1000, Budget::default(), 32).unwrap();
        let parent = chain
            .plan(&chain.root(), "on", &[], &Default::default(), 0)
            .unwrap()
            .cause;
        let mut c = Candidates::new(epoch(), 10, 4).unwrap();
        c.record(
            epoch(),
            DeviceId::new("lamp").unwrap(),
            Command::Switch(true),
            Candidate {
                cause: parent,
                command_id: 1,
                written_ms: 1,
            },
        )
        .unwrap();
        let observed = Observation::Switch(true);
        let Attribution::Inferred(candidate) = c
            .attribute(epoch(), &DeviceId::new("lamp").unwrap(), observed, 2)
            .unwrap()
        else {
            panic!("expected inferred attribution")
        };
        assert!(matches!(
            chain.plan(&candidate.cause, "on", &[], &Default::default(), 2),
            Err(crate::rule_chain::Error::Cycle)
        ));
        assert_eq!(observed, Observation::Switch(true));
    }
}
