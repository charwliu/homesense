//! Single-owner feedback projection onto registered HAP accessory handles.
//! Publish the corresponding read snapshot before calling this adapter. An
//! interrupted/failed projection requires an explicit snapshot resynchronization.
use crate::requests::LightBinding;
use hap::{HapType, server::AccessoryHandle};
use homesense_core::{DeviceId, ModuleKind, Observation};
use homesense_state::{Change, Cursor, DeviceState, Event, Quality, Snapshot};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidBinding,
    InvalidState,
    ResyncRequired,
    ClockWentBackwards,
    PublishFailed,
}
impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProjectionError {}
struct Target {
    binding: LightBinding,
    accessory: AccessoryHandle,
    last: Option<Observation>,
}
pub struct ObservedEvents {
    targets: BTreeMap<DeviceId, Target>,
    cursor: Option<Cursor>,
    max_age_ms: u64,
    now: u64,
    resync_required: bool,
}
impl ObservedEvents {
    pub async fn new(
        bindings: Vec<(LightBinding, AccessoryHandle)>,
        max_age_ms: u64,
    ) -> Result<Self, ProjectionError> {
        if max_age_ms == 0 || bindings.len() > 4096 {
            return Err(ProjectionError::InvalidBinding);
        }
        let mut targets = BTreeMap::new();
        let mut aids = BTreeSet::new();
        for (binding, accessory) in bindings {
            if !aids.insert(binding.aid) {
                return Err(ProjectionError::InvalidBinding);
            }
            {
                let a = accessory.lock().await;
                if a.get_id() != binding.aid {
                    return Err(ProjectionError::InvalidBinding);
                }
                let service = a
                    .get_service(HapType::Lightbulb)
                    .ok_or(ProjectionError::InvalidBinding)?;
                if service
                    .get_characteristic(HapType::PowerState)
                    .map(|c| c.get_id())
                    != Some(binding.on_iid)
                    || service
                        .get_characteristic(HapType::Brightness)
                        .map(|c| c.get_id())
                        != binding.brightness_iid
                {
                    return Err(ProjectionError::InvalidBinding);
                }
            }
            if targets
                .insert(
                    binding.device.clone(),
                    Target {
                        binding,
                        accessory,
                        last: None,
                    },
                )
                .is_some()
            {
                return Err(ProjectionError::InvalidBinding);
            }
        }
        Ok(Self {
            targets,
            cursor: None,
            max_age_ms,
            now: 0,
            resync_required: true,
        })
    }
    fn observed(
        state: &DeviceState,
        dimmable: bool,
        now: u64,
        max_age: u64,
    ) -> Result<Option<Observation>, ProjectionError> {
        if state.kind
            != if dimmable {
                ModuleKind::Dimmer
            } else {
                ModuleKind::Switch
            }
        {
            return Err(ProjectionError::InvalidState);
        }
        if state.quality != Quality::Confirmed
            || !state
                .observed_at_ms
                .and_then(|at| now.checked_sub(at))
                .is_some_and(|age| age < max_age)
        {
            return Ok(None);
        }
        match (dimmable, state.observed) {
            (true, Some(value @ Observation::Level(_)))
            | (false, Some(value @ Observation::Switch(_))) => Ok(Some(value)),
            _ => Err(ProjectionError::InvalidState),
        }
    }
    async fn publish(
        target: &mut Target,
        value: Option<Observation>,
    ) -> Result<(), ProjectionError> {
        let Some(value) = value else {
            target.last = None;
            return Ok(());
        };
        let power = |v: Observation| match v {
            Observation::Switch(v) => v,
            Observation::Level(v) => v.get() > 0,
            _ => false,
        };
        let mut accessory = target.accessory.lock().await;
        if accessory.get_id() != target.binding.aid {
            return Err(ProjectionError::InvalidBinding);
        }
        let service = accessory
            .get_mut_service(HapType::Lightbulb)
            .ok_or(ProjectionError::InvalidBinding)?;
        if service
            .get_characteristic(HapType::PowerState)
            .map(|c| c.get_id())
            != Some(target.binding.on_iid)
            || service
                .get_characteristic(HapType::Brightness)
                .map(|c| c.get_id())
                != target.binding.brightness_iid
        {
            return Err(ProjectionError::InvalidBinding);
        }
        if target.last.map(power) != Some(power(value)) {
            service
                .get_mut_characteristic(HapType::PowerState)
                .ok_or(ProjectionError::InvalidBinding)?
                .set_observed_value(serde_json::json!(power(value)))
                .await
                .map_err(|_| ProjectionError::PublishFailed)?;
        }
        if target.last != Some(value)
            && let Observation::Level(level) = value
        {
            service
                .get_mut_characteristic(HapType::Brightness)
                .ok_or(ProjectionError::InvalidBinding)?
                .set_observed_value(serde_json::json!(level.get()))
                .await
                .map_err(|_| ProjectionError::PublishFailed)?;
        }
        target.last = Some(value);
        Ok(())
    }
    pub async fn resynchronize(
        &mut self,
        snapshot: &Snapshot,
        now: u64,
    ) -> Result<(), ProjectionError> {
        if snapshot.cursor.boot_id == 0 || snapshot.state_version != snapshot.cursor.sequence {
            return Err(ProjectionError::InvalidState);
        }
        if self.cursor.is_some_and(|c| {
            c.boot_id == snapshot.cursor.boot_id
                && (c.sequence > snapshot.cursor.sequence || now < self.now)
        }) {
            return Err(ProjectionError::ClockWentBackwards);
        }
        let mut values = Vec::new();
        for (id, target) in &self.targets {
            let state = snapshot
                .devices
                .get(id)
                .ok_or(ProjectionError::InvalidState)?;
            values.push((
                id.clone(),
                Self::observed(
                    state,
                    target.binding.brightness_iid.is_some(),
                    now,
                    self.max_age_ms,
                )?,
            ));
        }
        self.resync_required = true;
        for (id, value) in values {
            let target = self.targets.get_mut(&id).unwrap();
            target.last = None;
            Self::publish(target, value).await?;
        }
        self.cursor = Some(snapshot.cursor);
        self.now = now;
        self.resync_required = false;
        Ok(())
    }
    pub async fn apply(&mut self, event: &Event, now: u64) -> Result<(), ProjectionError> {
        let cursor = self.cursor.ok_or(ProjectionError::ResyncRequired)?;
        if self.resync_required
            || event.cursor.boot_id != cursor.boot_id
            || cursor.sequence.checked_add(1) != Some(event.cursor.sequence)
        {
            self.resync_required = true;
            return Err(ProjectionError::ResyncRequired);
        }
        if now < self.now {
            return Err(ProjectionError::ClockWentBackwards);
        }
        if event.state.version != event.cursor.sequence {
            self.resync_required = true;
            return Err(ProjectionError::InvalidState);
        }
        self.resync_required = true;
        if matches!(event.change, Change::Observation | Change::Quality)
            && let Some(target) = self.targets.get_mut(&event.device_id)
        {
            let observed = Self::observed(
                &event.state,
                target.binding.brightness_iid.is_some(),
                now,
                self.max_age_ms,
            )?;
            Self::publish(target, observed).await?;
        }
        self.cursor = Some(event.cursor);
        self.now = now;
        self.resync_required = false;
        Ok(())
    }
}
