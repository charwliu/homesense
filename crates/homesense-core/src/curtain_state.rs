//! Opt-in command/time inference, never motor feedback or a replay plan.
use crate::{MotorAction, queue::Epochs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub boot: u128,
    pub epochs: Epochs,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Opening,
    Closing,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Open,
    Closed,
    Intermediate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Observed,
    Inferred,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fact<T> {
    pub value: T,
    pub source: Provenance,
    pub at_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    pub open_travel_ms: Option<u64>,
    pub close_travel_ms: Option<u64>,
    pub stop_led_idle_ms: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidTiming,
    InvalidScope,
    ClockWentBackwards,
    InvalidReceipt,
    StaleReceipt,
    InvalidObservation,
    DeadlineOverflow,
    CounterExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
/// Logical color intent, not physical LED feedback or button permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndicatorIntent {
    pub open_red: bool,
    pub close_red: bool,
    pub stop_red: Option<bool>,
}
impl Snapshot {
    pub fn indicators(&self) -> IndicatorIntent {
        let (open_red, close_red) = if matches!(
            self.inferred_movement.map(|f| f.value),
            Some(Movement::Opening | Movement::Closing)
        ) {
            (false, false)
        } else {
            match self.inferred_position.map(|f| f.value) {
                Some(Position::Closed) => (true, false),
                Some(Position::Open) => (false, true),
                _ => (true, true),
            }
        };
        IndicatorIntent {
            open_red,
            close_red,
            stop_red: self.stop_led_active.map(|f| f.value),
        }
    }
}
impl Timing {
    pub fn validate(self) -> Result<Self, Error> {
        if [
            self.open_travel_ms,
            self.close_travel_ms,
            self.stop_led_idle_ms,
        ]
        .into_iter()
        .flatten()
        .any(|ms| !(1..=86_400_000).contains(&ms))
        {
            return Err(Error::InvalidTiming);
        }
        Ok(self)
    }
}
/// Estimated hundredths of a percent, never a module actual position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionPercent(u16);
impl PositionPercent {
    pub fn hundredths(self) -> u16 {
        self.0
    }
    pub fn percent(self) -> f64 {
        f64::from(self.0) / 100.0
    }
    fn category(self) -> Position {
        match self.0 {
            0 => Position::Closed,
            10000 => Position::Open,
            _ => Position::Intermediate,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopEvidence {
    #[default]
    ActualOnly,
    ReviewedLocalEstimate,
}
/// Opaque identity of one current accepted command; old completions cannot act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    scope: Scope,
    command_id: u64,
    serial: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRecord {
    pub action: MotorAction,
    pub id: u64,
    pub written_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub scope: Scope,
    pub captured_ms: u64,
    /// Unknown availability is represented by absent facts, not a fourth action.
    pub observed_movement: Option<Fact<Movement>>,
    pub observed_position: Option<Fact<Position>>,
    pub inferred_movement: Option<Fact<Movement>>,
    pub inferred_position: Option<Fact<Position>>,
    pub inferred_percent: Option<Fact<PositionPercent>>,
    pub last_written: Option<CommandRecord>,
    pub pending: Option<MotorAction>,
    /// Inferred LED activity only. All valid manual STOPs remain actionable.
    pub stop_led_active: Option<Fact<bool>>,
}
#[derive(Debug, Clone, Copy)]
struct Pending {
    ticket: Ticket,
    action: MotorAction,
    prior_curve: Option<Travel>,
    prior_position: Option<Fact<Position>>,
    prior_percent: Option<PositionPercent>,
}
#[derive(Debug, Clone, Copy)]
struct Travel {
    ticket: Ticket,
    deadline: Option<u64>,
    started: u64,
    duration: Option<u64>,
    action: MotorAction,
    start_percent: Option<PositionPercent>,
}
impl Travel {
    fn percent_at(self, at: u64) -> Option<PositionPercent> {
        let elapsed = at.checked_sub(self.started)?;
        let duration = self.duration?;
        if elapsed >= duration {
            return Some(PositionPercent(if self.action == MotorAction::Open {
                10000
            } else {
                0
            }));
        }
        let start = u128::from(self.start_percent?.0);
        let delta = u128::from(elapsed) * 10000 / u128::from(duration);
        let value = if self.action == MotorAction::Open {
            start.saturating_add(delta).min(10000)
        } else {
            start.saturating_sub(delta)
        };
        Some(PositionPercent(value as u16))
    }
}
pub struct Tracker {
    timing: Timing,
    scope: Scope,
    now: u64,
    last_id: u64,
    serial: u64,
    write_barrier: Option<u64>,
    pending: Option<Pending>,
    travel: Option<Travel>,
    observed_movement: Option<Fact<Movement>>,
    observed_position: Option<Fact<Position>>,
    inferred_movement: Option<Fact<Movement>>,
    inferred_position: Option<Fact<Position>>,
    inferred_percent: Option<Fact<PositionPercent>>,
    last_written: Option<CommandRecord>,
}
impl Tracker {
    pub fn new(timing: Timing, scope: Scope, now: u64) -> Result<Self, Error> {
        let timing = timing.validate()?;
        Self::valid_scope(scope)?;
        Ok(Self {
            timing,
            scope,
            now,
            last_id: 0,
            serial: 0,
            write_barrier: None,
            pending: None,
            travel: None,
            observed_movement: None,
            observed_position: None,
            inferred_movement: None,
            inferred_position: None,
            inferred_percent: None,
            last_written: None,
        })
    }
    fn valid_scope(scope: Scope) -> Result<(), Error> {
        if scope.boot == 0
            || [
                scope.epochs.transport,
                scope.epochs.configuration,
                scope.epochs.control,
            ]
            .contains(&0)
        {
            Err(Error::InvalidScope)
        } else {
            Ok(())
        }
    }
    fn clock(&mut self, now: u64) -> Result<(), Error> {
        if now < self.now {
            self.invalidate();
            return Err(Error::ClockWentBackwards);
        }
        self.now = now;
        Ok(())
    }
    /// Drop local inference/observations; no STOP, completion or replay is made.
    /// Monotonic command identity remains fenced within the same boot/scope.
    pub fn invalidate(&mut self) {
        self.pending = None;
        self.travel = None;
        self.observed_movement = None;
        self.observed_position = None;
        self.inferred_movement = None;
        self.inferred_position = None;
        self.inferred_percent = None;
        self.last_written = None;
        self.write_barrier = None;
    }
    pub fn advance(&mut self, scope: Scope, now: u64) -> Result<Snapshot, Error> {
        Self::valid_scope(scope)?;
        if scope != self.scope {
            self.invalidate();
            self.scope = scope;
            self.last_id = 0;
            self.now = now;
        } else {
            self.clock(now)?;
        }
        if let Some(travel) = self.travel
            && travel.ticket.scope == self.scope
            && self.pending.is_none()
        {
            self.inferred_percent = travel.percent_at(now).map(|value| Fact {
                value,
                source: Provenance::Inferred,
                at_ms: now,
            });
            if travel.deadline.is_some_and(|deadline| now >= deadline) {
                let at_ms = travel.deadline.unwrap();
                let value = if travel.action == MotorAction::Open {
                    Position::Open
                } else {
                    Position::Closed
                };
                self.inferred_position = Some(Fact {
                    value,
                    source: Provenance::Inferred,
                    at_ms,
                });
                self.inferred_movement = Some(Fact {
                    value: Movement::Stopped,
                    source: Provenance::Inferred,
                    at_ms,
                });
            }
        }
        if let Some(prior) = self.pending.and_then(|p| p.prior_curve) {
            self.inferred_percent = prior.percent_at(now).map(|value| Fact {
                value,
                source: Provenance::Inferred,
                at_ms: now,
            });
        }
        Ok(self.snapshot())
    }
    /// Only attach a newly accepted, scoped owner receipt. Acceptance itself is
    /// not movement or completion; a new direction/STOP cancels the old timer.
    pub fn accepted(
        &mut self,
        scope: Scope,
        id: u64,
        action: MotorAction,
        now: u64,
    ) -> Result<Ticket, Error> {
        if scope != self.scope {
            return Err(Error::InvalidScope);
        }
        if id == 0 || id <= self.last_id {
            return Err(Error::InvalidReceipt);
        }
        self.advance(scope, now)?;
        let prior_curve = self.travel;
        let prior_position = self.inferred_position;
        let prior_percent = self.inferred_percent.map(|f| f.value).or_else(|| {
            self.observed_position.and_then(|f| match f.value {
                Position::Open => Some(PositionPercent(10000)),
                Position::Closed => Some(PositionPercent(0)),
                Position::Intermediate => None,
            })
        });
        self.serial = self.serial.checked_add(1).ok_or(Error::CounterExhausted)?;
        let ticket = Ticket {
            scope,
            command_id: id,
            serial: self.serial,
        };
        self.last_id = id;
        self.pending = Some(Pending {
            ticket,
            action,
            prior_curve,
            prior_position,
            prior_percent,
        });
        self.travel = None;
        // A queued action is not a new physical/command write boundary. Keep
        // the previous healthy motion and activity until the new Written.
        Ok(ticket)
    }
    /// Compatibility convenience for a receipt verified at its write instant.
    pub fn written(&mut self, ticket: Ticket, now: u64) -> Result<(), Error> {
        self.written_at(ticket, now, now)
    }
    /// The exact persisted Written instant starts time inference. Verification
    /// may be later than acceptance or even the full travel interval.
    pub fn written_at(&mut self, ticket: Ticket, written_ms: u64, now: u64) -> Result<(), Error> {
        let pending = self
            .pending
            .filter(|p| p.ticket == ticket)
            .ok_or(Error::StaleReceipt)?;
        if ticket.scope != self.scope
            || written_ms > now
            || self.write_barrier.is_some_and(|at| written_ms < at)
        {
            return Err(Error::InvalidReceipt);
        }
        self.clock(now)?;
        let start_percent = pending
            .prior_curve
            .and_then(|t| t.percent_at(written_ms))
            .or_else(|| {
                pending
                    .prior_curve
                    .is_none()
                    .then_some(pending.prior_percent)
                    .flatten()
            });
        self.pending = None;
        self.write_barrier = Some(written_ms);
        self.last_written = Some(CommandRecord {
            action: pending.action,
            id: ticket.command_id,
            written_ms,
        });
        if pending.action == MotorAction::Stop {
            self.inferred_movement = Some(Fact {
                value: Movement::Stopped,
                source: Provenance::Inferred,
                at_ms: written_ms,
            });
            self.inferred_percent = start_percent.map(|value| Fact {
                value,
                source: Provenance::Inferred,
                at_ms: written_ms,
            });
            let position = if let Some(percent) = start_percent {
                Some(percent.category())
            } else if let Some(travel) = pending.prior_curve {
                Some(if travel.deadline.is_some_and(|end| written_ms >= end) {
                    if travel.action == MotorAction::Open {
                        Position::Open
                    } else {
                        Position::Closed
                    }
                } else {
                    Position::Intermediate
                })
            } else {
                pending.prior_position.map(|p| p.value)
            };
            self.inferred_position = position.map(|value| Fact {
                value,
                source: Provenance::Inferred,
                at_ms: written_ms,
            });
            self.travel = None;
        } else {
            let duration = if pending.action == MotorAction::Open {
                self.timing.open_travel_ms
            } else {
                self.timing.close_travel_ms
            };
            let deadline = duration
                .map(|ms| written_ms.checked_add(ms).ok_or(Error::DeadlineOverflow))
                .transpose();
            let deadline = match deadline {
                Ok(v) => v,
                Err(e) => {
                    self.invalidate();
                    return Err(e);
                }
            };
            self.travel = Some(Travel {
                ticket,
                deadline,
                started: written_ms,
                duration,
                action: pending.action,
                start_percent,
            });
            self.inferred_movement = Some(Fact {
                value: if pending.action == MotorAction::Open {
                    Movement::Opening
                } else {
                    Movement::Closing
                },
                source: Provenance::Inferred,
                at_ms: written_ms,
            });
            self.inferred_position = None;
            self.inferred_percent = None;
        }
        self.advance(self.scope, now)?;
        Ok(())
    }
    /// Reserved for independently trustworthy motion/position feedback. The
    /// current 485 protocol has no such observation; parameter queries and
    /// command receipts must never call this method.
    pub fn observe(
        &mut self,
        scope: Scope,
        movement: Option<Movement>,
        position: Option<Position>,
        observed_ms: u64,
        now: u64,
    ) -> Result<(), Error> {
        if scope != self.scope || observed_ms > now || (movement.is_none() && position.is_none()) {
            return Err(Error::InvalidObservation);
        }
        if self
            .observed_movement
            .is_some_and(|f| f.at_ms > observed_ms)
            || self
                .observed_position
                .is_some_and(|f| f.at_ms > observed_ms)
        {
            return Err(Error::InvalidObservation);
        }
        self.clock(now)?;
        if let Some(value) = movement {
            self.observed_movement = Some(Fact {
                value,
                source: Provenance::Observed,
                at_ms: observed_ms,
            });
        }
        if let Some(value) = position {
            self.observed_position = Some(Fact {
                value,
                source: Provenance::Observed,
                at_ms: observed_ms,
            });
        }
        // An independent intervention supersedes time inference, not vice versa.
        self.travel = None;
        self.inferred_movement = None;
        self.inferred_position = None;
        self.inferred_percent = None;
        Ok(())
    }
    /// Default actual-only automatic-condition predicate. Independent previous
    /// command knowledge is required; local command memory is not the original
    /// variable. Reviewed estimate exceptions use stopped_open_gate instead.
    /// No manual command may be blocked by either predicate.
    pub fn explicit_stopped_open_gate(
        &self,
        previous_was_open: Option<bool>,
        now: u64,
        max_age: u64,
    ) -> bool {
        now >= self.now
            && self.pending.is_none()
            && previous_was_open == Some(false)
            && max_age > 0
            && self.observed_movement.is_some_and(|f| {
                f.value == Movement::Stopped
                    && self.write_barrier.is_none_or(|at| f.at_ms > at)
                    && now.checked_sub(f.at_ms).is_some_and(|age| age < max_age)
            })
    }
    /// Automatic-condition review only. Approval is explicit and local to a
    /// bound curtain; manual controls must never call this gate.
    pub fn stopped_open_gate(
        &self,
        previous_was_open: Option<bool>,
        now: u64,
        max_age: u64,
        policy: StopEvidence,
    ) -> Option<Provenance> {
        if self.explicit_stopped_open_gate(previous_was_open, now, max_age) {
            return Some(Provenance::Observed);
        }
        if policy == StopEvidence::ReviewedLocalEstimate
            && previous_was_open == Some(false)
            && self.pending.is_none()
            && self.last_written.is_some()
            && max_age > 0
            && now.checked_sub(self.now).is_some_and(|age| age < max_age)
            && self
                .inferred_movement
                .is_some_and(|f| f.value == Movement::Stopped)
        {
            return Some(Provenance::Inferred);
        }
        None
    }
    pub fn snapshot(&self) -> Snapshot {
        let stop_led_active = match self.inferred_movement {
            Some(Fact {
                value: Movement::Opening | Movement::Closing,
                ..
            }) => Some(Fact {
                value: true,
                source: Provenance::Inferred,
                at_ms: self.now,
            }),
            Some(Fact {
                value: Movement::Stopped,
                at_ms,
                ..
            }) => self.timing.stop_led_idle_ms.map(|ms| Fact {
                value: self.now.saturating_sub(at_ms) < ms,
                source: Provenance::Inferred,
                at_ms: self.now,
            }),
            None => None,
        };
        Snapshot {
            scope: self.scope,
            captured_ms: self.now,
            observed_movement: self.observed_movement,
            observed_position: self.observed_position,
            inferred_movement: self.inferred_movement,
            inferred_position: self.inferred_position,
            inferred_percent: self.inferred_percent,
            last_written: self.last_written,
            pending: self.pending.map(|p| p.action),
            stop_led_active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            boot: 1,
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
        }
    }
    fn tracker(timing: Timing) -> Tracker {
        Tracker::new(timing, scope(), 0).unwrap()
    }
    fn write(t: &mut Tracker, id: u64, action: MotorAction, now: u64) -> Ticket {
        let token = t.accepted(scope(), id, action, now).unwrap();
        t.written(token, now).unwrap();
        token
    }
    #[test]
    fn unconfigured_does_not_invent_completion_position_or_idle_timeout() {
        let mut t = tracker(Timing::default());
        let token = t.accepted(scope(), 1, MotorAction::Open, 0).unwrap();
        let s = t.snapshot();
        assert_eq!(s.pending, Some(MotorAction::Open));
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.last_written, None);
        t.written(token, 1).unwrap();
        let s = t.advance(scope(), 1_000_000).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Opening);
        assert_eq!(s.inferred_position, None);
        assert_eq!(s.observed_position, None);
        assert!(s.stop_led_active.unwrap().value);
        write(&mut t, 2, MotorAction::Stop, 1_000_001);
        let s = t.advance(scope(), 9_000_000).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert_eq!(s.inferred_position.unwrap().value, Position::Intermediate);
        assert_eq!(s.stop_led_active, None);
        assert_eq!(s.observed_movement, None);
        assert!(!t.explicit_stopped_open_gate(Some(false), 9_000_000, 5000));
    }
    #[test]
    fn configured_endpoint_and_stop_idle_have_distinct_exact_deadlines() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10_000),
            close_travel_ms: Some(12_000),
            stop_led_idle_ms: Some(2_000),
        });
        write(&mut t, 1, MotorAction::Open, 100);
        assert_eq!(
            t.advance(scope(), 10_099)
                .unwrap()
                .inferred_movement
                .unwrap()
                .value,
            Movement::Opening
        );
        let s = t.advance(scope(), 10_100).unwrap();
        assert_eq!(
            s.inferred_position.unwrap(),
            Fact {
                value: Position::Open,
                source: Provenance::Inferred,
                at_ms: 10_100
            }
        );
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert!(s.stop_led_active.unwrap().value);
        assert!(
            t.advance(scope(), 12_099)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
        assert!(
            !t.advance(scope(), 12_100)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
        assert_eq!(t.snapshot().observed_position, None);
        assert!(!t.explicit_stopped_open_gate(Some(false), 12_100, 5000));
        write(&mut t, 2, MotorAction::Close, 13_000);
        let s = t.advance(scope(), 24_999).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Closing);
        assert!(s.stop_led_active.unwrap().value);
        assert_eq!(
            t.advance(scope(), 25_000)
                .unwrap()
                .inferred_position
                .unwrap()
                .value,
            Position::Closed
        );
    }
    #[test]
    fn reversing_and_midtravel_stop_cancel_old_deadlines_without_unknown_start_guessing() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10),
            close_travel_ms: Some(20),
            stop_led_idle_ms: Some(5),
        });
        write(&mut t, 1, MotorAction::Open, 0);
        write(&mut t, 2, MotorAction::Close, 4);
        let s = t.advance(scope(), 11).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Closing);
        assert_eq!(s.inferred_position, None);
        write(&mut t, 3, MotorAction::Stop, 12);
        let s = t.advance(scope(), 25).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert_eq!(s.inferred_position.unwrap().value, Position::Intermediate);
        assert!(!s.stop_led_active.unwrap().value);
        write(&mut t, 4, MotorAction::Open, 26);
        let s = t.advance(scope(), 27).unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Opening);
        assert!(s.stop_led_active.unwrap().value);
    }
    #[test]
    fn stop_after_endpoint_does_not_fabricate_intermediate_and_unknown_stop_stays_unknown() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10),
            ..Timing::default()
        });
        write(&mut t, 1, MotorAction::Stop, 0);
        assert_eq!(t.snapshot().inferred_position, None);
        write(&mut t, 2, MotorAction::Open, 1);
        t.advance(scope(), 11).unwrap();
        write(&mut t, 3, MotorAction::Stop, 12);
        assert_eq!(
            t.snapshot().inferred_position.unwrap().value,
            Position::Open
        );
    }
    #[test]
    fn old_receipts_and_generation_changes_cannot_complete_new_motion() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10),
            ..Timing::default()
        });
        let old = t.accepted(scope(), 1, MotorAction::Open, 0).unwrap();
        let stop = t.accepted(scope(), 2, MotorAction::Stop, 1).unwrap();
        assert_eq!(t.written(old, 2), Err(Error::StaleReceipt));
        t.written(stop, 2).unwrap();
        for next in [
            Scope { boot: 2, ..scope() },
            Scope {
                epochs: Epochs {
                    transport: 2,
                    ..scope().epochs
                },
                ..scope()
            },
            Scope {
                epochs: Epochs {
                    configuration: 2,
                    ..scope().epochs
                },
                ..scope()
            },
            Scope {
                epochs: Epochs {
                    control: 2,
                    ..scope().epochs
                },
                ..scope()
            },
        ] {
            let mut t = tracker(Timing {
                open_travel_ms: Some(10),
                ..Timing::default()
            });
            write(&mut t, 1, MotorAction::Open, 0);
            let s = t.advance(next, 0).unwrap();
            assert_eq!(s.inferred_movement, None);
            assert_eq!(s.last_written, None);
            assert_eq!(s.pending, None);
            assert_eq!(t.advance(next, 100).unwrap().inferred_position, None);
        }
        let next = Scope {
            epochs: Epochs {
                transport: 2,
                ..scope().epochs
            },
            ..scope()
        };
        t.advance(next, 0).unwrap();
        t.accepted(next, 1, MotorAction::Open, 0).unwrap();
        t.advance(scope(), 0).unwrap();
        t.accepted(scope(), 1, MotorAction::Close, 0).unwrap();
        assert_eq!(
            t.written(old, 1),
            Err(Error::StaleReceipt),
            "reused scope/ID cannot revive an old opaque ticket"
        );
        let new = Tracker::new(Timing::default(), Scope { boot: 2, ..scope() }, 0).unwrap();
        assert_eq!(new.snapshot().inferred_movement, None);
    }
    #[test]
    fn knowledge_loss_and_clock_error_never_mean_stopped() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10),
            ..Timing::default()
        });
        write(&mut t, 1, MotorAction::Open, 1);
        t.invalidate();
        let s = t.advance(scope(), 100).unwrap();
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.inferred_position, None);
        assert_eq!(s.last_written, None);
        write(&mut t, 2, MotorAction::Close, 101);
        assert_eq!(t.advance(scope(), 100), Err(Error::ClockWentBackwards));
        assert_eq!(t.snapshot().inferred_movement, None);
    }
    #[test]
    fn explicit_stop_gate_requires_fresh_actual_evidence_and_known_previous_nonopen() {
        let mut t = tracker(Timing::default());
        write(&mut t, 1, MotorAction::Stop, 10);
        assert!(!t.explicit_stopped_open_gate(Some(false), 10, 5));
        t.observe(scope(), Some(Movement::Stopped), None, 10, 10)
            .unwrap();
        assert!(!t.explicit_stopped_open_gate(Some(false), 10, 5));
        t.observe(scope(), Some(Movement::Stopped), None, 11, 11)
            .unwrap();
        assert!(!t.explicit_stopped_open_gate(None, 11, 5));
        assert!(!t.explicit_stopped_open_gate(Some(true), 11, 5));
        assert!(t.explicit_stopped_open_gate(Some(false), 15, 5));
        assert!(!t.explicit_stopped_open_gate(Some(false), 16, 5));
        let ticket = t.accepted(scope(), 2, MotorAction::Open, 16).unwrap();
        assert!(!t.explicit_stopped_open_gate(Some(false), 16, 500));
        t.written(ticket, 17).unwrap();
        assert!(!t.explicit_stopped_open_gate(Some(false), 17, 500));
    }
    #[test]
    fn observed_facts_remain_distinct_and_supersede_local_time_inference() {
        let mut t = tracker(Timing {
            open_travel_ms: Some(10),
            ..Timing::default()
        });
        write(&mut t, 1, MotorAction::Open, 0);
        t.observe(
            scope(),
            Some(Movement::Stopped),
            Some(Position::Closed),
            3,
            3,
        )
        .unwrap();
        let s = t.advance(scope(), 20).unwrap();
        assert_eq!(s.observed_position.unwrap().source, Provenance::Observed);
        assert_eq!(s.observed_position.unwrap().value, Position::Closed);
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.inferred_position, None);
        assert_eq!(
            t.observe(scope(), Some(Movement::Stopped), None, 2, 20),
            Err(Error::InvalidObservation)
        );
        assert_eq!(
            t.observe(
                Scope { boot: 2, ..scope() },
                Some(Movement::Stopped),
                None,
                20,
                20
            ),
            Err(Error::InvalidObservation)
        );
        assert_eq!(
            t.observe(scope(), Some(Movement::Stopped), None, 21, 20),
            Err(Error::InvalidObservation)
        );
    }
    #[test]
    fn timings_are_optional_bounded_and_overflow_fails_unknown() {
        for ms in [0, 86_400_001, u64::MAX] {
            assert_eq!(
                Timing {
                    open_travel_ms: Some(ms),
                    ..Timing::default()
                }
                .validate(),
                Err(Error::InvalidTiming)
            );
        }
        let mut t = Tracker::new(
            Timing {
                open_travel_ms: Some(10),
                ..Timing::default()
            },
            scope(),
            u64::MAX - 5,
        )
        .unwrap();
        let ticket = t
            .accepted(scope(), 1, MotorAction::Open, u64::MAX - 5)
            .unwrap();
        assert_eq!(
            t.written(ticket, u64::MAX - 5),
            Err(Error::DeadlineOverflow)
        );
        assert_eq!(t.snapshot().inferred_movement, None);
        let mut t = tracker(Timing::default());
        assert_eq!(
            t.accepted(scope(), 0, MotorAction::Open, 0),
            Err(Error::InvalidReceipt)
        );
    }
}

