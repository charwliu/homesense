//! Single-owner state store. Times are monotonic milliseconds since this boot,
//! supplied by the caller; tests require no wall clock or sleeps.
use homesense_core::{Command, DeviceId, ModuleKind, Observation};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Unknown,
    Confirmed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub kind: ModuleKind,
    pub desired: Option<Command>,
    pub observed: Option<Observation>,
    pub quality: Quality,
    pub observed_at_ms: Option<u64>,
    pub last_command_id: Option<u64>,
    pub pending_command: Option<u64>,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub boot_id: u128,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Registered,
    Desired,
    Observation,
    Quality,
    PendingFinished,
    Button,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub cursor: Cursor,
    pub device_id: DeviceId,
    pub change: Change,
    pub state: DeviceState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub cursor: Cursor,
    pub state_version: u64,
    pub devices: BTreeMap<DeviceId, DeviceState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    InvalidLimits,
    DuplicateDevice,
    UnknownDevice,
    WrongKind,
    ClockWentBackwards,
    WrongTransportEpoch,
    ResyncRequired,
    Capacity,
    CounterExhausted,
}
impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StateError {}

/// Opaque feedback read set. Validation and admission must run without another
/// state mutation between them in the owning actor; this is not a lock or lease.
pub struct RuleFeedbackRead {
    boot_id: u128,
    epoch: u64,
    captured_ms: u64,
    max_age_ms: u64,
    states: BTreeMap<DeviceId, Option<DeviceState>>,
    feedback: BTreeMap<DeviceId, homesense_core::rule::Feedback>,
}
impl RuleFeedbackRead {
    pub fn expires_at_ms(&self) -> Option<u64> {
        self.feedback
            .values()
            .map(|v| v.observed_ms.saturating_add(self.max_age_ms))
            .min()
    }

    pub fn feedback(&self) -> homesense_core::rule::FeedbackSnapshot<'_> {
        homesense_core::rule::FeedbackSnapshot {
            devices: &self.feedback,
            now_ms: self.captured_ms,
            max_age_ms: self.max_age_ms,
        }
    }
}

