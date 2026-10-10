//! Per-command execution evidence. No retries, I/O or durable journal.
//! A single owner must serialize writes and observations for each endpoint.
use crate::{
    DeviceId, Observation,
    queue::{CancelReason, Epochs, QueuedCommand},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Queued,
    Writing,
    Written,
    Observed,
    TimedOut,
    Cancelled(CancelReason),
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionError {
    InvalidTransition,
    ClockWentBackwards,
    Expired,
    StaleEpoch,
}
impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ExecutionError {}

#[derive(Debug)]
pub struct Execution {
    request: QueuedCommand,
    outcome: Outcome,
    written_at: Option<u64>,
    last_time: u64,
}
impl Execution {
    pub fn new(request: QueuedCommand, now: u64) -> Result<Self, ExecutionError> {
        if now >= request.deadline_ms {
            return Err(ExecutionError::Expired);
        }
        Ok(Self {
            request,
            outcome: Outcome::Queued,
            written_at: None,
            last_time: now,
        })
    }
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub fn request(&self) -> &QueuedCommand {
        &self.request
    }
    fn time(&self, now: u64) -> Result<(), ExecutionError> {
        if now < self.last_time {
            Err(ExecutionError::ClockWentBackwards)
        } else {
            Ok(())
        }
    }
    pub fn begin_write(&mut self, epochs: Epochs, now: u64) -> Result<(), ExecutionError> {
        self.time(now)?;
        if self.outcome != Outcome::Queued {
            return Err(ExecutionError::InvalidTransition);
        }
        if self.request.epochs != epochs {
            return Err(ExecutionError::StaleEpoch);
        }
        if now >= self.request.deadline_ms {
            return Err(ExecutionError::Expired);
        }
        self.outcome = Outcome::Writing;
        self.last_time = now;
        Ok(())
    }
    /// Call only when the entire frame was accepted by the local transport.
    pub fn written(&mut self, now: u64) -> Result<(), ExecutionError> {
        self.time(now)?;
        if self.outcome != Outcome::Writing {
            return Err(ExecutionError::InvalidTransition);
        }
        self.outcome = if now >= self.request.deadline_ms {
            Outcome::Uncertain
        } else {
            Outcome::Written
        };
        self.written_at = Some(now);
        self.last_time = now;
        Ok(())
    }
    /// A failed write is uncertain unless the transport proves zero bytes sent.
    pub fn write_failed(
        &mut self,
        zero_bytes_proven: bool,
        now: u64,
    ) -> Result<(), ExecutionError> {
        self.time(now)?;
        if self.outcome != Outcome::Writing {
            return Err(ExecutionError::InvalidTransition);
        }
        self.outcome = if zero_bytes_proven {
            Outcome::Failed
        } else {
            Outcome::Uncertain
        };
        self.last_time = now;
        Ok(())
    }
    /// Feedback may confirm a target state, never causal ownership of that state.
    /// Query, LED and motor commands have no absolute target in this model.
    pub fn observe(
        &mut self,
        device: &DeviceId,
        observation: Observation,
        transport_epoch: u64,
        received_at: u64,
    ) -> Result<bool, ExecutionError> {
        self.time(received_at)?;
        if self.outcome != Outcome::Written
            || device != &self.request.device_id
            || transport_epoch != self.request.epochs.transport
            || received_at >= self.request.deadline_ms
            || self.written_at.is_none_or(|t| received_at < t)
            || !self.request.command.target_matches(observation)
        {
            return Ok(false);
        }
        self.outcome = Outcome::Observed;
        self.last_time = received_at;
        Ok(true)
    }
    /// Deadlines never imply that a physical action was undone.
    pub fn expire(&mut self, now: u64) -> Result<(), ExecutionError> {
        self.time(now)?;
        if now >= self.request.deadline_ms {
            self.outcome = match self.outcome {
                Outcome::Queued | Outcome::Written => Outcome::TimedOut,
                Outcome::Writing => Outcome::Uncertain,
                terminal => terminal,
            };
        }
        self.last_time = now;
        Ok(())
    }
    /// Invalidation cancels unsent work; started work cannot be called cancelled.
    pub fn invalidate(&mut self, reason: CancelReason, now: u64) -> Result<(), ExecutionError> {
        self.time(now)?;
        self.outcome = match self.outcome {
            Outcome::Queued => Outcome::Cancelled(reason),
            Outcome::Writing | Outcome::Written => Outcome::Uncertain,
            terminal => terminal,
        };
        self.last_time = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, queue::Origin};
    fn execution() -> Execution {
        Execution::new(
            QueuedCommand {
                id: 1,
                device_id: DeviceId::new("light").unwrap(),
                command: Command::Switch(true),
                origin: Origin::Hap,
                epochs: Epochs {
                    transport: 1,
                    configuration: 1,
                    control: 1,
                },
                deadline_ms: 100,
                run_id: None,
            },
            0,
        )
        .unwrap()
    }
    #[test]
    fn feedback_requires_completed_write_correct_endpoint_epoch_value_and_deadline() {
        let mut e = execution();
        let id = e.request.device_id.clone();
        assert!(!e.observe(&id, Observation::Switch(true), 1, 1).unwrap());
        e.begin_write(e.request.epochs, 2).unwrap();
        e.written(3).unwrap();
        assert!(!e.observe(&id, Observation::Switch(false), 1, 4).unwrap());
        assert!(!e.observe(&id, Observation::Switch(true), 2, 4).unwrap());
        assert!(
            !e.observe(
                &DeviceId::new("other").unwrap(),
                Observation::Switch(true),
                1,
                4
            )
            .unwrap()
        );
        assert!(!e.observe(&id, Observation::Switch(true), 1, 100).unwrap());
        assert!(e.observe(&id, Observation::Switch(true), 1, 5).unwrap());
        e.invalidate(CancelReason::TransportChanged, 6).unwrap();
        assert_eq!(e.outcome(), Outcome::Observed);
    }
    #[test]
    fn disconnect_never_replays_and_only_unsent_work_is_cancelled() {
        for stage in 0..3 {
            let mut e = execution();
            if stage > 0 {
                e.begin_write(e.request.epochs, 1).unwrap();
            }
            if stage > 1 {
                e.written(2).unwrap();
            }
            e.invalidate(CancelReason::TransportChanged, 3).unwrap();
            assert_eq!(
                e.outcome(),
                if stage == 0 {
                    Outcome::Cancelled(CancelReason::TransportChanged)
                } else {
                    Outcome::Uncertain
                }
            );
            assert_eq!(
                e.begin_write(e.request.epochs, 4),
                Err(ExecutionError::InvalidTransition)
            );
        }
    }
    #[test]
    fn partial_write_and_deadline_preserve_uncertainty() {
        for zero in [true, false] {
            let mut e = execution();
            e.begin_write(e.request.epochs, 1).unwrap();
            e.write_failed(zero, 2).unwrap();
            assert_eq!(
                e.outcome(),
                if zero {
                    Outcome::Failed
                } else {
                    Outcome::Uncertain
                }
            );
        }
        let mut e = execution();
        e.begin_write(e.request.epochs, 1).unwrap();
        e.expire(100).unwrap();
        assert_eq!(e.outcome(), Outcome::Uncertain);
        let mut e = execution();
        e.begin_write(e.request.epochs, 1).unwrap();
        e.written(2).unwrap();
        e.expire(100).unwrap();
        assert_eq!(e.outcome(), Outcome::TimedOut);
        assert!(
            !e.observe(
                &e.request.device_id.clone(),
                Observation::Switch(true),
                1,
                101
            )
            .unwrap()
        );
    }
}