#[cfg(test)]
mod written_interval_tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            boot: 3,
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
        }
    }
    fn tracker(start: Option<Position>) -> Tracker {
        let mut t = Tracker::new(
            Timing {
                open_travel_ms: Some(10000),
                close_travel_ms: Some(10000),
                stop_led_idle_ms: Some(2000),
            },
            scope(),
            0,
        )
        .unwrap();
        if let Some(position) = start {
            t.observe(scope(), None, Some(position), 0, 0).unwrap();
        }
        t
    }
    #[test]
    fn delayed_first_read_uses_written_and_exact_led_deadline() {
        let mut t = tracker(None);
        let open = t.accepted(scope(), 1, MotorAction::Open, 1000).unwrap();
        t.written_at(open, 1000, 12999).unwrap();
        let s = t.snapshot();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert_eq!(s.inferred_position.unwrap().value, Position::Open);
        assert_eq!(s.inferred_percent.unwrap().value.percent(), 100.0);
        assert!(s.stop_led_active.unwrap().value);
        assert!(
            !t.advance(scope(), 13000)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
        assert_eq!(t.snapshot().last_written.unwrap().written_ms, 1000);
        assert_eq!(t.snapshot().observed_movement, None);
    }
    #[test]
    fn stop_written_not_accepted_bounds_motion_and_preserves_unknown_start() {
        for start in [Some(Position::Closed), None] {
            let mut t = tracker(start);
            let open = t.accepted(scope(), 1, MotorAction::Open, 1000).unwrap();
            t.written_at(open, 1000, 1000).unwrap();
            let stop = t.accepted(scope(), 2, MotorAction::Stop, 3000).unwrap();
            t.written_at(stop, 5000, 7000).unwrap();
            let s = t.snapshot();
            assert_eq!(s.inferred_position.unwrap().value, Position::Intermediate);
            assert_eq!(
                s.inferred_percent.map(|f| f.value.percent()),
                start.map(|_| 40.0)
            );
            assert!(!s.stop_led_active.unwrap().value);
            assert_eq!(
                t.advance(scope(), 20000)
                    .unwrap()
                    .inferred_percent
                    .map(|f| f.value.percent()),
                start.map(|_| 40.0)
            );
        }
    }
    #[test]
    fn reversal_keeps_previous_motion_until_new_written_then_stops_at90() {
        let mut t = tracker(Some(Position::Open));
        let close = t.accepted(scope(), 1, MotorAction::Close, 1000).unwrap();
        t.written_at(close, 1000, 1000).unwrap();
        let open = t.accepted(scope(), 2, MotorAction::Open, 2000).unwrap();
        t.written_at(open, 4000, 4000).unwrap();
        assert_eq!(t.snapshot().inferred_percent.unwrap().value.percent(), 70.0);
        let stop = t.accepted(scope(), 3, MotorAction::Stop, 4500).unwrap();
        t.written_at(stop, 6000, 7000).unwrap();
        assert_eq!(t.snapshot().inferred_percent.unwrap().value.percent(), 90.0);
        assert_eq!(
            t.snapshot().inferred_movement.unwrap().value,
            Movement::Stopped
        );
    }
    #[test]
    fn partial_start_and_repeated_stop_preserve_current_estimate() {
        let mut t = tracker(Some(Position::Closed));
        let open = t.accepted(scope(), 1, MotorAction::Open, 1000).unwrap();
        t.written_at(open, 1000, 1000).unwrap();
        let stop = t.accepted(scope(), 2, MotorAction::Stop, 2000).unwrap();
        t.written_at(stop, 5000, 5000).unwrap();
        assert_eq!(t.snapshot().inferred_percent.unwrap().value.percent(), 40.0);
        let close = t.accepted(scope(), 3, MotorAction::Close, 5100).unwrap();
        t.written_at(close, 6000, 6000).unwrap();
        let stop = t.accepted(scope(), 4, MotorAction::Stop, 6500).unwrap();
        t.written_at(stop, 8000, 8000).unwrap();
        assert_eq!(t.snapshot().inferred_percent.unwrap().value.percent(), 20.0);
        let again = t.accepted(scope(), 5, MotorAction::Stop, 10000).unwrap();
        t.written_at(again, 11000, 14000).unwrap();
        assert_eq!(t.snapshot().inferred_percent.unwrap().value.percent(), 20.0);
        assert!(!t.snapshot().stop_led_active.unwrap().value);
    }
    #[test]
    fn terminal_estimate_never_suppresses_repeated_direction_or_restarts_old_timer() {
        let mut t = tracker(Some(Position::Open));
        for id in 1..=2 {
            let at = id * 20000;
            let open = t.accepted(scope(), id, MotorAction::Open, at).unwrap();
            t.written_at(open, at, at).unwrap();
            assert_eq!(
                t.snapshot().inferred_movement.unwrap().value,
                Movement::Opening
            );
            assert!(t.snapshot().stop_led_active.unwrap().value);
            t.advance(scope(), at + 13000).unwrap();
            assert!(!t.snapshot().stop_led_active.unwrap().value);
        }
        t.invalidate();
        let s = t.advance(scope(), 100000).unwrap();
        assert_eq!(s.inferred_position, None);
        assert_eq!(s.inferred_percent, None);
    }
    #[test]
    fn reviewed_estimated_stop_policy_is_local_optional_and_never_a_manual_gate() {
        let mut t = tracker(None);
        let close = t.accepted(scope(), 1, MotorAction::Close, 1000).unwrap();
        t.written_at(close, 1000, 13000).unwrap();
        assert_eq!(
            t.stopped_open_gate(Some(false), 13000, 5000, StopEvidence::ActualOnly),
            None
        );
        assert_eq!(
            t.stopped_open_gate(
                Some(false),
                13000,
                5000,
                StopEvidence::ReviewedLocalEstimate
            ),
            Some(Provenance::Inferred)
        );
        assert_eq!(
            t.stopped_open_gate(None, 13000, 5000, StopEvidence::ReviewedLocalEstimate),
            None
        );
        assert_eq!(
            t.stopped_open_gate(Some(true), 13000, 5000, StopEvidence::ReviewedLocalEstimate),
            None
        );
        t.invalidate();
        assert_eq!(
            t.stopped_open_gate(
                Some(false),
                13000,
                5000,
                StopEvidence::ReviewedLocalEstimate
            ),
            None
        );
    }
}

