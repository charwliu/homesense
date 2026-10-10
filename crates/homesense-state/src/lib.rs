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
            (ModuleKind::Switch, Command::Switch(_))
                | (
                    ModuleKind::Dimmer,
                    Command::SetLevel { .. } | Command::PauseRamp
                )
                | (ModuleKind::Motor, Command::Motor(_))
                | (ModuleKind::Keypad, Command::PanelLed(_))
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
