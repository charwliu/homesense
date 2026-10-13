//! Configured feedback projection. Observations never imply command causality.
use crate::transport_owner::GatewayRead;
use hb_protocol::{Decoded, Registry};
use hb_transport::Receive;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use homesense_state::Snapshot;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Private policy proof for LED planning only. Old evidence becomes Unknown,
/// allowing a separate fresh Switch Off to satisfy an Any condition.
pub(crate) struct IndicatorPolicy {
    pub(crate) after_ms: u64,
    /// A validated physical projection may reuse its very same observation;
    /// external key/initialization fences still require strictly later samples.
    pub(crate) include_boundary: bool,
    pub(crate) recent_off: std::collections::BTreeSet<DeviceId>,
}
/// A lease is validated while holding the same lock used by every feedback
/// mutation. The commit closure must be synchronous and must not call Feedback.
#[derive(Clone)]
pub struct RuleLease {
    version: Arc<Mutex<u64>>,
    expected: u64,
    boot: u128,
    revision: u64,
    epoch: u64,
    captured_ms: u64,
    expires_ms: Option<u64>,
    pub(crate) observations: BTreeMap<DeviceId, Option<u64>>,
    /// Zero readings supplied by durable dimmer policy, rather than an actual
    /// supply relay Off. Inferred selections require a post-key completed write.
    pub(crate) recorded_off_reads: std::collections::BTreeSet<DeviceId>,
    pub(crate) indicator_after: Option<u64>,
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
/// These proofs can only be constructed by the configured feedback owner.
/// Their lease is checked by Admission before consuming a raw key or stream cut.
pub(crate) struct SceneObservationStart {
    snapshot: Snapshot,
    lease: RuleLease,
}
impl SceneObservationStart {
    pub(crate) fn snapshot(&self)->&Snapshot {&self.snapshot}
    pub(crate) fn lease(&self)->&RuleLease {&self.lease}
}
pub(crate) struct SceneKeyObservation {
    event: homesense_state::Event,
    lease: RuleLease,
}
impl SceneKeyObservation {
    pub(crate) fn event(&self)->&homesense_state::Event {&self.event}
    pub(crate) fn lease(&self)->&RuleLease {&self.lease}
}
pub struct Feedback {
    lease_version: Arc<Mutex<u64>>,
    boot: u128,
    revision: u64,
    registries: BTreeMap<String, Registry>,
    states: crate::module_truth::ModuleTruth,
    supply_relays: BTreeMap<DeviceId, DeviceId>,
    epoch: u64,
    state_epoch: u64,
    now: u64,
    stopped: bool,
}
type NativeSceneCapture = (
    BTreeMap<DeviceId, Option<bool>>,
    BTreeMap<DeviceId, homesense_core::Percent>,
    RuleLease,
);
impl Feedback {
    /// Native eligibility distinguishes a healthy registered never-observed
    /// value from stale/invalid/failed reads. None does not mean a read timeout.
    pub(crate) fn native_scene_capture(&self,ids:&[DeviceId],now:u64,max_age:u64)->Result<NativeSceneCapture,String> {
        use homesense_core::{ModuleKind,Observation,rule::{Action,Budget,Expr,Lifetime,Node,Value,ValueType,Variable}};
        use homesense_state::Quality;
        if self.stopped || max_age==0 || now<self.now {return Err("relative feedback owner unavailable".into());}
        let snapshot=self.snapshot();let mut physical=std::collections::BTreeSet::new();
        for id in ids {physical.insert(id.clone());if let Some(relay)=self.supply_relays.get(id){physical.insert(relay.clone());}}
        let mut levels=BTreeMap::new();
        for id in physical {
            let state=snapshot.devices.get(&id).ok_or("relative device missing")?;
            if self.states.invalid_alias(&id) {return Err("relative feedback response invalid".into());}
            let initial=state.quality==Quality::Unknown && state.observed.is_none() && state.observed_at_ms.is_none() && self.states.observation_count(&id)==Some(0);
            let fresh=state.quality==Quality::Confirmed && state.observed_at_ms.and_then(|at|now.checked_sub(at)).is_some_and(|age|age<max_age);
            if !initial && !fresh {return Err("relative feedback stale or failed".into());}
            if fresh {match (state.kind,state.observed) {
                (ModuleKind::Switch,Some(Observation::Switch(_)))=>{},
                (ModuleKind::Dimmer,Some(Observation::Level(level)))=>{levels.insert(id,level);},
                _=>return Err("relative feedback type invalid".into()),
            }}
        }
        let mut variables=BTreeMap::new();let mut nodes=vec![];
        for (index,id) in ids.iter().enumerate() {let name=format!("native:{index}");variables.insert(name.clone(),Variable{value_type:ValueType::Bool,lifetime:Lifetime::Temporary,value:None});nodes.push(Node{source:name.clone(),action:Action::Set{variable:name,value:Expr::DeviceOn(id.clone())}});}
        let (plan,lease)=self.plan_rule(&nodes,&variables,Budget::default(),now,max_age)?;
        let powers=ids.iter().enumerate().map(|(index,id)|(id.clone(),match plan.variables[&format!("native:{index}")].value {Some(Value::Bool(on))=>Some(on),_=>None})).collect();
        Ok((powers,levels,lease))
    }
    pub(crate) fn supply_relay(&self, device: &DeviceId) -> Option<&DeviceId> {
        self.supply_relays.get(device)
    }
    pub(crate) fn affected_devices(&self, source: &DeviceId) -> Vec<DeviceId> {
        std::iter::once(source.clone())
            .chain(
                self.supply_relays
                    .iter()
                    .filter(|(_, relay)| *relay == source)
                    .map(|(id, _)| id.clone()),
            )
            .collect()
    }
    pub fn plan_rule(
        &self,
        nodes: &[homesense_core::rule::Node],
        variables: &BTreeMap<String, homesense_core::rule::Variable>,
        budget: homesense_core::rule::Budget,
        now: u64,
        max_age: u64,
    ) -> Result<(homesense_core::rule::Plan, RuleLease), String> {
        self.plan_rule_with_levels(nodes, variables, budget, now, max_age, &BTreeMap::new())
    }
    /// Policy reads for indicator reconciliation only. Physical rule predicates
    /// and diagnostic feedback remain actual observations.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan_rule_with_levels(
        &self,
        nodes: &[homesense_core::rule::Node],
        variables: &BTreeMap<String, homesense_core::rule::Variable>,
        budget: homesense_core::rule::Budget,
        now: u64,
        max_age: u64,
        recorded: &BTreeMap<DeviceId, homesense_core::Percent>,
    ) -> Result<(homesense_core::rule::Plan, RuleLease), String> {
        self.plan_indicator_rule(nodes,variables,budget,now,max_age,recorded,None)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan_indicator_rule(
        &self, nodes:&[homesense_core::rule::Node],variables:&BTreeMap<String,homesense_core::rule::Variable>,
        budget:homesense_core::rule::Budget,now:u64,max_age:u64,recorded:&BTreeMap<DeviceId,homesense_core::Percent>,policy:Option<&IndicatorPolicy>,
    )->Result<(homesense_core::rule::Plan,RuleLease),String> {
        if self.stopped {
            return Err("feedback stopped".into());
        }
        let version = self
            .lease_version
            .lock()
            .map_err(|_| "feedback lease poisoned")?;
        let mut physical = self
            .states
            .rule_feedback(self.epoch, now, max_age)
            .map_err(|e| e.to_string())?;
        if let Some(policy)=policy {physical.retain(|_,s|s.observed_ms>policy.after_ms || (policy.include_boundary && s.observed_ms==policy.after_ms));}
        let mut logical = physical.clone();
        for (device, relay) in &self.supply_relays {
            let projected =
                physical
                    .get(device)
                    .zip(physical.get(relay))
                    .and_then(|(dimmer, supply)| {
                        let level = dimmer.level?;
                        let powered = supply.on?;
                        Some(homesense_core::rule::Feedback {
                            on: Some(powered && level.get() > 0),
                            level: Some(if powered {
                                level
                            } else {
                                homesense_core::Percent::new(0).unwrap()
                            }),
                            observed_ms: dimmer.observed_ms.min(supply.observed_ms),
                        })
                    });
            if let Some(sample) = projected {
                logical.insert(device.clone(), sample);
            } else {
                logical.remove(device);
            }
        }
        let mut recorded_off_reads = std::collections::BTreeSet::new();
        for (id, level) in recorded {
            if let Some(sample)=logical.get_mut(id) {
                let supply_off=self.supply_relays.get(id).and_then(|relay|physical.get(relay)).is_some_and(|s|s.on==Some(false));
                let effective=if supply_off {homesense_core::Percent::new(0).unwrap()} else {*level};
                if !supply_off && effective.get()==0 && policy.is_some_and(|p|!p.recent_off.contains(id)) {
                    sample.on=None;sample.level=None;continue;
                }
                if !supply_off && effective.get()==0 {recorded_off_reads.insert(id.clone());}
                sample.level=Some(effective);
                sample.on=Some(effective.get()>0);
            }
        }
        let mut plan = homesense_core::rule::evaluate_with_feedback(
            nodes,
            variables,
            budget,
            homesense_core::rule::FeedbackSnapshot {
                devices: &logical,
                now_ms: now,
                max_age_ms: max_age,
            },
        )
        .map_err(|e| format!("rule: {e:?}"))?;
        // A logical read depends on both physical samples, including a missing
        // relay. Admission must reject pending output on either component.
        let relays: Vec<_> = plan
            .feedback_reads
            .iter()
            .filter_map(|id| self.supply_relays.get(id).cloned())
            .collect();
        plan.feedback_reads.extend(relays);
        let read = self
            .states
            .capture_rule_feedback(
                &plan.feedback_reads.iter().cloned().collect::<Vec<_>>(),
                self.epoch,
                now,
                max_age,
            )
            .map_err(|e| e.to_string())?;
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
        let recorded_off_reads = recorded_off_reads.into_iter().filter(|id|plan.feedback_reads.contains(id)).collect();
        Ok((
            plan,
            RuleLease {
                recorded_off_reads,
                indicator_after: policy.map(|p|p.after_ms),
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
        let states=crate::module_truth::ModuleTruth::new(config,boot,epoch)?;
        let mut registries = BTreeMap::new();
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
        Ok(Self {
            lease_version: Arc::new(Mutex::new(0)),
            boot,
            revision: config.revision(),
            supply_relays: config
                .devices()
                .iter()
                .filter(|(_, d)| d.enabled)
                .filter_map(|(id, d)| d.supply_relay.clone().map(|relay| (id.clone(), relay)))
                .collect(),
            registries,
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
        let before = self.states.cursor();
        let next_version = lease_guard.checked_add(1).ok_or("feedback lease exhausted")?;

        if self.stopped || now < self.now || max_age_ms == 0 {
            return Err("inactive feedback, backwards time or invalid maximum age".into());
        }
        self.states
            .mark_stale(now, max_age_ms)
            .map_err(|e| e.to_string())?;
        if self.states.cursor() != before { *lease_guard = next_version; }
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
        self.states.observation_count(id)
    }
    pub(crate) fn transport_epoch(&self) -> u64 {
        self.epoch
    }
    /// Immutable access to canonical module-channel truth, including unbound
    /// and disabled slots. This does not expose a mutable logical state store.
    pub fn module_truth(&self)->&crate::module_truth::ModuleTruth{&self.states}
    pub fn snapshot(&self) -> Snapshot {
        self.states.snapshot()
    }
    pub(crate) fn scene_observation_start(&self,now:u64)->Result<SceneObservationStart,String> {
        let version=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
        if self.stopped {return Err("scene observation feedback stopped".into())}
        Ok(SceneObservationStart {snapshot:self.states.snapshot(),lease:RuleLease {
            version:self.lease_version.clone(),expected:*version,boot:self.boot,revision:self.revision,epoch:self.epoch,
            captured_ms:now,expires_ms:None,observations:BTreeMap::new(),recorded_off_reads:std::collections::BTreeSet::new(),indicator_after:None,
        }})
    }
    pub(crate) fn scene_key_observation(&self,event:&homesense_state::Event,now:u64)->Result<SceneKeyObservation,String> {
        let version=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
        if self.stopped || event.cursor.boot_id!=self.boot || event.change!=homesense_state::Change::Button
            || event.state.kind!=homesense_core::ModuleKind::Keypad || event.state.quality!=homesense_state::Quality::Confirmed
            || event.state.observed_at_ms.is_none() || !matches!(event.state.observed,Some(homesense_core::Observation::Button{..})) {
            return Err("invalid scene key observation".into())
        }
        let sequence=event.cursor.sequence.checked_sub(1).ok_or("invalid scene key cursor")?;
        let recorded=self.states.events_since(homesense_state::Cursor{boot_id:self.boot,sequence}).map_err(|e|e.to_string())?;
        if recorded.first()!=Some(event) {return Err("scene key is not in the current feedback stream".into())}
        Ok(SceneKeyObservation {event:event.clone(),lease:RuleLease {
            version:self.lease_version.clone(),expected:*version,boot:self.boot,revision:self.revision,epoch:self.epoch,
            captured_ms:now,expires_ms:None,observations:BTreeMap::new(),recorded_off_reads:std::collections::BTreeSet::new(),indicator_after:None,
        }})
    }
    pub fn apply(&mut self, read: &GatewayRead) -> Result<usize, String> {
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        // Idle polling changes no observation; lease expiry is checked at use.
        if !matches!(read.result, Receive::Idle) {
            *lease_guard = lease_guard.checked_add(1).ok_or("feedback lease exhausted")?;
        }

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
                    if let Decoded::Unknown { reason: hb_protocol::UnknownReason::InvalidValue, .. }=registry.decode(*frame)
                        && homesense_core::BusAddress::new(frame.address).ok()
                            .and_then(|address|registry.module(address))
                            .is_some_and(|module|matches!(module.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer))
                        && let Some(channel)=frame.command.checked_sub(0xd0) {
                        self.states.invalidate_port(&read.gateway_id,frame.address,channel).map_err(|e|e.to_string())?;
                    }
                    if let Decoded::Feedback {address,channel,observation}=registry.decode(*frame)
                        && self.states.observe(&read.gateway_id,address.get(),channel.get(),observation,self.epoch,read.received_at_ms).map_err(|e|e.to_string())? {
                        count+=1;
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
    fn combined_rules_read_logical_state_and_lease_both_physical_samples() {
        use homesense_core::rule::{Lifetime, Value, ValueType, Variable};
        let bytes=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"relay"},{"id":"relay","name":"Relay","gateway":"bus","address":"03","channel":4,"enabled":true}]}"#;
        let config = Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
        let mut feedback = Feedback::new(&config, ExecutionContext::Simulation, 123, 1).unwrap();
        let light = DeviceId::new("light").unwrap();
        let relay = DeviceId::new("relay").unwrap();
        let variables = BTreeMap::from([
            (
                "on".into(),
                Variable {
                    value_type: ValueType::Bool,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            ),
            (
                "level".into(),
                Variable {
                    value_type: ValueType::Integer,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            ),
        ]);
        let nodes = [
            Node {
                source: "logical:on".into(),
                action: Action::Set {
                    variable: "on".into(),
                    value: Expr::DeviceOn(light.clone()),
                },
            },
            Node {
                source: "logical:level".into(),
                action: Action::Set {
                    variable: "level".into(),
                    value: Expr::DeviceLevel(light.clone()),
                },
            },
        ];
        let sample = |address, value, time| GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: time,
            result: Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address,
                    command: 0xd4,
                    value,
                }],
            },
        };
        feedback.apply(&sample(0x12, 30, 10)).unwrap();
        let (plan, lease) = feedback
            .plan_rule(&nodes, &variables, Budget::default(), 10, 100)
            .unwrap();
        assert_eq!(plan.variables["on"].value, None);
        assert_eq!(plan.variables["level"].value, None);
        assert!(plan.feedback_reads.contains(&relay));
        assert_eq!(lease.observations[&relay], None);
        feedback.apply(&sample(0x03, 0xf2, 20)).unwrap();
        assert!(lease.commit(123, 1, 1, 20, || Ok(())).is_err());
        let (plan, lease) = feedback
            .plan_rule(&nodes, &variables, Budget::default(), 20, 100)
            .unwrap();
        assert_eq!(plan.variables["on"].value, Some(Value::Bool(false)));
        assert_eq!(plan.variables["level"].value, Some(Value::Integer(0)));
        assert_eq!(lease.observations[&light], Some(10));
        assert_eq!(lease.observations[&relay], Some(20));
        assert!(lease.commit(123, 1, 1, 109, || Ok(())).is_ok());
        assert!(lease.commit(123, 1, 1, 110, || Ok(())).is_err());
        feedback.apply(&sample(0x03, 0xf1, 30)).unwrap();
        let (plan, _) = feedback
            .plan_rule(&nodes, &variables, Budget::default(), 30, 100)
            .unwrap();
        assert_eq!(plan.variables["on"].value, Some(Value::Bool(true)));
        assert_eq!(plan.variables["level"].value, Some(Value::Integer(30)));
        let (plan, _) = feedback
            .plan_rule(&nodes, &variables, Budget::default(), 110, 100)
            .unwrap();
        assert_eq!(plan.variables["on"].value, None);
        assert_eq!(plan.variables["level"].value, None);
    }
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
        feedback.apply(&GatewayRead {
            gateway_id: "simulation".into(), transport_epoch: 1,
            received_at_ms: 11, result: Receive::Idle,
        }).unwrap();
        feedback.expire(11, 10).unwrap();
        assert_eq!(lease.commit::<i32>(123, config.revision(), 1, 11, || Ok(7)).unwrap(), 7);
        // An actual fresh observation still invalidates the original snapshot.
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
        feedback.expire(13, 1).unwrap();
        assert!(lease.commit::<i32>(123, config.revision(), 1, 13, || panic!("stale state committed")).is_err());
        let (_, lease) = feedback.plan_rule(&nodes, &Default::default(), Budget::default(), 13, 10).unwrap();
        feedback.invalidate().unwrap();
        assert!(
            lease
                .commit::<i32>(123, config.revision(), 1, 13, || panic!(
                    "stopped lease committed"
                ))
                .is_err()
        );
    }
}