#[cfg(test)]
mod indicator_tests {
    use super::*;
    #[test]
    fn direction_and_stop_indicators_do_not_disable_manual_actions() {
        let scope = Scope {
            boot: 7,
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
        };
        let mut t = Tracker::new(
            Timing {
                open_travel_ms: Some(10),
                close_travel_ms: Some(10),
                stop_led_idle_ms: Some(2),
            },
            scope,
            0,
        )
        .unwrap();
        assert_eq!(
            t.snapshot().indicators(),
            IndicatorIntent {
                open_red: true,
                close_red: true,
                stop_red: None
            }
        );
        let open = t.accepted(scope, 1, MotorAction::Open, 0).unwrap();
        t.written(open, 0).unwrap();
        assert_eq!(
            t.snapshot().indicators(),
            IndicatorIntent {
                open_red: false,
                close_red: false,
                stop_red: Some(true)
            }
        );
        t.advance(scope, 10).unwrap();
        assert_eq!(
            t.snapshot().indicators(),
            IndicatorIntent {
                open_red: false,
                close_red: true,
                stop_red: Some(true)
            }
        );
        t.advance(scope, 12).unwrap();
        assert_eq!(t.snapshot().indicators().stop_red, Some(false));
        let close = t.accepted(scope, 2, MotorAction::Close, 13).unwrap();
        t.written(close, 13).unwrap();
        let stop = t.accepted(scope, 3, MotorAction::Stop, 14).unwrap();
        t.written(stop, 15).unwrap();
        assert_eq!(
            t.snapshot().indicators(),
            IndicatorIntent {
                open_red: true,
                close_red: true,
                stop_red: Some(true)
            }
        );
        let close = t.accepted(scope, 4, MotorAction::Close, 16).unwrap();
        t.written(close, 16).unwrap();
        t.advance(scope, 26).unwrap();
        assert_eq!(
            t.snapshot().indicators(),
            IndicatorIntent {
                open_red: true,
                close_red: false,
                stop_red: Some(true)
            }
        );
        t.invalidate();
        assert!(t.snapshot().indicators().open_red);
        assert!(t.snapshot().indicators().close_red);
    }
}

