//! Configured feedback projection. Observations never imply command causality.
use crate::transport_owner::GatewayRead;
use hb_protocol::{Decoded, Registry};
use hb_transport::Receive;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use homesense_state::{Snapshot, StateStore};
use std::collections::BTreeMap;

pub struct Feedback {
    revision: u64,
    registries: BTreeMap<String, Registry>,
    bindings: BTreeMap<(String, u8, u8), DeviceId>,
    states: StateStore,
    observation_counts: BTreeMap<DeviceId, u64>,
    epoch: u64,
    state_epoch: u64,
    now: u64,
    stopped: bool,
}
impl Feedback {
    pub fn configuration_revision(&self) -> u64 {
        self.revision
    }
    pub fn new(
        config: &Configuration,
        context: ExecutionContext,
        boot: u128,
        epoch: u64,
    ) -> Result<Self, String> {
        let mut states = StateStore::new(boot, epoch, config.devices().len().max(1), 256)
            .map_err(|e| e.to_string())?;
        let mut registries = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        for (name, gateway) in config.gateways() {
            let mut registry = if context == ExecutionContext::Simulation {
                Registry::for_simulation()
            } else {
                Registry::default()
            };
            for module in gateway.modules().values() {
                registry
                    .register(module.clone())
                    .map_err(|e| e.to_string())?;
            }
            registries.insert(name.clone(), registry);
        }
        for id in config.devices().keys() {
            if let Some((device, module)) = config.binding(id) {
                states
                    .register(id.clone(), module.kind())
                    .map_err(|e| e.to_string())?;
                bindings.insert(
                    (
                        device.gateway.clone(),
                        device.address.get(),
                        device.channel.get(),
                    ),
                    id.clone(),
                );
            }
        }
        let observation_counts = bindings.values().cloned().map(|id| (id, 0)).collect();
        Ok(Self {
            revision: config.revision(),
            observation_counts,
            registries,
            bindings,
            states,
            epoch,
            state_epoch: epoch,
            now: 0,
            stopped: false,
        })
    }
    /// Stop synchronously, including from an async operation cancellation guard.
    /// mark_stale emits quality events without requiring a new transport epoch.
    pub fn invalidate(&mut self) -> Result<(), String> {
        self.stopped = true;
        self.states
            .mark_stale(self.now, 0)
            .map_err(|e| e.to_string())
    }
    /// Age observations on the same monotonic clock used by transport reads.
    pub fn expire(&mut self, now: u64, max_age_ms: u64) -> Result<(), String> {
        if self.stopped || now < self.now || max_age_ms == 0 {
            return Err("inactive feedback, backwards time or invalid maximum age".into());
        }
        self.states
            .mark_stale(now, max_age_ms)
            .map_err(|e| e.to_string())?;
        self.now = now;
        Ok(())
    }
    /// Attach a newly established transport generation after invalidation.
    /// Keeps old observations stale; only new-generation feedback can confirm them.
    /// This does not mark Admission synchronized or authorize control.
    pub fn resume(&mut self, next_epoch: u64, now: u64) -> Result<(), String> {
        if !self.stopped
            || next_epoch <= self.epoch
            || next_epoch < self.state_epoch
            || now < self.now
        {
            return Err(
                "recovery requires stopped feedback, a fresh epoch and monotonic time".into(),
            );
        }
        if next_epoch > self.state_epoch {
            self.states
                .disconnect(next_epoch, now)
                .map_err(|e| e.to_string())?;
        }
        self.state_epoch = next_epoch;
        self.epoch = next_epoch;
        self.now = now;
        self.stopped = false;
        Ok(())
    }
    pub fn events_since(
        &self,
        cursor: homesense_state::Cursor,
    ) -> Result<Vec<homesense_state::Event>, String> {
        self.states.events_since(cursor).map_err(|e| e.to_string())
    }
    /// Counts valid received observations, including unchanged values. State
    /// event versions intentionally deduplicate values and are not query receipts.
    pub(crate) fn observation_count(&self, id: &DeviceId) -> Option<u64> {
        self.observation_counts.get(id).copied()
    }
    pub(crate) fn transport_epoch(&self) -> u64 {
        self.epoch
    }
    pub fn snapshot(&self) -> Snapshot {
        self.states.snapshot()
    }
    pub fn apply(&mut self, read: &GatewayRead) -> Result<usize, String> {
        if self.stopped || read.transport_epoch != self.epoch || read.received_at_ms < self.now {
            return Err("inactive feedback, stale epoch or backwards time".into());
        }
        let registry = self
            .registries
            .get(&read.gateway_id)
            .ok_or("unknown feedback gateway")?;
        match &read.result {
            Receive::Frames {
                transport_epoch,
                frames,
            } => {
                if *transport_epoch != self.epoch || frames.len() > 103 {
                    return Err("invalid feedback batch".into());
                }
                let mut count = 0;
                for frame in frames {
                    if let Decoded::Feedback {
                        address,
                        channel,
                        observation,
                    } = registry.decode(*frame)
                        && let Some(id) = self.bindings.get(&(
                            read.gateway_id.clone(),
                            address.get(),
                            channel.get(),
                        ))
                    {
                        let next = self
                            .observation_counts
                            .get(id)
                            .ok_or("missing observation counter")?
                            .checked_add(1)
                            .ok_or("observation counter exhausted")?;
                        self.states
                            .observe(id, observation, self.epoch, read.received_at_ms)
                            .map_err(|e| e.to_string())?;
                        self.observation_counts.insert(id.clone(), next);
                        count += 1;
                    }
                }
                self.now = read.received_at_ms;
                Ok(count)
            }
            Receive::Idle => {
                self.now = read.received_at_ms;
                Ok(0)
            }
            Receive::Disconnected { .. } => {
                self.stopped = true;
                self.states
                    .disconnect(
                        self.epoch.checked_add(1).ok_or("epoch exhausted")?,
                        read.received_at_ms,
                    )
                    .map_err(|e| e.to_string())?;
                self.state_epoch = self.epoch.checked_add(1).ok_or("epoch exhausted")?;
                self.now = read.received_at_ms;
                Ok(0)
            }
        }
    }
}
