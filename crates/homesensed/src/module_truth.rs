//! Single mutable owner of canonical module/channel observations. DeviceIds are
//! read-only aliases; the generic StateStore keys below are private port keys.
use homesense_config::Configuration;
use homesense_core::{DeviceId, ModuleSpec, Observation};
use homesense_state::{Change, Cursor, DeviceState, Event, Snapshot, StateError, StateStore};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[derive(Debug, Clone)]
pub struct ModuleObject {
    pub module_id: Option<String>,
    pub gateway: String,
    pub spec: ModuleSpec,
    ports: BTreeMap<u8, DeviceId>,
}
pub struct ModuleTruth {
    states: StateStore,
    modules: Vec<ModuleObject>,
    locators: BTreeMap<(String, u8, u8), DeviceId>,
    aliases: BTreeMap<DeviceId, DeviceId>,
    reverse: BTreeMap<DeviceId, DeviceId>,
    alias_versions: BTreeMap<DeviceId, u64>,
    counts: BTreeMap<DeviceId, u64>,
    port_gateway: BTreeMap<DeviceId,String>,
    unavailable_gateways:BTreeSet<String>,
    invalid: BTreeSet<DeviceId>,
    boot: u128,
    sequence: u64,
    events: VecDeque<Event>,
    max_alias_reads: usize,
}
pub(crate) struct Read {
    feedback: BTreeMap<DeviceId, homesense_core::rule::Feedback>,
    expires: Option<u64>,
    now: u64,
    max_age: u64,
}
impl Read {
    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires
    }
    pub fn feedback(&self) -> homesense_core::rule::FeedbackSnapshot<'_> {
        homesense_core::rule::FeedbackSnapshot {
            devices: &self.feedback,
            now_ms: self.now,
            max_age_ms: self.max_age,
        }
    }
}
impl ModuleTruth {
    pub fn new(config: &Configuration, boot: u128, epoch: u64) -> Result<Self, String> {
        let capacity = config
            .gateways()
            .values()
            .flat_map(|g| g.modules().values())
            .map(|m| usize::from(m.channels()))
            .sum::<usize>()
            .max(1);
        let mut states = StateStore::new(boot, epoch, capacity, 256).map_err(|e| e.to_string())?;
        let mut modules = vec![];
        let mut locators = BTreeMap::new();
        let mut counts = BTreeMap::new();
        for (gateway, g) in config.gateways() {
            for spec in g.modules().values() {
                let module_id = config.module_id(gateway, spec.address()).map(str::to_owned);
                // Legacy identities are runtime-only adapter keys, never persistent
                // module IDs. Schema2 keys are independent of address and gateway.
                let identity = module_id
                    .as_ref()
                    .map(|id| format!("stable:{id}"))
                    .unwrap_or_else(|| format!("legacy:{gateway}:{}", spec.address()));
                let hash = Sha256::digest(identity.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let mut ports = BTreeMap::new();
                for channel in 1..=spec.channels() {
                    let key = DeviceId::new(format!("port:{hash}:{channel}"))
                        .map_err(|e| e.to_string())?;
                    states
                        .register(key.clone(), spec.kind())
                        .map_err(|e| e.to_string())?;
                    locators.insert(
                        (gateway.clone(), spec.address().get(), channel),
                        key.clone(),
                    );
                    counts.insert(key.clone(), 0);
                    ports.insert(channel, key);
                }
                modules.push(ModuleObject {
                    module_id,
                    gateway: gateway.clone(),
                    spec: spec.clone(),
                    ports,
                });
            }
        }
        let port_gateway=locators.iter().map(|((gateway,_,_),key)|(key.clone(),gateway.clone())).collect();
        let mut truth = Self {
            port_gateway,
            unavailable_gateways:BTreeSet::new(),
            states,
            modules,
            locators,
            aliases: BTreeMap::new(),
            reverse: BTreeMap::new(),
            alias_versions: BTreeMap::new(),
            counts,
            invalid: BTreeSet::new(),
            boot,
            sequence: 0,
            events: VecDeque::new(),
            max_alias_reads: config.logical_devices().len().max(1),
        };
        truth.bind_aliases(config)?;
        Ok(truth)
    }
    fn alias_ports(&self, config: &Configuration) -> Result<BTreeMap<DeviceId, DeviceId>, String> {
        let mut aliases = BTreeMap::new();
        for (id, logical) in config.logical_devices().iter().filter(|(_, d)| d.enabled) {
            let port = if let Some(module_id) = &logical.module_id {
                self.modules
                    .iter()
                    .find(|object| object.module_id.as_ref() == Some(module_id))
                    .and_then(|object| object.ports.get(&logical.channel.get()))
            } else {
                // Explicit legacy adapter; addresses never enter logical objects.
                let (physical, _) = config.binding(id).ok_or("legacy binding missing")?;
                self.locators.get(&(
                    physical.gateway.clone(),
                    physical.address.get(),
                    physical.channel.get(),
                ))
            }
            .ok_or("physical module channel missing")?
            .clone();
            if aliases.values().any(|existing| existing == &port) {
                return Err("duplicate logical channel alias".into());
            }
            aliases.insert(id.clone(), port);
        }
        Ok(aliases)
    }
    fn bind_aliases(&mut self, config: &Configuration) -> Result<(), String> {
        self.aliases = self.alias_ports(config)?;
        for (id, port) in self.aliases.clone() {
            self.reverse.insert(port.clone(), id.clone());
            let state = self
                .states
                .get(&port)
                .ok_or("canonical state missing")?
                .clone();
            self.emit(id, Change::Registered, state)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// Read-only alternate logical references over the same owned module truth.
    /// Replacement/transport changes require a new owner and fresh epoch, not
    /// reinterpretation of an old module snapshot under a new physical mapping.
    pub fn project_logical(&self, config: &Configuration) -> Result<Snapshot, String> {
        if self.modules.len()
            != config
                .gateways()
                .values()
                .map(|g| g.modules().len())
                .sum::<usize>()
            || self.modules.iter().any(|object| {
                config
                    .gateways()
                    .get(&object.gateway)
                    .and_then(|g| g.modules().get(&object.spec.address()))
                    != Some(&object.spec)
                    || config.module_id(&object.gateway, object.spec.address())
                        != object.module_id.as_deref()
            })
        {
            return Err("physical registry changed; fresh owner/epoch required".into());
        }
        let aliases = self.alias_ports(config)?;
        Ok(Snapshot {
            cursor: self.cursor(),
            state_version: self.sequence,
            devices: aliases
                .into_iter()
                .filter_map(|(alias, key)| {
                    self.projected_state(&alias, &key)
                        .map(|state| (alias, state))
                })
                .collect(),
        })
    }
    fn emit(
        &mut self,
        id: DeviceId,
        change: Change,
        mut state: DeviceState,
    ) -> Result<(), StateError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(StateError::CounterExhausted)?;
        state.version = self.sequence;
        self.alias_versions.insert(id.clone(), self.sequence);
        if self.events.len() == 256 {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            cursor: self.cursor(),
            device_id: id,
            change,
            state,
        });
        Ok(())
    }
    fn project_event(&mut self, event: Event) -> Result<(), StateError> {
        if let Some(alias) = self.reverse.get(&event.device_id).cloned() {
            self.emit(alias, event.change, event.state)?;
        }
        Ok(())
    }
    fn publish_changes(&mut self, before: Cursor) -> Result<(), StateError> {
        match self.states.events_since(before) {
            Ok(events) => {
                for event in events {
                    self.project_event(event)?;
                }
            }
            Err(StateError::ResyncRequired) => {
                // A bulk physical quality update can exceed the canonical ring.
                // Invalidate the derived event cursor rather than lose quality
                // changes or turn an event journal into another mutable truth.
                self.events.clear();
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or(StateError::CounterExhausted)?;
                for (alias, key) in self.aliases.clone() {
                    self.emit(
                        alias,
                        Change::Quality,
                        self.port_state(&key).ok_or(StateError::UnknownDevice)?,
                    )?;
                }
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }
    pub fn module_objects(&self) -> &[ModuleObject] {
        &self.modules
    }
    pub fn module_state(&self, module_id: &str, channel: u8) -> Option<DeviceState> {
        let object = self
            .modules
            .iter()
            .find(|m| m.module_id.as_deref() == Some(module_id))?;
        self.port_state(object.ports.get(&channel)?)
    }
    pub fn located_state(&self, gateway: &str, address: u8, channel: u8) -> Option<DeviceState> {
        self.port_state(self.locators.get(&(gateway.to_owned(), address, channel))?)
    }
    fn port_state(&self, key: &DeviceId) -> Option<DeviceState> {
        let mut state = self.states.get(key)?.clone();
        if self.invalid.contains(key) || self.port_gateway.get(key).is_some_and(|gateway|self.unavailable_gateways.contains(gateway)) {
            state.quality = homesense_state::Quality::Unknown;
        }
        Some(state)
    }
    pub fn alias_state(&self, id: &DeviceId) -> Option<DeviceState> {
        self.projected_state(id, self.aliases.get(id)?)
    }
    fn projected_state(&self, alias: &DeviceId, key: &DeviceId) -> Option<DeviceState> {
        let mut state = self.port_state(key)?;
        state.version = self
            .alias_versions
            .get(alias)
            .copied()
            .unwrap_or(self.sequence);
        Some(state)
    }
    pub fn observation_count(&self, id: &DeviceId) -> Option<u64> {
        self.counts.get(self.aliases.get(id)?).copied()
    }
    pub(crate) fn port_observation_count(
        &self,
        gateway: &str,
        address: u8,
        channel: u8,
    ) -> Option<u64> {
        self.counts
            .get(self.locators.get(&(gateway.to_owned(), address, channel))?)
            .copied()
    }
    /// Valid actual receipt state for synchronization, before the gateway's
    /// complete-ready mask is removed from public/actuation projections.
    pub(crate) fn received_port_state(&self,gateway:&str,address:u8,channel:u8)->Option<DeviceState> {
        let key=self.locators.get(&(gateway.into(),address,channel))?;
        let mut state=self.states.get(key)?.clone();
        if self.invalid.contains(key) {state.quality=homesense_state::Quality::Unknown;}
        Some(state)
    }
    pub fn invalid_alias(&self, id: &DeviceId) -> bool {
        self.aliases
            .get(id)
            .is_some_and(|key| self.invalid.contains(key))
    }
    pub fn invalidate_port(
        &mut self,
        gateway: &str,
        address: u8,
        channel: u8,
    ) -> Result<(), StateError> {
        if let Some(key) = self
            .locators
            .get(&(gateway.to_owned(), address, channel))
            .cloned()
            && self.invalid.insert(key.clone())
            && let Some(alias) = self.reverse.get(&key).cloned()
        {
            let state = self.port_state(&key).ok_or(StateError::UnknownDevice)?;
            self.emit(alias, Change::Quality, state)?;
        }
        Ok(())
    }
    pub(crate) fn invalidate_gateway(&mut self,gateway:&str)->Result<(),StateError> {
        self.unavailable_gateways.insert(gateway.to_owned());
        let ports=self.locators.keys().filter(|(g,_,_)|g==gateway).cloned().collect::<Vec<_>>();
        for (g,address,channel) in ports {self.invalidate_port(&g,address,channel)?;}
        Ok(())
    }
    pub(crate) fn gateway_ready(&mut self,gateway:&str)->Result<(),StateError> {
        if self.unavailable_gateways.remove(gateway) {
            let aliases=self.aliases.iter().filter(|(_,key)|self.port_gateway.get(*key).is_some_and(|g|g==gateway))
                .map(|(id,key)|(id.clone(),key.clone())).collect::<Vec<_>>();
            for (id,key) in aliases {self.emit(id,Change::Quality,self.port_state(&key).ok_or(StateError::UnknownDevice)?)?;}
        }
        Ok(())
    }
    pub fn observe(
        &mut self,
        gateway: &str,
        address: u8,
        channel: u8,
        observation: Observation,
        epoch: u64,
        at: u64,
    ) -> Result<bool, StateError> {
        self.observe_received(gateway,address,channel,observation,epoch,at,at)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_received(&mut self,gateway:&str,address:u8,channel:u8,
        observation:Observation,epoch:u64,at:u64,applied:u64)->Result<bool,StateError> {
        let Some(key) = self
            .locators
            .get(&(gateway.to_owned(), address, channel))
            .cloned()
        else {
            return Ok(false);
        };
        let next = self.counts[&key]
            .checked_add(1)
            .ok_or(StateError::CounterExhausted)?;
        let before = self.states.cursor();
        self.states.observe_received(&key, observation, epoch, at,applied)?;
        self.counts.insert(key.clone(), next);
        let recovered = self.invalid.remove(&key);
        let changed = self.states.cursor() != before;
        self.publish_changes(before)?;
        if recovered
            && !changed
            && let Some(alias) = self.reverse.get(&key).cloned()
        {
            self.emit(
                alias,
                Change::Quality,
                self.port_state(&key).ok_or(StateError::UnknownDevice)?,
            )?;
        }
        Ok(true)
    }
    pub fn cursor(&self) -> Cursor {
        Cursor {
            boot_id: self.boot,
            sequence: self.sequence,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            cursor: self.cursor(),
            state_version: self.sequence,
            devices: self
                .aliases
                .iter()
                .filter_map(|(alias, key)| {
                    self.projected_state(alias, key)
                        .map(|state| (alias.clone(), state))
                })
                .collect(),
        }
    }
    pub fn events_since(&self, cursor: Cursor) -> Result<Vec<Event>, StateError> {
        if cursor.boot_id != self.boot
            || cursor.sequence > self.sequence
            || self
                .events
                .front()
                .is_some_and(|e| cursor.sequence.saturating_add(1) < e.cursor.sequence)
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
    pub fn mark_stale(&mut self, now: u64, max_age: u64) -> Result<(), StateError> {
        let before = self.states.cursor();
        self.states.mark_stale(now, max_age)?;
        self.publish_changes(before)
    }
    pub fn disconnect(&mut self, epoch: u64, now: u64) -> Result<(), StateError> {
        let before = self.states.cursor();
        self.states.disconnect(epoch, now)?;
        self.publish_changes(before)
    }
    pub fn rule_feedback(
        &self,
        epoch: u64,
        now: u64,
        max_age: u64,
    ) -> Result<BTreeMap<DeviceId, homesense_core::rule::Feedback>, StateError> {
        let physical = self.states.rule_feedback(epoch, now, max_age)?;
        Ok(self
            .aliases
            .iter()
            .filter(|(_, key)| !self.invalid.contains(*key))
            .filter_map(|(alias, key)| {
                physical
                    .get(key)
                    .cloned()
                    .map(|value| (alias.clone(), value))
            })
            .collect())
    }
    pub(crate) fn capture_rule_feedback(
        &self,
        ids: &[DeviceId],
        epoch: u64,
        now: u64,
        max_age: u64,
    ) -> Result<Read, StateError> {
        if ids.len() > self.max_alias_reads {
            return Err(StateError::Capacity);
        }
        // Capture just this rule's canonical ports, then project its aliases.
        // Multiple aliases of one port share one physical read and freshness.
        let keys=ids.iter().filter_map(|id|self.aliases.get(id)).filter(|key|!self.invalid.contains(*key)).cloned().collect::<BTreeSet<_>>();
        let physical=self.states.capture_rule_feedback(&keys.into_iter().collect::<Vec<_>>(),epoch,now,max_age)?;
        let feedback=ids.iter().filter_map(|id| {
            let key=self.aliases.get(id)?;
            if self.invalid.contains(key) {return None;}
            physical.feedback().devices.get(key).cloned().map(|value|(id.clone(),value))
        }).collect::<BTreeMap<_,_>>();
        let expires = feedback
            .values()
            .map(|s| s.observed_ms.saturating_add(max_age))
            .min();
        Ok(Read {
            feedback,
            expires,
            now,
            max_age,
        })
    }
}

#[cfg(test)]
#[path = "module_truth_tests.rs"]
mod tests;
