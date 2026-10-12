//! Read-only scene-selection inference from explicitly reviewed panel keys.
//! Produces evidence, never commands or claims about EA-5's actual variables.
use crate::{DeviceId, queue::Epochs};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub boot_id: u128,
    pub epochs: Epochs,
    /// Increment after an event-buffer gap, even within the same transport.
    pub stream_generation: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// A local accepted selection intent, not physical completion proof.
    Recorded,
    Inferred(Scope),
}
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub value: Option<i64>,
    pub version: u64,
    pub authority: Authority,
}
#[derive(Debug, Clone)]
pub struct Key {
    pub device: DeviceId,
    pub group: String,
    pub member: i64,
}
#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub members: BTreeSet<i64>,
}
#[derive(Debug, Clone)]
pub struct Press {
    pub scope: Scope,
    pub sequence: u64,
    pub device: DeviceId,
    pub down: bool,
    pub observed_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    UnknownInitial,
    SelectMember,
    ToggleOff,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inference {
    pub group: String,
    pub expected_version: u64,
    pub value: Option<i64>,
    pub basis: Basis,
    pub scope: Scope,
    pub sequence: u64,
    pub device: DeviceId,
    pub observed_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration,
    InvalidSelection,
    ClockWentBackwards,
}
struct Registered {
    key: Key,
    down: bool,
    on_release: bool,
    release_armed: bool,
    last_press: Option<u64>,
}
pub struct Observer {
    started: bool,
    scope: Scope,
    cursor: u64,
    last_now: u64,
    debounce_ms: u64,
    max_age_ms: u64,
    groups: BTreeMap<String, BTreeSet<i64>>,
    keys: BTreeMap<DeviceId, Registered>,
}
impl Observer {
    pub fn new(
        scope: Scope,
        cursor: u64,
        now: u64,
        groups: Vec<Group>,
        keys: Vec<Key>,
        debounce_ms: u64,
        max_age_ms: u64,
    ) -> Result<Self, Error> {
        if scope.boot_id == 0
            || scope.epochs.configuration == 0
            || scope.epochs.transport == 0
            || scope.epochs.control == 0
            || scope.stream_generation == 0
            || groups.is_empty()
            || groups.len() > 64
            || keys.is_empty()
            || keys.len() > 256
            || !(30..=2000).contains(&debounce_ms)
            || !(debounce_ms..=30000).contains(&max_age_ms)
        {
            return Err(Error::InvalidConfiguration);
        }
        let mut registered_groups = BTreeMap::new();
        for group in groups {
            if group.id.is_empty()
                || group.id.len() > 256
                || group.id.chars().any(char::is_control)
                || group.members.is_empty()
                || group.members.len() > 32
                || group.members.iter().any(|v| *v <= 0)
                || registered_groups.insert(group.id, group.members).is_some()
            {
                return Err(Error::InvalidConfiguration);
            }
        }
        let mut registered_keys = BTreeMap::new();
        for key in keys {
            if !registered_groups
                .get(&key.group)
                .is_some_and(|members| members.contains(&key.member))
                || registered_keys
                    .insert(
                        key.device.clone(),
                        Registered {
                            key,
                            down: false,
                            on_release: false,
                            release_armed: false,
                            last_press: None,
                        },
                    )
                    .is_some()
            {
                return Err(Error::InvalidConfiguration);
            }
        }
        Ok(Self {
            started: false,
            scope,
            cursor,
            last_now: now,
            debounce_ms,
            max_age_ms,
            groups: registered_groups,
            keys: registered_keys,
        })
    }
    /// Select reviewed release-triggered keys before consuming any events.
    /// Existing callers retain press-triggered behavior. A release requires a
    /// fresh accepted press in this stream; initial holds never arm a release.
    pub fn released_keys(&mut self, devices: &[DeviceId]) -> Result<(), Error> {
        if self.started || devices.len() > 256 {
            return Err(Error::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        if devices
            .iter()
            .any(|id| !seen.insert(id) || !self.keys.contains_key(id))
        {
            return Err(Error::InvalidConfiguration);
        }
        for id in devices {
            self.keys.get_mut(id).unwrap().on_release = true;
        }
        Ok(())
    }
    /// Suppress keys known held at a new stream's initial snapshot. Call only on construction, before consuming events.
    pub fn initially_held(&mut self, devices: &[DeviceId]) -> Result<(), Error> {
        if self.started || devices.len() > 256 {
            return Err(Error::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        if devices
            .iter()
            .any(|id| !seen.insert(id) || !self.keys.contains_key(id))
        {
            return Err(Error::InvalidConfiguration);
        }
        for id in devices {
            self.keys.get_mut(id).unwrap().down = true;
        }
        Ok(())
    }
    /// Caller supplies a fresh selection snapshot and commits any inference with
    /// CAS plus this source evidence. Unknown does not default to "none".
    pub fn observe(
        &mut self,
        event: &Press,
        selection: Selection,
        now: u64,
    ) -> Result<Option<Inference>, Error> {
        if now < self.last_now {
            return Err(Error::ClockWentBackwards);
        }
        self.last_now = now;
        if event.scope != self.scope || event.sequence <= self.cursor {
            return Ok(None);
        }
        self.started = true;
        // Consume stale/future frames too; they may not become valid on replay.
        self.cursor = event.sequence;
        let Some(registered) = self.keys.get_mut(&event.device) else {
            return Ok(None);
        };
        let fresh = now
            .checked_sub(event.observed_ms)
            .is_some_and(|age| age < self.max_age_ms);
        if !event.down {
            let armed = registered.down && registered.release_armed;
            registered.release_armed = false;
            if event.observed_ms <= now {
                registered.down = false;
            }
            if !registered.on_release
                || !armed
                || !fresh
                || registered
                    .last_press
                    .is_none_or(|at| event.observed_ms < at)
            {
                return Ok(None);
            }
        } else {
            let held = registered.down;
            registered.down = true;
            if !fresh {
                registered.release_armed = false;
                return Ok(None);
            }
            if held {
                return Ok(None);
            }
            if registered.last_press.is_some_and(|at| {
                !event
                    .observed_ms
                    .checked_sub(at)
                    .is_some_and(|elapsed| elapsed >= self.debounce_ms)
            }) {
                return Ok(None);
            }
            registered.last_press = Some(event.observed_ms);
            if registered.on_release {
                registered.release_armed = true;
                return Ok(None);
            }
        }
        let value = match selection.authority {
            Authority::Recorded => selection.value,
            Authority::Inferred(scope) if scope == self.scope => selection.value,
            Authority::Inferred(_) => None,
        };
        if value.is_some_and(|v| v != 0 && !self.groups[&registered.key.group].contains(&v)) {
            return Err(Error::InvalidSelection);
        }
        let (value, basis) = match value {
            None => (None, Basis::UnknownInitial),
            Some(value) if value == registered.key.member => (Some(0), Basis::ToggleOff),
            Some(_) => (Some(registered.key.member), Basis::SelectMember),
        };
        Ok(Some(Inference {
            group: registered.key.group.clone(),
            expected_version: selection.version,
            value,
            basis,
            scope: self.scope,
            sequence: event.sequence,
            device: event.device.clone(),
            observed_ms: event.observed_ms,
        }))
    }
}
