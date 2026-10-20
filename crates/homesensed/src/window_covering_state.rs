//! HAP-only percentage presentation of the admission owner's written motor
//! trajectory. Unknown starts at 50%; a direction reaches its endpoint within
//! 10 seconds unless a newer written direction or STOP replaces it.
use homesense_core::{
    MotorAction,
    curtain_state::{Scope, Snapshot},
};
use uuid::Uuid;

pub const TRAVEL_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub current: u8,
    pub target: u8,
    /// HAP PositionState: decreasing=0, increasing=1, stopped=2.
    pub state: u8,
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    action: MotorAction,
    start: u8,
    written_ms: u64,
}

#[derive(Clone, Copy, Debug)]
struct AutoStop {
    command_id: u64,
    controller: Uuid,
    target: u8,
    deadline_ms: Option<u64>,
    dispatched: bool,
}

pub struct CoveringState {
    scope: Option<Scope>,
    seen_command: Option<u64>,
    motion: Option<Motion>,
    view: View,
    auto_stop: Option<AutoStop>,
}

impl Default for CoveringState {
    fn default() -> Self {
        Self {
            scope: None,
            seen_command: None,
            motion: None,
            view: View {
                current: 50,
                target: 50,
                state: 2,
            },
            auto_stop: None,
        }
    }
}

impl CoveringState {
    fn percent_at(&self, now: u64) -> u8 {
        let Some(motion) = self.motion else {
            return self.view.current;
        };
        let elapsed = now.saturating_sub(motion.written_ms).min(TRAVEL_MS);
        let remaining = match motion.action {
            MotorAction::Open => 100u64.saturating_sub(u64::from(motion.start)),
            MotorAction::Close => u64::from(motion.start),
            MotorAction::Stop => return self.view.current,
        };
        let delta = (remaining * elapsed + TRAVEL_MS / 2) / TRAVEL_MS;
        match motion.action {
            MotorAction::Open => (u64::from(motion.start) + delta).min(100) as u8,
            MotorAction::Close => u64::from(motion.start).saturating_sub(delta) as u8,
            MotorAction::Stop => self.view.current,
        }
    }

    pub fn view(&self, now: u64) -> View {
        let mut view = self.view;
        view.current = self.percent_at(now);
        if self
            .motion
            .is_some_and(|m| now.saturating_sub(m.written_ms) >= TRAVEL_MS)
        {
            view.state = 2;
        }
        view
    }

    /// Only a successfully admitted direction owns an automatic intermediate
    /// stop. Endpoints need no synthetic STOP; physical end limits remain the
    /// motor's own responsibility.
    pub fn accepted_target(&mut self, controller: Uuid, command_id: u64, target: u8) {
        self.view.target = target;
        self.auto_stop = (target > 0 && target < 100).then_some(AutoStop {
            command_id,
            controller,
            target,
            deadline_ms: None,
            dispatched: false,
        });
    }

    pub fn apply(&mut self, snapshot: Option<&Snapshot>, now: u64) -> View {
        let Some(snapshot) = snapshot else {
            *self = Self::default();
            return self.view;
        };
        if self.scope != Some(snapshot.scope) {
            *self = Self {
                scope: Some(snapshot.scope),
                ..Self::default()
            };
        }
        let Some(command) = snapshot.last_written else {
            // Owner invalidated its trajectory (disconnect, revocation, failed
            // receipt or publication). Never retain an old local percentage.
            if self.seen_command.is_some() {
                *self = Self {
                    scope: Some(snapshot.scope),
                    ..Self::default()
                };
            }
            return self.view;
        };
        if self.seen_command != Some(command.id) {
            let start = self.percent_at(command.written_ms);
            self.seen_command = Some(command.id);
            self.motion = match command.action {
                MotorAction::Stop => None,
                action => Some(Motion {
                    action,
                    start,
                    written_ms: command.written_ms,
                }),
            };
            match command.action {
                MotorAction::Stop => {
                    self.view = View {
                        current: start,
                        target: start,
                        state: 2,
                    };
                    self.auto_stop = None;
                }
                action => {
                    let endpoint = if action == MotorAction::Open { 100 } else { 0 };
                    let pending = self.auto_stop.filter(|p| p.command_id == command.id);
                    self.auto_stop = pending.map(|mut p| {
                        let distance = u64::from(start.abs_diff(p.target));
                        let span = u64::from(start.abs_diff(endpoint));
                        // A full direction is modeled as a 10-second trip from
                        // its reported start, so the requested fraction uses
                        // the same time base and cannot exceed the endpoint.
                        p.deadline_ms = command.written_ms.checked_add(
                            (TRAVEL_MS * distance + span / 2)
                                .checked_div(span)
                                .unwrap_or(0),
                        );
                        p
                    });
                    self.view = View {
                        current: start,
                        target: self.auto_stop.map_or(endpoint, |p| p.target),
                        state: if action == MotorAction::Open { 1 } else { 0 },
                    };
                }
            }
        }
        let mut next = self.view(now);
        // The single admission owner may know a prior percentage that this
        // presentation first saw after the command. Prefer that inference.
        if let Some(percent) = snapshot.inferred_percent {
            next.current = ((u32::from(percent.value.hundredths()) + 50) / 100).min(100) as u8;
        }
        if self
            .motion
            .is_some_and(|m| now.saturating_sub(m.written_ms) >= TRAVEL_MS)
        {
            next.current = if command.action == MotorAction::Open {
                100
            } else {
                0
            };
            next.state = 2;
        }
        self.view = next;
        next
    }

