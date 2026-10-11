//! Configured feedback projection. Observations never imply command causality.
use crate::transport_owner::GatewayRead;
use hb_protocol::{Decoded, Registry};
use hb_transport::Receive;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use homesense_state::{Snapshot, StateStore};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// A lease is validated while holding the same lock used by every feedback
/// mutation. The commit closure must be synchronous and must not call Feedback.
pub struct RuleLease {
    version: Arc<Mutex<u64>>,
    expected: u64,
    boot: u128,
    revision: u64,
    epoch: u64,
    captured_ms: u64,
    expires_ms: Option<u64>,
    pub(crate) observations: BTreeMap<DeviceId, Option<u64>>,
}
impl RuleLease {
    #[cfg(test)]
    pub(crate) fn commit<T>(
        &self,
        boot: u128,
        revision: u64,
        epoch: u64,
        now: u64,
        commit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = self.lock(boot, revision, epoch, now)?;
        commit()
    }
    pub(crate) fn lock(
        &self,
        boot: u128,
        revision: u64,
        epoch: u64,
        now: u64,
    ) -> Result<std::sync::MutexGuard<'_, u64>, String> {
        let version = self.version.lock().map_err(|_| "feedback lease poisoned")?;
        if *version != self.expected
            || self.boot != boot
            || self.revision != revision
            || self.epoch != epoch
            || now < self.captured_ms
            || self.expires_ms.is_some_and(|deadline| now >= deadline)
        {
            return Err("feedback lease expired or changed".into());
        }
        Ok(version)
    }
}
pub struct Feedback {
    lease_version: Arc<Mutex<u64>>,
    boot: u128,
    revision: u64,
    registries: BTreeMap<String, Registry>,
    bindings: BTreeMap<(String, u8, u8), DeviceId>,
    states: StateStore,
    observation_counts: BTreeMap<DeviceId, u64>,
    supply_relays: BTreeMap<DeviceId, DeviceId>,
    epoch: u64,
    state_epoch: u64,
    now: u64,
    stopped: bool,
}
impl Feedback {
    pub(crate) fn supply_relay(&self, device: &DeviceId) -> Option<&DeviceId> {
        self.supply_relays.get(device)
    }
    pub fn plan_rule(
        &self,
        nodes: &[homesense_core::rule::Node],
        variables: &BTreeMap<String, homesense_core::rule::Variable>,
        budget: homesense_core::rule::Budget,
        now: u64,
        max_age: u64,
    ) -> Result<(homesense_core::rule::Plan, RuleLease), String> {
        if self.stopped {
            return Err("feedback stopped".into());
        }
        let version = self
            .lease_version
            .lock()
            .map_err(|_| "feedback lease poisoned")?;
        let (plan, read) = self
            .states
            .plan_rule(nodes, variables, budget, self.epoch, now, max_age)?;
        let observations = plan
            .feedback_reads
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    read.feedback()
                        .devices
                        .get(id)
                        .map(|sample| sample.observed_ms),
                )
            })
            .collect();
        Ok((
            plan,
            RuleLease {
                observations,
                version: self.lease_version.clone(),
                expected: *version,
                boot: self.boot,
                revision: self.revision,
                epoch: self.epoch,
                captured_ms: now,
                expires_ms: read.expires_at_ms(),
            },
        ))
    }
    pub(crate) fn is_active(&self) -> bool {
        !self.stopped
    }
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
            lease_version: Arc::new(Mutex::new(0)),
            boot,
            revision: config.revision(),
            observation_counts,
            supply_relays: config
                .devices()
                .iter()
                .filter(|(_, d)| d.enabled)
                .filter_map(|(id, d)| d.supply_relay.clone().map(|relay| (id.clone(), relay)))
                .collect(),
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
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        *lease_guard = lease_guard
            .checked_add(1)
            .ok_or("feedback lease exhausted")?;

        self.stopped = true;
        self.states
            .mark_stale(self.now, 0)
            .map_err(|e| e.to_string())
    }
    /// Age observations on the same monotonic clock used by transport reads.
    pub fn expire(&mut self, now: u64, max_age_ms: u64) -> Result<(), String> {
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        *lease_guard = lease_guard
            .checked_add(1)
            .ok_or("feedback lease exhausted")?;

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
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        *lease_guard = lease_guard
            .checked_add(1)
            .ok_or("feedback lease exhausted")?;

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
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        *lease_guard = lease_guard
            .checked_add(1)
            .ok_or("feedback lease exhausted")?;

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

#[cfg(test)]
mod lease_tests {
    use super::*;
    use homesense_core::rule::{Action, Budget, Condition, Expr, Node};
    #[test]
    fn lease_binds_generation_freshness_and_all_mutations() {
        let config = Configuration::parse(
            include_bytes!("../../../config/simulation.json"),
            ExecutionContext::Simulation,
        )
        .unwrap();
        let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 123, 1).unwrap();
        let sample = |time| GatewayRead {
            gateway_id: "simulation".into(),
            transport_epoch: 1,
            received_at_ms: time,
            result: Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x15,
                    command: 0xD1,
                    value: 30,
                }],
            },
        };
        feedback.apply(&sample(10)).unwrap();
        let nodes = [Node {
            source: "lease-test".into(),
            action: Action::If {
                condition: Condition::Unknown(Expr::DeviceLevel(
                    DeviceId::new("simulation:light").unwrap(),
                )),
                then_actions: vec![],
                else_actions: vec![],
            },
        }];
        let (_, lease) = feedback
            .plan_rule(&nodes, &Default::default(), Budget::default(), 11, 10)
            .unwrap();
        assert_eq!(
            lease
                .commit::<i32>(123, config.revision(), 1, 11, || Ok(42))
                .unwrap(),
            42
        );
        for (boot, epoch, now) in [(124, 1, 11), (123, 2, 11), (123, 1, 20), (123, 1, 10)] {
            assert!(
                lease
                    .commit::<i32>(boot, config.revision(), epoch, now, || panic!(
                        "invalid lease committed"
                    ))
                    .is_err()
            );
        }
        feedback.apply(&sample(12)).unwrap();
        assert!(
            lease
                .commit::<i32>(123, config.revision(), 1, 12, || panic!(
                    "refreshed lease committed"
                ))
                .is_err()
        );
        let (_, lease) = feedback
            .plan_rule(&nodes, &Default::default(), Budget::default(), 12, 10)
            .unwrap();
        feedback.invalidate().unwrap();
        assert!(
            lease
                .commit::<i32>(123, config.revision(), 1, 12, || panic!(
                    "stopped lease committed"
                ))
                .is_err()
        );
    }
}
