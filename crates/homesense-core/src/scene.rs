//! Volatile scene timeline. Delays are explicit milliseconds, not imported C4
//! rate units. A dispatcher must still authorize and durably admit each action.
use crate::{Command, DeviceId, queue::Epochs};
#[derive(Debug, Clone)]
pub struct Step {
    pub device: DeviceId,
    pub command: Command,
    pub delay_ms: u64,
    pub source: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Waiting,
    Offered,
    Written,
    Partial,
    Failed,
    Cancelled,
    Expired,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    CommandsWritten,
    Failed,
    Partial,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPlan,
    Clock,
    WrongGeneration,
    InFlight,
    InvalidResult,
}
#[derive(Debug, Clone)]
pub struct Due {
    pub index: usize,
    pub step: Step,
}
pub struct Timeline {
    epochs: Epochs,
    started: u64,
    now: u64,
    lateness: u64,
    steps: Vec<Step>,
    order: Vec<usize>,
    states: Vec<State>,
    offered: Option<usize>,
}
impl Timeline {
    pub fn new(
        steps: Vec<Step>,
        epochs: Epochs,
        started: u64,
        max_lateness_ms: u64,
    ) -> Result<Self, Error> {
        if steps.is_empty()
            || steps.len() > 4096
            || max_lateness_ms == 0
            || max_lateness_ms > 60_000
            || steps.iter().any(|s| {
                s.source.is_empty()
                    || s.delay_ms > 3_600_000
                    || started
                        .checked_add(s.delay_ms)
                        .and_then(|v| v.checked_add(max_lateness_ms))
                        .is_none()
            })
        {
            return Err(Error::InvalidPlan);
        }
        let mut order: Vec<_> = (0..steps.len()).collect();
        order.sort_by_key(|&i| (steps[i].delay_ms, i));
        let states = vec![State::Waiting; steps.len()];
        Ok(Self {
            epochs,
            started,
            now: started,
            lateness: max_lateness_ms,
            steps,
            order,
            states,
            offered: None,
        })
    }
    /// At most one offered action. The caller must resolve it even on failure;
    /// never replay an ambiguous network write as a new offer.
    pub fn next(&mut self, now: u64, epochs: Epochs) -> Result<Option<Due>, Error> {
        if now < self.now {
            return Err(Error::Clock);
        }
        self.now = now;
        if epochs != self.epochs {
            self.cancel();
            return Err(Error::WrongGeneration);
        }
        if self.offered.is_some() {
            return Err(Error::InFlight);
        }
        for &i in &self.order {
            if self.states[i] != State::Waiting {
                continue;
            }
            let due = self.started + self.steps[i].delay_ms;
            if now < due {
                return Ok(None);
            }
            if now > due + self.lateness {
                self.states[i] = State::Expired;
                continue;
            }
            self.states[i] = State::Offered;
            self.offered = Some(i);
            return Ok(Some(Due {
                index: i,
                step: self.steps[i].clone(),
            }));
        }
        Ok(None)
    }
    /// Written is transport completion only. Scene active requires feedback.
    pub fn resolve(&mut self, index: usize, written: bool) -> Result<(), Error> {
        if self.offered != Some(index) {
            return Err(Error::InvalidResult);
        }
        self.offered = None;
        self.states[index] = if written {
            State::Written
        } else {
            State::Failed
        };
        Ok(())
    }
    /// Some expanded output was written, but the complete action did not finish.
    pub fn resolve_partial(&mut self, index: usize) -> Result<(), Error> {
        if self.offered != Some(index) {
            return Err(Error::InvalidResult);
        }
        self.offered = None;
        self.states[index] = State::Partial;
        Ok(())
    }
    /// Only after the runtime proves all offered commands were durably cancelled.
    pub fn cancel_offered(&mut self, index: usize) -> Result<(), Error> {
        if self.offered != Some(index) {
            return Err(Error::InvalidResult);
        }
        self.offered = None;
        self.states[index] = State::Cancelled;
        Ok(())
    }
    /// Manual override cancels only this device's unoffered future actions.
    pub fn cancel_device(&mut self, device: &DeviceId) -> usize {
        let mut n = 0;
        for (step, state) in self.steps.iter().zip(&mut self.states) {
            if &step.device == device && *state == State::Waiting {
                *state = State::Cancelled;
                n += 1;
            }
        }
        n
    }
    /// Cancels timers. Offered/admitted output needs runtime queue cancellation.
    pub fn cancel(&mut self) -> usize {
        let mut n = 0;
        for s in &mut self.states {
            if *s == State::Waiting {
                *s = State::Cancelled;
                n += 1;
            }
        }
        n
    }
    pub fn due_step(&self, now: u64) -> Option<(usize, &Step)> {
        self.order.iter().copied().find_map(|i| {
            let due=self.started+self.steps[i].delay_ms;
            (self.states[i]==State::Waiting && now>=due && now<=due+self.lateness)
                .then_some((i,&self.steps[i]))
        })
    }
    pub fn step(&self, index: usize) -> Option<&Step> { self.steps.get(index) }
    pub fn states(&self) -> &[State] {
        &self.states
    }
    pub fn status(&self) -> Status {
        let written =
            self.states.contains(&State::Written) || self.states.contains(&State::Partial);
        if self.states.contains(&State::Waiting) || self.states.contains(&State::Offered) {
            if self.states.iter().all(|s| *s == State::Waiting) {
                Status::Pending
            } else {
                Status::Running
            }
        } else if self.states.iter().all(|s| *s == State::Written) {
            Status::CommandsWritten
        } else if written {
            Status::Partial
        } else if self
            .states
            .iter()
            .any(|s| matches!(s, State::Failed | State::Expired))
        {
            Status::Failed
        } else {
            Status::Cancelled
        }
    }
}