    pub fn due_stop(&mut self, snapshot: Option<&Snapshot>, now: u64) -> Option<(Uuid, u64)> {
        let pending = self.auto_stop.as_mut()?;
        let snapshot = snapshot?;
        if snapshot.last_written.map(|c| c.id) != Some(pending.command_id)
            || snapshot.pending.is_some()
            || pending.dispatched
            || pending.deadline_ms.is_none_or(|deadline| now < deadline)
        {
            return None;
        }
        pending.dispatched = true;
        Some((pending.controller, pending.command_id))
    }

    pub fn stop_failed(&mut self, command_id: u64) {
        if self.auto_stop.is_some_and(|p| p.command_id == command_id) {
            self.auto_stop = None;
            self.view.target = if self.view.state == 1 { 100 } else { 0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{curtain_state::Tracker, queue::Epochs};

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

    fn tracker() -> Tracker {
        Tracker::new(
            Timing {
                open_travel_ms: Some(TRAVEL_MS),
                close_travel_ms: Some(TRAVEL_MS),
                stop_led_idle_ms: None,
            },
            scope(),
            0,
        )
        .unwrap()
    }

    use homesense_core::curtain_state::Timing;

    #[test]
    fn unknown_is_fifty_and_unstopped_direction_reaches_endpoint_at_ten_seconds() {
        let mut state = CoveringState::default();
        let mut owner = tracker();
        assert_eq!(
            state.apply(Some(&owner.snapshot()), 0),
            View {
                current: 50,
                target: 50,
                state: 2
            }
        );
        let ticket = owner.accepted(scope(), 1, MotorAction::Open, 100).unwrap();
        owner.written(ticket, 100).unwrap();
        assert_eq!(
            state.apply(Some(&owner.advance(scope(), 100).unwrap()), 100),
            View {
                current: 50,
                target: 100,
                state: 1
            }
        );
        assert_eq!(
            state
                .apply(Some(&owner.advance(scope(), 5_100).unwrap()), 5_100)
                .current,
            75
        );
        assert_eq!(
            state
                .apply(Some(&owner.advance(scope(), 10_099).unwrap()), 10_099)
                .state,
            1
        );
        assert_eq!(
            state.apply(Some(&owner.advance(scope(), 10_100).unwrap()), 10_100),
            View {
                current: 100,
                target: 100,
                state: 2
            }
        );
        assert_eq!(
            state.apply(None, 10_101),
            View {
                current: 50,
                target: 50,
                state: 2
            }
        );
    }

    #[test]
    fn intermediate_target_stops_only_its_own_written_command() {
        let mut state = CoveringState::default();
        let mut owner = tracker();
        state.apply(Some(&owner.snapshot()), 0);
        let controller = Uuid::new_v4();
        let ticket = owner.accepted(scope(), 1, MotorAction::Close, 100).unwrap();
        state.accepted_target(controller, 1, 30);
        assert_eq!(state.due_stop(Some(&owner.snapshot()), 4_100), None);
        owner.written(ticket, 100).unwrap();
        assert_eq!(
            state
                .apply(Some(&owner.advance(scope(), 100).unwrap()), 100)
                .target,
            30
        );
        let before = owner.advance(scope(), 4_099).unwrap();
        assert_eq!(state.apply(Some(&before), 4_099).current, 30);
        assert_eq!(state.due_stop(Some(&before), 4_099), None);
        let due = owner.advance(scope(), 4_100).unwrap();
        assert_eq!(state.due_stop(Some(&due), 4_100), Some((controller, 1)));
        assert_eq!(state.due_stop(Some(&due), 4_200), None);
        let stop = owner
            .accepted(scope(), 2, MotorAction::Stop, 4_101)
            .unwrap();
        owner.written(stop, 4_101).unwrap();
        assert_eq!(
            state.apply(Some(&owner.advance(scope(), 4_101).unwrap()), 4_101),
            View {
                current: 30,
                target: 30,
                state: 2
            }
        );
    }

    #[test]
    fn newer_direction_replaces_old_automatic_stop() {
        let mut state = CoveringState::default();
        let mut owner = tracker();
        state.apply(Some(&owner.snapshot()), 0);
        let ticket = owner.accepted(scope(), 1, MotorAction::Open, 0).unwrap();
        state.accepted_target(Uuid::new_v4(), 1, 70);
        owner.written(ticket, 0).unwrap();
        state.apply(Some(&owner.advance(scope(), 0).unwrap()), 0);
        let next = owner
            .accepted(scope(), 2, MotorAction::Close, 2_000)
            .unwrap();
        owner.written(next, 2_000).unwrap();
        let view = state.apply(Some(&owner.advance(scope(), 2_000).unwrap()), 2_000);
        assert_eq!(view.target, 0);
        assert_eq!(
            state.due_stop(Some(&owner.advance(scope(), 10_000).unwrap()), 10_000),
            None
        );
    }
}
