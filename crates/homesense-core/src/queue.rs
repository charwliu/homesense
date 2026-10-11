//! Deterministic bounded scheduling after authorization and protocol validation.
//! This queue performs no I/O and is not a substitute for admission/authentication.
use crate::{Command, DeviceId, LevelTransition, MotorMotion};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Epochs {
    pub transport: u64,
    pub configuration: u64,
    pub control: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Http,
    Hap,
    Panel,
    Rule,
    Background,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedCommand {
    pub id: u64,
    pub device_id: DeviceId,
    pub command: Command,
    pub origin: Origin,
    pub epochs: Epochs,
    pub deadline_ms: u64,
    pub run_id: Option<u64>,
}
impl QueuedCommand {
    pub fn is_stop(&self) -> bool {
        self.command == Command::Motor(MotorMotion::Stop)
    }
    fn priority(&self) -> u8 {
        if self.is_stop() {
            0
        } else if self.command == Command::Query {
            4
        } else {
            match self.origin {
                Origin::Http | Origin::Hap => 1,
                Origin::Panel => 2,
                Origin::Rule => 3,
                Origin::Background => 4,
            }
        }
    }
    fn is_immediate_level(&self) -> bool {
        matches!(
            self.command,
            Command::SetLevel {
                transition: LevelTransition::Immediate,
                ..
            }
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    Expired,
    TransportChanged,
    ConfigurationChanged,
    ControlChanged,
    Superseded,
    Stopped,
    RunCancelled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancellation {
    pub command: QueuedCommand,
    pub reason: CancelReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    InvalidCapacity,
    QueueFull,
    InvalidCommandId,
    Expired,
    StaleEpoch,
}
impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for QueueError {}
#[derive(Debug, Default)]
pub struct Poll {
    pub next: Option<QueuedCommand>,
    pub cancelled: Vec<Cancellation>,
}

#[derive(Clone)]
pub struct CommandQueue {
    commands: VecDeque<QueuedCommand>,
    normal_capacity: usize,
    stop_capacity: usize,
    last_id: u64,
}
impl CommandQueue {
    pub fn new(normal_capacity: usize, stop_capacity: usize) -> Result<Self, QueueError> {
        if normal_capacity == 0
            || stop_capacity == 0
            || normal_capacity.checked_add(stop_capacity).is_none()
        {
            return Err(QueueError::InvalidCapacity);
        }
        Ok(Self {
            commands: VecDeque::new(),
            normal_capacity,
            stop_capacity,
            last_id: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.commands.len()
    }
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
    pub fn has_stop(&self) -> bool {
        self.commands.iter().any(QueuedCommand::is_stop)
    }
    /// IDs increase within one boot. Rejected IDs may be retried; accepted IDs cannot.
    /// Rejections leave every queued command unchanged.
    pub fn enqueue(
        &mut self,
        request: QueuedCommand,
        now_ms: u64,
        current: Epochs,
    ) -> Result<Vec<Cancellation>, QueueError> {
        if request.id <= self.last_id {
            return Err(QueueError::InvalidCommandId);
        }
        if request.deadline_ms <= now_ms {
            return Err(QueueError::Expired);
        }
        if request.epochs != current {
            return Err(QueueError::StaleEpoch);
        }
        let stop = request.is_stop();
        // Coalesce only adjacent actions. Crossing another device can remove a
        // prerequisite, e.g. dimmer preparation before a supply relay activation.
        let replace = self
            .commands
            .iter()
            .rposition(|old| old.device_id == request.device_id)
            .filter(|&i| {
                let old = &self.commands[i];
                i + 1 == self.commands.len()
                    && request.is_immediate_level()
                    && old.is_immediate_level()
                    && old.origin == request.origin
                    && old.run_id == request.run_id
                    && old.epochs == current
            });
        let supersedes_rule = |old: &QueuedCommand| {
            old.device_id == request.device_id
                && old.origin == Origin::Rule
                && old.run_id.is_some()
                && !old.is_stop()
                && !request.is_stop()
                && (matches!(request.origin, Origin::Http | Origin::Hap | Origin::Panel)
                    || (request.origin == Origin::Rule && request.run_id != old.run_id))
        };
        let replaced_rules = self
            .commands
            .iter()
            .filter(|c| c.is_stop() == stop && supersedes_rule(c))
            .count();
        let used = self.commands.iter().filter(|c| c.is_stop() == stop).count();
        let capacity = if stop {
            self.stop_capacity
        } else {
            self.normal_capacity
        };
        if used - usize::from(replace.is_some()) - replaced_rules >= capacity {
            return Err(QueueError::QueueFull);
        }
        let mut cancelled = Vec::new();
        if let Some(i) = replace {
            cancelled.push(Cancellation {
                command: self.commands.remove(i).unwrap(),
                reason: CancelReason::Superseded,
            });
        }
        self.commands.retain(|old| {
            if supersedes_rule(old) {
                cancelled.push(Cancellation {
                    command: old.clone(),
                    reason: CancelReason::Superseded,
                });
                false
            } else {
                true
            }
        });
        if stop {
            // A queued UP must not execute after a later STOP and restart the motor.
            self.commands.retain(|old| {
                let remove = old.device_id == request.device_id
                    && matches!(
                        old.command,
                        Command::Motor(MotorMotion::Up | MotorMotion::Down)
                    );
                if remove {
                    cancelled.push(Cancellation {
                        command: old.clone(),
                        reason: CancelReason::Stopped,
                    });
                }
                !remove
            });
        }
        self.last_id = request.id;
        self.commands.push_back(request);
        Ok(cancelled)
    }
    /// Stale/expired commands are returned to the caller for outcome reporting.
    pub fn poll(&mut self, now_ms: u64, current: Epochs) -> Poll {
        let mut cancelled = Vec::new();
        self.commands.retain(|command| {
            let reason = if command.epochs.transport != current.transport {
                Some(CancelReason::TransportChanged)
            } else if command.epochs.configuration != current.configuration {
                Some(CancelReason::ConfigurationChanged)
            } else if command.epochs.control != current.control {
                Some(CancelReason::ControlChanged)
            } else if command.deadline_ms <= now_ms {
                Some(CancelReason::Expired)
            } else {
                None
            };
            if let Some(reason) = reason {
                cancelled.push(Cancellation {
                    command: command.clone(),
                    reason,
                });
            }
            reason.is_none()
        });
        let index = self
            .commands
            .iter()
            .enumerate()
            .filter(|(i, command)| {
                command.is_stop()
                    || !self
                        .commands
                        .iter()
                        .take(*i)
                        .any(|old| old.device_id == command.device_id)
            })
            .min_by_key(|(i, command)| (command.priority(), *i))
            .map(|(i, _)| i);
        Poll {
            next: index.and_then(|i| self.commands.remove(i)),
            cancelled,
        }
    }
    pub fn cancel_run(&mut self, run_id: u64) -> Vec<Cancellation> {
        let mut cancelled = Vec::new();
        self.commands.retain(|command| {
            // STOP remains available when a scene is cancelled.
            let remove = command.run_id == Some(run_id) && !command.is_stop();
            if remove {
                cancelled.push(Cancellation {
                    command: command.clone(),
                    reason: CancelReason::RunCancelled,
                });
            }
            !remove
        });
        cancelled
    }
    /// Used after disconnection/control loss. No command, including STOP, is replayed.
    pub fn clear(&mut self, reason: CancelReason) -> Vec<Cancellation> {
        self.commands
            .drain(..)
            .map(|command| Cancellation { command, reason })
            .collect()
    }
    /// Publication takes STOP commands out for explicit revalidation/draining.
    /// They are not silently stamped with a new configuration or control epoch.
    pub fn publication_barrier(&mut self) -> (Vec<Cancellation>, Vec<QueuedCommand>) {
        let mut cancelled = Vec::new();
        let mut stops = Vec::new();
        for command in self.commands.drain(..) {
            if command.is_stop() {
                stops.push(command);
            } else {
                cancelled.push(Cancellation {
                    command,
                    reason: CancelReason::ConfigurationChanged,
                });
            }
        }
        (cancelled, stops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Percent;
    const EPOCH: Epochs = Epochs {
        transport: 1,
        configuration: 1,
        control: 1,
    };
    fn req(id: u64, device: &str, command: Command, origin: Origin) -> QueuedCommand {
        QueuedCommand {
            id,
            device_id: DeviceId::new(device).unwrap(),
            command,
            origin,
            epochs: EPOCH,
            deadline_ms: 100,
            run_id: None,
        }
    }
    fn level(n: u8) -> Command {
        Command::SetLevel {
            level: Percent::new(n).unwrap(),
            transition: LevelTransition::Immediate,
        }
    }
    #[test]
    fn stop_has_reserved_capacity_and_prevents_queued_motor_restart() {
        let mut q = CommandQueue::new(2, 1).unwrap();
        q.enqueue(
            req(1, "motor", Command::Motor(MotorMotion::Up), Origin::Rule),
            0,
            EPOCH,
        )
        .unwrap();
        q.enqueue(
            req(2, "light", Command::Switch(true), Origin::Hap),
            0,
            EPOCH,
        )
        .unwrap();
        assert_eq!(
            q.enqueue(
                req(3, "other", Command::Switch(true), Origin::Hap),
                0,
                EPOCH
            ),
            Err(QueueError::QueueFull)
        );
        let cancelled = q
            .enqueue(
                req(3, "motor", Command::Motor(MotorMotion::Stop), Origin::Hap),
                0,
                EPOCH,
            )
            .unwrap();
        assert_eq!(cancelled[0].command.id, 1);
        assert_eq!(cancelled[0].reason, CancelReason::Stopped);
        assert_eq!(q.poll(1, EPOCH).next.unwrap().id, 3);
        assert_eq!(q.poll(1, EPOCH).next.unwrap().id, 2);
        assert!(q.poll(1, EPOCH).next.is_none());
    }
    #[test]
    fn priority_is_fifo_within_class_and_respects_device_order() {
        let mut q = CommandQueue::new(5, 1).unwrap();
        q.enqueue(req(1, "a", Command::Switch(true), Origin::Rule), 0, EPOCH)
            .unwrap();
        q.enqueue(req(2, "a", Command::Switch(false), Origin::Hap), 0, EPOCH)
            .unwrap();
        q.enqueue(req(3, "b", Command::Switch(true), Origin::Hap), 0, EPOCH)
            .unwrap();
        q.enqueue(req(4, "c", Command::Query, Origin::Hap), 0, EPOCH)
            .unwrap();
        assert_eq!(
            (0..4)
                .map(|_| q.poll(1, EPOCH).next.unwrap().id)
                .collect::<Vec<_>>(),
            [3, 1, 2, 4]
        );
    }
    #[test]
    fn coalescing_is_bounded_and_does_not_cross_query_or_run_barriers() {
        let mut q = CommandQueue::new(2, 1).unwrap();
        q.enqueue(req(1, "light", level(10), Origin::Hap), 0, EPOCH)
            .unwrap();
        let cancelled = q
            .enqueue(req(2, "light", level(20), Origin::Hap), 0, EPOCH)
            .unwrap();
        assert_eq!(cancelled[0].reason, CancelReason::Superseded);
        assert_eq!(q.len(), 1);
        q.enqueue(
            req(3, "light", Command::Query, Origin::Background),
            0,
            EPOCH,
        )
        .unwrap();
        assert_eq!(
            q.enqueue(req(4, "light", level(30), Origin::Hap), 0, EPOCH),
            Err(QueueError::QueueFull)
        );
        assert_eq!(q.poll(1, EPOCH).next.unwrap().command, level(20));
    }
    #[test]
    fn stale_epochs_and_expiration_are_cancelled_without_dispatch() {
        for current in [
            Epochs {
                transport: 2,
                ..EPOCH
            },
            Epochs {
                configuration: 2,
                ..EPOCH
            },
            Epochs {
                control: 2,
                ..EPOCH
            },
        ] {
            let mut q = CommandQueue::new(2, 1).unwrap();
            q.enqueue(req(1, "light", level(10), Origin::Hap), 0, EPOCH)
                .unwrap();
            let result = q.poll(1, current);
            assert!(result.next.is_none());
            assert_eq!(result.cancelled.len(), 1);
        }
        let mut q = CommandQueue::new(1, 1).unwrap();
        q.enqueue(req(1, "light", level(10), Origin::Hap), 0, EPOCH)
            .unwrap();
        let result = q.poll(100, EPOCH);
        assert!(result.next.is_none());
        assert_eq!(result.cancelled[0].reason, CancelReason::Expired);
    }
    #[test]
    fn publication_retains_stop_for_explicit_drain_and_cancel_run_keeps_other_runs() {
        let mut q = CommandQueue::new(3, 1).unwrap();
        let mut a = req(1, "light", level(10), Origin::Rule);
        a.run_id = Some(7);
        q.enqueue(a, 0, EPOCH).unwrap();
        let mut b = req(2, "other-light", level(20), Origin::Rule);
        b.run_id = Some(8);
        assert!(q.enqueue(b, 0, EPOCH).unwrap().is_empty());
        q.enqueue(
            req(3, "motor", Command::Motor(MotorMotion::Stop), Origin::Hap),
            0,
            EPOCH,
        )
        .unwrap();
        assert_eq!(q.cancel_run(7)[0].command.id, 1);
        let (cancelled, stops) = q.publication_barrier();
        assert_eq!(cancelled[0].command.id, 2);
        assert_eq!(stops[0].id, 3);
        assert_eq!(stops[0].epochs, EPOCH);
        assert!(q.is_empty());
    }
}
