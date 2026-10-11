//! Snapshot-only HAP reads. The runtime publishes snapshots and owns the clock;
//! reading never queries the bus, acquires runtime ownership, or submits intent.
use crate::graph::LightGraph;
use hap::characteristic::CharacteristicCallbacks;
use homesense_core::{DeviceId, ModuleKind, Observation, Percent};
use homesense_state::{Quality, Snapshot};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    Unavailable,
    InvalidMaxAge,
    GroupedWriteRequired,
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReadError {}
pub type RestoreLevels = Arc<RwLock<std::collections::BTreeMap<DeviceId, Percent>>>;

#[derive(Clone)]
pub struct ObservedView {
    snapshot: Arc<RwLock<Snapshot>>,
    restore_levels: Option<RestoreLevels>,
    max_age_ms: u64,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl ObservedView {
    /// `now` and observed timestamps must use the same monotonic boot clock.
    /// The supplied callback must be bounded, nonblocking, and perform no I/O.
    pub fn new(
        snapshot: Arc<RwLock<Snapshot>>,
        max_age_ms: u64,
        now: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Result<Self, ReadError> {
        if max_age_ms == 0 {
            return Err(ReadError::InvalidMaxAge);
        }
        Ok(Self {
            snapshot,
            restore_levels: None,
            max_age_ms,
            now: Arc::new(now),
        })
    }
    /// Brightness is a retained setting when On=false; it is not emitted light.
    /// Freshness still comes from the actual observation, never this memory.
    pub fn with_restore_levels(mut self, levels: RestoreLevels) -> Self {
        self.restore_levels = Some(levels);
        self
    }
    fn observation(&self, device: &DeviceId, dimmable: bool) -> Result<Observation, ReadError> {
        let snapshot = self
            .snapshot
            .try_read()
            .map_err(|_| ReadError::Unavailable)?;
        let state = snapshot.devices.get(device).ok_or(ReadError::Unavailable)?;
        let age = (self.now)()
            .checked_sub(state.observed_at_ms.ok_or(ReadError::Unavailable)?)
            .ok_or(ReadError::Unavailable)?;
        if state.quality != Quality::Confirmed || age >= self.max_age_ms {
            return Err(ReadError::Unavailable);
        }
        match (dimmable, state.kind, state.observed) {
            (true, ModuleKind::Dimmer, Some(value @ Observation::Level(_)))
            | (false, ModuleKind::Switch, Some(value @ Observation::Switch(_))) => Ok(value),
            _ => Err(ReadError::Unavailable),
        }
    }
    pub fn power(&self, device: &DeviceId, dimmable: bool) -> Result<bool, ReadError> {
        match self.observation(device, dimmable)? {
            Observation::Switch(value) => Ok(value),
            Observation::Level(value) => Ok(value.get() > 0),
            _ => Err(ReadError::Unavailable),
        }
    }
    pub fn brightness(&self, device: &DeviceId) -> Result<Percent, ReadError> {
        match self.observation(device, true)? {
            Observation::Level(mut value) => {
                if value.get() == 0
                    && let Some(levels) = &self.restore_levels
                {
                    value = levels
                        .try_read()
                        .map_err(|_| ReadError::Unavailable)?
                        .get(device)
                        .copied()
                        .unwrap_or(value);
                }
                log::debug!(
                    "hap_brightness_read device={device:?} observed={}",
                    value.get()
                );
                Ok(value)
            }
            _ => Err(ReadError::Unavailable),
        }
    }
}
impl LightGraph {
    /// Install before handing the graph to HAP. The grouped HTTP handler remains
    /// the only write path; per-characteristic fallback is explicitly rejected.
    pub fn install_observed_reads(&mut self, view: ObservedView) {
        for (device, accessory) in &mut self.lights {
            let dimmable = accessory.lightbulb.brightness.is_some();
            let id = device.clone();
            let reader = view.clone();
            accessory
                .lightbulb
                .power_state
                .on_read(Some(move || Ok(Some(reader.power(&id, dimmable)?))));
            accessory
                .lightbulb
                .power_state
                .on_update(Some(|_: &bool, _: &bool| {
                    Err(Box::new(ReadError::GroupedWriteRequired)
                        as Box<dyn std::error::Error + Send + Sync>)
                }));
            if let Some(brightness) = &mut accessory.lightbulb.brightness {
                let id = device.clone();
                let reader = view.clone();
                brightness.on_read(Some(move || {
                    Ok(Some(i32::from(reader.brightness(&id)?.get())))
                }));
                brightness.on_update(Some(|_: &i32, _: &i32| {
                    Err(Box::new(ReadError::GroupedWriteRequired)
                        as Box<dyn std::error::Error + Send + Sync>)
                }));
            }
        }
    }
}