#[cfg(test)]
mod pending_indicator_tests {
    use super::*;
    #[test]
    fn queued_stop_or_reverse_keeps_active_direction_and_red_until_written() {
        let scope = Scope {
            boot: 9,
            epochs: Epochs {
                transport: 1,
                configuration: 1,
                control: 1,
            },
        };
        for action in [MotorAction::Stop, MotorAction::Close] {
            let mut t = Tracker::new(
                Timing {
                    open_travel_ms: Some(10000),
                    close_travel_ms: Some(10000),
                    stop_led_idle_ms: Some(2000),
                },
                scope,
                0,
            )
            .unwrap();
            let open = t.accepted(scope, 1, MotorAction::Open, 500).unwrap();
            t.written_at(open, 1000, 1000).unwrap();
            let next = t.accepted(scope, 2, action, 2000).unwrap();
            assert_eq!(
                t.advance(scope, 3000).unwrap().indicators(),
                IndicatorIntent {
                    open_red: false,
                    close_red: false,
                    stop_red: Some(true)
                }
            );
            t.written_at(next, 4000, 4000).unwrap();
            assert!(t.snapshot().stop_led_active.unwrap().value);
            t.invalidate();
            assert_eq!(t.snapshot().stop_led_active, None);
            assert!(t.snapshot().indicators().open_red);
            assert!(t.snapshot().indicators().close_red);
        }
    }
}