pub struct StateStore {
    boot_id: u128,
    sequence: u64,
    transport_epoch: u64,
    now_ms: u64,
    capacity: usize,
    event_capacity: usize,
    devices: BTreeMap<DeviceId, DeviceState>,
    events: VecDeque<Event>,
}
impl StateStore {
    /// A store is scoped to one gateway; its epoch is controlled by that connection actor.
    pub fn new(
        boot_id: u128,
        transport_epoch: u64,
        capacity: usize,
        event_capacity: usize,
    ) -> Result<Self, StateError> {
        if boot_id == 0 || capacity == 0 || event_capacity == 0 {
            return Err(StateError::InvalidLimits);
        }
        Ok(Self {
            boot_id,
            transport_epoch,
            sequence: 0,
            now_ms: 0,
            capacity,
            event_capacity,
            devices: BTreeMap::new(),
            events: VecDeque::new(),
        })
    }
    fn validate_time(&self, now_ms: u64) -> Result<(), StateError> {
        if now_ms < self.now_ms {
            Err(StateError::ClockWentBackwards)
        } else {
            Ok(())
        }
    }
    fn reserve_events(&self, count: usize) -> Result<(), StateError> {
        self.sequence
            .checked_add(count as u64)
            .ok_or(StateError::CounterExhausted)
            .map(|_| ())
    }
    fn emit(&mut self, id: &DeviceId, change: Change) {
        self.sequence += 1; // Each public mutation reserves sequence space before changing state.
        let state = self.devices.get_mut(id).expect("registered device");
        state.version = self.sequence;
        let event = Event {
            cursor: self.cursor(),
            device_id: id.clone(),
            change,
            state: self.devices[id].clone(),
        };
        if self.events.len() == self.event_capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
    pub fn register(&mut self, id: DeviceId, kind: ModuleKind) -> Result<(), StateError> {
        if self.devices.contains_key(&id) {
            return Err(StateError::DuplicateDevice);
        }
        if self.devices.len() == self.capacity {
            return Err(StateError::Capacity);
        }
        self.reserve_events(1)?;
        self.devices.insert(
            id.clone(),
            DeviceState {
                kind,
                desired: None,
                observed: None,
                quality: Quality::Unknown,
                observed_at_ms: None,
                last_command_id: None,
                pending_command: None,
                version: 0,
            },
        );
        self.emit(&id, Change::Registered);
        Ok(())
    }
    pub fn get(&self, id: &DeviceId) -> Option<&DeviceState> {
        self.devices.get(id)
    }
    pub const fn cursor(&self) -> Cursor {
        Cursor {
            boot_id: self.boot_id,
            sequence: self.sequence,
        }
    }
    /// Project only current, settled actual feedback for rule evaluation.
    /// The caller must additionally bind the resulting plan to its configuration
    /// generation and revalidate the read set at command admission.
    pub fn rule_feedback(
        &self,
        expected_epoch: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<BTreeMap<DeviceId, homesense_core::rule::Feedback>, StateError> {
        self.validate_time(now_ms)?;
        if expected_epoch != self.transport_epoch {
            return Err(StateError::WrongTransportEpoch);
        }
        Ok(self
            .devices
            .iter()
            .filter_map(|(id, state)| {
                if state.quality != Quality::Confirmed
                    || state.pending_command.is_some()
                    || max_age_ms == 0
                {
                    return None;
                }
                let observed_ms = state.observed_at_ms?;
                if now_ms.checked_sub(observed_ms)? >= max_age_ms {
                    return None;
                }
                let (on, level) = match state.observed? {
                    Observation::Switch(on) => (Some(on), None),
                    Observation::Level(level) => (Some(level.get() > 0), Some(level)),
                    Observation::Button { .. } => return None,
                };
                Some((
                    id.clone(),
                    homesense_core::rule::Feedback {
                        on,
                        level,
                        observed_ms,
                    },
                ))
            })
            .collect())
    }

    /// Evaluate and capture the exact feedback read set in one immutable borrow.
    /// Variables are supplied by their owner and require their own commit CAS.
    pub fn plan_rule(
        &self,
        nodes: &[homesense_core::rule::Node],
        variables: &BTreeMap<String, homesense_core::rule::Variable>,
        budget: homesense_core::rule::Budget,
        expected_epoch: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(homesense_core::rule::Plan, RuleFeedbackRead), String> {
        let feedback = self
            .rule_feedback(expected_epoch, now_ms, max_age_ms)
            .map_err(|e| e.to_string())?;
        let plan = homesense_core::rule::evaluate_with_feedback(
            nodes,
            variables,
            budget,
            homesense_core::rule::FeedbackSnapshot {
                devices: &feedback,
                now_ms,
                max_age_ms,
            },
        )
        .map_err(|e| format!("rule: {e:?}"))?;
        let devices: Vec<_> = plan.feedback_reads.iter().cloned().collect();
        let read = self
            .capture_rule_feedback(&devices, expected_epoch, now_ms, max_age_ms)
            .map_err(|e| e.to_string())?;
        Ok((plan, read))
    }

    /// Capture every device referenced by the rule, including missing devices.
    /// Unrelated device changes do not invalidate this read set.
    pub fn capture_rule_feedback(
        &self,
        devices: &[DeviceId],
        expected_epoch: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<RuleFeedbackRead, StateError> {
        if devices.len() > self.capacity {
            return Err(StateError::Capacity);
        }
        let mut feedback = self.rule_feedback(expected_epoch, now_ms, max_age_ms)?;
        let states: BTreeMap<_, _> = devices
            .iter()
            .map(|id| (id.clone(), self.devices.get(id).cloned()))
            .collect();
        feedback.retain(|id, _| states.contains_key(id));
        Ok(RuleFeedbackRead {
            boot_id: self.boot_id,
            epoch: expected_epoch,
            captured_ms: now_ms,
            max_age_ms,
            states,
            feedback,
        })
    }
    /// Revalidate at the point of admission, not merely before awaiting a queue.
    pub fn validate_rule_feedback(
        &self,
        read: &RuleFeedbackRead,
        now_ms: u64,
    ) -> Result<(), StateError> {
        self.validate_time(now_ms)?;
        if now_ms < read.captured_ms {
            return Err(StateError::ClockWentBackwards);
        }
        if self.boot_id != read.boot_id || self.transport_epoch != read.epoch {
            return Err(StateError::WrongTransportEpoch);
        }
        if read
            .states
            .iter()
            .any(|(id, state)| self.devices.get(id) != state.as_ref())
            || read.feedback.values().any(|v| {
                now_ms
                    .checked_sub(v.observed_ms)
                    .is_none_or(|age| age >= read.max_age_ms)
            })
        {
            return Err(StateError::ResyncRequired);
        }
        Ok(())
    }

    /// Snapshot and subscription cursor are captured atomically by the owning actor.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            cursor: self.cursor(),
            state_version: self.sequence,
            devices: self.devices.clone(),
        }
    }
    pub fn events_since(&self, cursor: Cursor) -> Result<Vec<Event>, StateError> {
        if cursor.boot_id != self.boot_id
            || cursor.sequence > self.sequence
            || self
                .events
                .front()
                .is_some_and(|e| cursor.sequence < e.cursor.sequence - 1)
        {
            return Err(StateError::ResyncRequired);
        }
        Ok(self
            .events
            .iter()
            .filter(|e| e.cursor.sequence > cursor.sequence)
            .cloned()
            .collect())
    }
    /// Called only after admission. Does not mutate observed or quality.
    pub fn set_desired(
        &mut self,
        id: &DeviceId,
        command_id: u64,
        command: Command,
        now_ms: u64,
    ) -> Result<(), StateError> {
        self.validate_time(now_ms)?;
        let state = self.devices.get(id).ok_or(StateError::UnknownDevice)?;
        let valid = matches!(
            (state.kind, command),
            (ModuleKind::Switch, Command::Switch(_) | Command::Toggle)
                | (
                    ModuleKind::Dimmer,
                    Command::SetLevel { .. } | Command::PauseRamp | Command::Toggle
                )
                | (ModuleKind::Motor, Command::Motor(_))
                | (ModuleKind::Keypad, Command::PanelLed(_) | Command::PanelLedOff | Command::PanelParameter(_))
        );
        if !valid {
            return Err(StateError::WrongKind);
        }
        self.reserve_events(1)?;
        self.now_ms = now_ms;
        let state = self.devices.get_mut(id).unwrap();
        state.desired = Some(command);
        state.last_command_id = Some(command_id);
        state.pending_command = Some(command_id);
        self.emit(id, Change::Desired);
        Ok(())
    }
    /// Finishing an older command cannot clear a newer pending command.
    pub fn finish_pending(
        &mut self,
        id: &DeviceId,
        command_id: u64,
        now_ms: u64,
    ) -> Result<(), StateError> {
        self.validate_time(now_ms)?;
        let state = self.devices.get(id).ok_or(StateError::UnknownDevice)?;
        if state.pending_command != Some(command_id) {
            return Ok(());
        }
        self.reserve_events(1)?;
        self.now_ms = now_ms;
        self.devices.get_mut(id).unwrap().pending_command = None;
        self.emit(id, Change::PendingFinished);
        Ok(())
    }
    /// Only the driver feedback path calls this. A TCP write is not an observation.
    pub fn observe(
        &mut self,
        id: &DeviceId,
        value: Observation,
        epoch: u64,
        now_ms: u64,
    ) -> Result<(), StateError> {
        if epoch != self.transport_epoch {
            return Err(StateError::WrongTransportEpoch);
        }
        self.validate_time(now_ms)?;
        let state = self.devices.get(id).ok_or(StateError::UnknownDevice)?;
        if !matches!(
            (state.kind, value),
            (ModuleKind::Switch, Observation::Switch(_))
                | (ModuleKind::Dimmer, Observation::Level(_))
                | (ModuleKind::Keypad, Observation::Button { .. })
        ) {
            return Err(StateError::WrongKind);
        }
        let button = matches!(value, Observation::Button { .. });
        let changed =
            button || state.observed != Some(value) || state.quality != Quality::Confirmed;
        self.reserve_events(usize::from(changed))?;
        self.now_ms = now_ms;
        let state = self.devices.get_mut(id).unwrap();
        state.observed = Some(value);
        state.observed_at_ms = Some(now_ms);
        state.quality = Quality::Confirmed;
        // Pending is managed by the command tracker, not by matching coincidental feedback.
        if changed {
            self.emit(
                id,
                if button {
                    Change::Button
                } else {
                    Change::Observation
                },
            );
        }
        Ok(())
    }
    pub fn mark_stale(&mut self, now_ms: u64, max_age_ms: u64) -> Result<(), StateError> {
        self.validate_time(now_ms)?;
        let ids: Vec<_> = self
            .devices
            .iter()
            .filter(|(_, s)| {
                s.quality == Quality::Confirmed
                    && s.observed_at_ms
                        .is_some_and(|time| now_ms - time >= max_age_ms)
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.reserve_events(ids.len())?;
        self.now_ms = now_ms;
        for id in ids {
            self.devices.get_mut(&id).unwrap().quality = Quality::Stale;
            self.emit(&id, Change::Quality);
        }
        Ok(())
    }
    pub fn disconnect(&mut self, next_epoch: u64, now_ms: u64) -> Result<(), StateError> {
        if next_epoch <= self.transport_epoch {
            return Err(StateError::WrongTransportEpoch);
        }
        self.validate_time(now_ms)?;
        let ids: Vec<_> = self
            .devices
            .iter()
            .filter(|(_, s)| s.quality == Quality::Confirmed)
            .map(|(id, _)| id.clone())
            .collect();
        self.reserve_events(ids.len())?;
        self.now_ms = now_ms;
        self.transport_epoch = next_epoch;
        for id in ids {
            self.devices.get_mut(&id).unwrap().quality = Quality::Stale;
            self.emit(&id, Change::Quality);
        }
        Ok(())
    }
}

#[cfg(test)]
mod native_toggle_tests {
    use super::*;
    #[test]
    fn relative_intent_does_not_guess_state_and_is_limited_to_lights() {
        for kind in [ModuleKind::Switch, ModuleKind::Dimmer] {
            let id = DeviceId::new("synthetic").unwrap();
            let mut state = StateStore::new(1, 1, 1, 8).unwrap();
            state.register(id.clone(), kind).unwrap();
            state.set_desired(&id, 1, Command::Toggle, 1).unwrap();
            assert_eq!(state.get(&id).unwrap().observed, None);
            assert_eq!(state.get(&id).unwrap().quality, Quality::Unknown);
            assert_eq!(state.get(&id).unwrap().pending_command, Some(1));
            let observed = if kind == ModuleKind::Switch { Observation::Switch(false) }
                else { Observation::Level(homesense_core::Percent::new(17).unwrap()) };
            state.observe(&id, observed, 1, 2).unwrap();
            assert_eq!(state.get(&id).unwrap().observed, Some(observed));
            assert_eq!(state.get(&id).unwrap().pending_command, Some(1));
            assert!(!Command::Toggle.target_matches(observed));
            state.finish_pending(&id, 1, 3).unwrap();
            assert_eq!(state.get(&id).unwrap().observed, Some(observed));
        }
        for kind in [ModuleKind::Motor, ModuleKind::Keypad] {
            let id = DeviceId::new("unsupported").unwrap();
            let mut state = StateStore::new(1, 1, 1, 8).unwrap();
            state.register(id.clone(), kind).unwrap();
            assert_eq!(state.set_desired(&id, 1, Command::Toggle, 1), Err(StateError::WrongKind));
        }
    }
}
