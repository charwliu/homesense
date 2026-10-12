//! Bounded live panel input. Start at the current cursor, never a historical snapshot.
//! The owner supplies the complete ordered feedback stream and invalidates on loss.
use crate::{panel_graph::PanelKeyBinding, panel_graph::switch_event};
use homesense_core::{
    DeviceId, ModuleKind, Observation,
    gesture::{KeyGesture, Settings},
};
use homesense_state::{Change, Cursor, Event, Quality};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub configuration: u64,
    pub transport: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Notification {
    pub aid: u64,
    pub iid: u64,
    pub value: u8,
}
#[derive(Debug, PartialEq, Eq)]
pub enum InputError {
    InvalidBinding,
    InvalidStream,
    Stopped,
}
struct Key {
    binding: PanelKeyBinding,
    gesture: KeyGesture,
}
pub struct PanelInput {
    keys: BTreeMap<DeviceId, Key>,
    generation: Generation,
    cursor: Cursor,
    now: u64,
    max_age: u64,
    stopped: bool,
}
impl PanelInput {
    pub fn new(
        bindings: Vec<(PanelKeyBinding, Settings)>,
        generation: Generation,
        cursor: Cursor,
        now: u64,
        max_age: u64,
    ) -> Result<Self, InputError> {
        if bindings.len() > 256 || max_age == 0 {
            return Err(InputError::InvalidBinding);
        }
        let mut keys = BTreeMap::new();
        let mut addresses = std::collections::BTreeSet::new();
        for (binding, settings) in bindings {
            if !(1..=6).contains(&binding.key)
                || binding.aid < 2
                || binding.event_iid == 0
                || !addresses.insert((binding.aid, binding.event_iid))
            {
                return Err(InputError::InvalidBinding);
            }
            let gesture = KeyGesture::new(settings, now).map_err(|_| InputError::InvalidBinding)?;
            if keys
                .insert(binding.device.clone(), Key { binding, gesture })
                .is_some()
            {
                return Err(InputError::InvalidBinding);
            }
        }
        Ok(Self {
            keys,
            generation,
            cursor,
            now,
            max_age,
            stopped: false,
        })
    }
    /// Stop is terminal. Reconnect/publication needs a fresh instance bound to
    /// the new live cursor; pending clicks and holds must not cross generations.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.keys.clear();
    }
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
    pub fn consume(
        &mut self,
        generation: Generation,
        events: &[Event],
        now: u64,
    ) -> Result<Vec<Notification>, InputError> {
        if self.stopped {
            return Err(InputError::Stopped);
        }
        let result = self.consume_inner(generation, events, now);
        if result.is_err() {
            self.stop();
        }
        result
    }
    fn consume_inner(
        &mut self,
        generation: Generation,
        events: &[Event],
        now: u64,
    ) -> Result<Vec<Notification>, InputError> {
        if generation != self.generation || now < self.now || events.len() > 4096 {
            return Err(InputError::InvalidStream);
        }
        let mut cursor = self.cursor;
        let mut previous = None;
        // Validate the whole batch before changing any gesture or returning output.
        for event in events {
            if event.cursor.boot_id != cursor.boot_id
                || previous.is_some_and(|p| event.cursor.sequence <= p)
            {
                return Err(InputError::InvalidStream);
            }
            previous = Some(event.cursor.sequence);
            if event.cursor.sequence <= self.cursor.sequence {
                continue;
            }
            if cursor.sequence.checked_add(1) != Some(event.cursor.sequence) {
                return Err(InputError::InvalidStream);
            }
            cursor = event.cursor;
            if event.change == Change::Button && self.keys.contains_key(&event.device_id) {
                let valid_time = event.state.observed_at_ms.is_some_and(|at| {
                    at >= self.now && now.checked_sub(at).is_some_and(|age| age < self.max_age)
                });
                if !valid_time
                    || event.state.kind != ModuleKind::Keypad
                    || event.state.quality != Quality::Confirmed
                    || !matches!(event.state.observed, Some(Observation::Button { .. }))
                {
                    return Err(InputError::InvalidStream);
                }
            }
        }
        let mut notifications = Vec::new();
        for event in events {
            if event.cursor.sequence <= self.cursor.sequence || event.change != Change::Button {
                continue;
            }
            if let Some(key) = self.keys.get_mut(&event.device_id) {
                let Some(Observation::Button { pressed }) = event.state.observed else {
                    unreachable!()
                };
                let gestures = key
                    .gesture
                    .edge(pressed, event.state.observed_at_ms.unwrap())
                    .map_err(|_| InputError::InvalidStream)?;
                notifications.extend(gestures.into_iter().filter_map(switch_event).map(|value| {
                    Notification {
                        aid: key.binding.aid,
                        iid: key.binding.event_iid,
                        value,
                    }
                }));
            }
        }
        for key in self.keys.values_mut() {
            let gestures = key
                .gesture
                .advance(now)
                .map_err(|_| InputError::InvalidStream)?;
            notifications.extend(gestures.into_iter().filter_map(switch_event).map(|value| {
                Notification {
                    aid: key.binding.aid,
                    iid: key.binding.event_iid,
                    value,
                }
            }));
        }
        self.cursor = cursor;
        self.now = now;
        Ok(notifications)
    }
}
