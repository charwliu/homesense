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

#[path="scene_projection_lease.rs"]
mod projection_lease;
#[path="panel_query_guard.rs"]
mod panel_query_guard;
#[path="bulk_reply_guard.rs"]
mod bulk_reply_guard;
#[path="rule_feedback_scope.rs"]
mod rule_feedback_scope;
pub(crate) use projection_lease::{ProjectionLease,Fingerprint as IndicatorFingerprint};

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
    pub(crate) motor:Option<crate::motor_rules::Lease>,
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

    pub(crate) fn merge_physical(mut self,other:&Self)->Result<Self,String>{
        if !Arc::ptr_eq(&self.version,&other.version)||self.expected!=other.expected||self.boot!=other.boot||self.revision!=other.revision||self.epoch!=other.epoch||other.motor.is_some()||other.indicator_after.is_some()||!other.recorded_off_reads.is_empty(){return Err("foreign component baseline lease".into());}
        for (id,at) in &other.observations {if self.observations.get(id).is_some_and(|old|old!=at){return Err("inconsistent component baseline".into());}self.observations.insert(id.clone(),*at);}
        self.captured_ms=self.captured_ms.max(other.captured_ms);self.expires_ms=match(self.expires_ms,other.expires_ms){(Some(a),Some(b))=>Some(a.min(b)),(a,b)=>a.or(b)};Ok(self)
    }
    pub(crate) fn covers(&self,other:&Self)->bool{Arc::ptr_eq(&self.version,&other.version)&&self.expected==other.expected&&self.boot==other.boot&&self.revision==other.revision&&self.epoch==other.epoch&&self.captured_ms>=other.captured_ms&&other.expires_ms.is_none_or(|limit|self.expires_ms.is_some_and(|at|at<=limit))&&other.observations.iter().all(|(id,at)|self.observations.get(id)==Some(at))}
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
    wire:Option<homesense_core::gateway_scope::WireScope>,
}
impl SceneKeyObservation {
    pub(crate) fn event(&self)->&homesense_state::Event {&self.event}
    pub(crate) fn lease(&self)->&RuleLease {&self.lease}
    pub(crate) fn wire(&self)->Option<&homesense_core::gateway_scope::WireScope>{self.wire.as_ref()}
}
pub struct Feedback {
    lease_version: Arc<Mutex<u64>>,
    projection_versions: Arc<Mutex<projection_lease::Versions>>,
    boot: u128,
    revision: u64,
    registries: BTreeMap<String, Registry>,
    states: crate::module_truth::ModuleTruth,
    supply_relays: BTreeMap<DeviceId, DeviceId>,
    epoch: u64,
    state_epoch: u64,
    local_wires:Option<BTreeMap<String,Option<u64>>>,
    event_wires:BTreeMap<u64,(String,u64)>,
    panel_queries: panel_query_guard::Guard,
    bulk_replies: bulk_reply_guard::Guard,
    now: u64,
    stopped: bool,
    managed_max_age_ms: Option<u64>,
    query_health: crate::query_health::Health,
}
type NativeSceneCapture = (
    BTreeMap<DeviceId, Option<bool>>,
    BTreeMap<DeviceId, homesense_core::Percent>,
    RuleLease,
);
impl Feedback {
    /// Explicitly driven embedded runtimes may publish the existing physical
    /// snapshot to pending control-phase barriers. This performs no I/O and
    /// cannot manufacture targets or Written receipts; Admission validates the
    /// original source, generation, deadline, and post-write observation lease.
    pub async fn reconcile_control_phases(&self,admission:&crate::admission::Admission,controller:uuid::Uuid)->Result<(),String>{
        crate::control_batch::Consumer::default().tick(admission,controller,self).await
    }

    pub(crate) fn effective_max_age_ms(&self, requested:u64)->u64 {
        self.managed_max_age_ms.map_or(requested,|bound|requested.min(bound))
    }
    /// Continuing ownership uses the adaptive observation allowance. This
    /// never extends a command's pending confirmation or changes sample times.
    pub(crate) fn continuing_max_age_ms(&self, confirmation_age:u64)->u64 {
        self.managed_max_age_ms.unwrap_or(confirmation_age)
    }
    pub(crate) fn set_managed_max_age_ms(&mut self,bound:u64)->Result<(),String> {
        if bound==0 || bound>homesense_config::RefreshPolicy::MAX_OBSERVATION_AGE_MS {return Err("invalid managed observation allowance".into());}
        if self.managed_max_age_ms!=Some(bound) {
            let mut version=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
            *version=version.checked_add(1).ok_or("feedback lease exhausted")?;
            self.managed_max_age_ms=Some(bound);
        }
        Ok(())
    }

    /// Native eligibility distinguishes a healthy registered never-observed
    /// value from stale/invalid/failed reads. None does not mean a read timeout.
    pub(crate) fn native_scene_capture(&self,ids:&[DeviceId],now:u64,max_age:u64)->Result<NativeSceneCapture,String> {
        use homesense_core::{ModuleKind,Observation,rule::{Action,Budget,Expr,Lifetime,Node,Value,ValueType,Variable}};
        use homesense_state::Quality;
        let max_age=self.effective_max_age_ms(max_age);
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
    pub(crate) fn plan_rule_with_motor(&self,nodes:&[homesense_core::rule::Node],variables:&BTreeMap<String,homesense_core::rule::Variable>,budget:homesense_core::rule::Budget,now:u64,max_age:u64,motor:Option<&crate::motor_rules::Lease>)->Result<(homesense_core::rule::Plan,RuleLease),String> {
        self.plan_indicator_rule_inputs(nodes,variables,budget,now,max_age,&BTreeMap::new(),None,motor)
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
        self.plan_indicator_rule_inputs(nodes,variables,budget,now,max_age,recorded,policy,None)
    }
    #[allow(clippy::too_many_arguments)]
    fn plan_indicator_rule_inputs(
        &self,nodes:&[homesense_core::rule::Node],variables:&BTreeMap<String,homesense_core::rule::Variable>,budget:homesense_core::rule::Budget,
        now:u64,max_age:u64,recorded:&BTreeMap<DeviceId,homesense_core::Percent>,policy:Option<&IndicatorPolicy>,motor:Option<&crate::motor_rules::Lease>,
    )->Result<(homesense_core::rule::Plan,RuleLease),String>{
        let max_age=self.effective_max_age_ms(max_age);
        if self.stopped {
            return Err("feedback stopped".into());
        }
        let version = self
            .lease_version
            .lock()
            .map_err(|_| "feedback lease poisoned")?;
        // Catalog activity evaluates many small predicates. Read their bounded
        // input closure rather than cloning every unrelated panel/light port
        // for every scene. Actual evaluator reads still create the same lease.
        let scoped=rule_feedback_scope::inputs(nodes).map(|mut ids| {
            let supplies=ids.iter().filter_map(|id|self.supply_relays.get(id)).cloned().collect::<Vec<_>>();
            ids.extend(supplies);
            self.states.capture_rule_feedback(&ids.into_iter().collect::<Vec<_>>(),self.epoch,now,max_age)
        });
        let mut physical=match scoped {
            Some(Ok(read))=>read.feedback().devices.clone(),
            None|Some(Err(homesense_state::StateError::Capacity))=>self.states.rule_feedback(self.epoch,now,max_age).map_err(|e|e.to_string())?,
            Some(Err(error))=>return Err(error.to_string()),
        };
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
        let mut plan = homesense_core::rule::evaluate_with_inputs(
            nodes,
            variables,
            budget,
            homesense_core::rule::FeedbackSnapshot {
                devices: &logical,
                now_ms: now,
                max_age_ms: max_age,
            },
            motor.map_or(homesense_core::rule_motor::Snapshot{readings:&[],scope:None,now_ms:now},|m|m.input(now)),
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
        let motor=motor.and_then(|m|m.used(&plan.motor_reads));
        Ok((
            plan,
            RuleLease {
                motor,
                recorded_off_reads,
                indicator_after: policy.map(|p|p.after_ms),
                observations,
                version: self.lease_version.clone(),
                expected: *version,
                boot: self.boot,
                revision: self.revision,
                epoch: self.epoch,
                captured_ms: now,
                expires_ms: if self.managed_max_age_ms.is_some() {Some(read.expires_at_ms().unwrap_or(u64::MAX).min(now.saturating_add(2000)))} else {read.expires_at_ms()},
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
            managed_max_age_ms: None,
            query_health: Default::default(),
            lease_version: Arc::new(Mutex::new(0)),
            projection_versions: Arc::new(Mutex::new(projection_lease::Versions::new(&states,config,boot,epoch))),
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
            local_wires:None,
            event_wires:BTreeMap::new(),
            panel_queries: panel_query_guard::Guard::default(),
            bulk_replies: bulk_reply_guard::Guard::default(),
            now: 0,
            stopped: false,
        })
    }
    pub(crate) fn scene_projection_lease(&self,lease:&RuleLease)->Result<ProjectionLease,String>{ProjectionLease::capture(self,lease)}
    // Caller retains lease_version while changing both physical state and these
    // dependency versions. Cache validation holds that same mutation fence.
    fn refresh_projection_versions(&self)->Result<(),String>{self.projection_versions.lock().map_err(|_|"projection dependency lease poisoned")?.refresh(&self.states,self.epoch,!self.stopped)}
    fn refresh_projection_port(&self,gateway:&str,address:u8,channel:u8)->Result<(),String>{self.projection_versions.lock().map_err(|_|"projection dependency lease poisoned")?.refresh_port(&self.states,gateway,address,channel)}
    pub(crate) fn indicator_fingerprint(&self,lease:&RuleLease,level_sensitive:bool)->Result<IndicatorFingerprint,String>{Ok(self.scene_projection_lease(lease)?.fingerprint(level_sensitive))}
    /// Stop synchronously, including from an async operation cancellation guard.
    /// mark_stale emits quality events without requiring a new transport epoch.
    pub fn invalidate(&mut self) -> Result<(), String> {
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        *lease_guard = lease_guard
            .checked_add(1)
            .ok_or("feedback lease exhausted")?;

        self.stopped = true;
        self.panel_queries.reset();
        self.bulk_replies.reset();
        self.states.mark_stale(self.now,0).map_err(|e|e.to_string())?;
        self.refresh_projection_versions()
    }
    /// Age observations on the same monotonic clock used by transport reads.
    pub fn expire(&mut self, now: u64, max_age_ms: u64) -> Result<(), String> {
        let max_age_ms=self.effective_max_age_ms(max_age_ms);
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
        if self.states.cursor() != before {
            *lease_guard = next_version;
            self.refresh_projection_versions()?;
        }
        self.bulk_replies.expire(&self.states,self.epoch,now);
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
        self.panel_queries.reset();
        self.bulk_replies.reset();
        self.refresh_projection_versions()?;
        Ok(())
    }
    pub fn events_since(
        &self,
        cursor: homesense_state::Cursor,
    ) -> Result<Vec<homesense_state::Event>, String> {
        let mut events=self.states.events_since(cursor).map_err(|e| e.to_string())?;
        if let Some(wires)=&self.local_wires {
            events.retain(|event|!matches!(event.change,homesense_state::Change::Button|homesense_state::Change::Observation)
                || self.event_wires.get(&event.cursor.sequence).is_some_and(|(gateway,epoch)|wires.get(gateway)==Some(&Some(*epoch))));
        }
        Ok(events)
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
        Ok(SceneObservationStart {snapshot:self.states.snapshot(),lease:RuleLease {motor:None,
            version:self.lease_version.clone(),expected:*version,boot:self.boot,revision:self.revision,epoch:self.epoch,
            captured_ms:now,expires_ms:None,observations:BTreeMap::new(),recorded_off_reads:std::collections::BTreeSet::new(),indicator_after:None,
        }})
    }
    pub(crate) fn away_observation(&self,now:u64)->Result<crate::away_execution::Observation,String> {
        let proof=self.scene_observation_start(now)?;
        let counts=proof.snapshot().devices.keys().filter_map(|id|self.states.observation_count(id).map(|n|(id.clone(),n))).collect();
        Ok(crate::away_execution::Observation {proof,counts})
    }
    pub(crate) fn scene_key_observation(&self,event:&homesense_state::Event,now:u64)->Result<SceneKeyObservation,String> {
        let version=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
        if self.stopped || event.cursor.boot_id!=self.boot || event.change!=homesense_state::Change::Button
            || event.state.kind!=homesense_core::ModuleKind::Keypad || event.state.quality!=homesense_state::Quality::Confirmed
            || event.state.observed_at_ms.is_none() || !matches!(event.state.observed,Some(homesense_core::Observation::Button{..})) {
            return Err("invalid scene key observation".into())
        }
        if let Some(wires)=&self.local_wires && !self.event_wires.get(&event.cursor.sequence)
            .is_some_and(|(gateway,epoch)|wires.get(gateway)==Some(&Some(*epoch))) {
            return Err("scene key belongs to a retired local wire".into());
        }
        let sequence=event.cursor.sequence.checked_sub(1).ok_or("invalid scene key cursor")?;
        let recorded=self.states.events_since(homesense_state::Cursor{boot_id:self.boot,sequence}).map_err(|e|e.to_string())?;
        if recorded.first()!=Some(event) {return Err("scene key is not in the current feedback stream".into())}
        let wire=self.event_wires.get(&event.cursor.sequence).map(|(gateway,epoch)|homesense_core::gateway_scope::WireScope::from_persisted(gateway.clone(),*epoch).map_err(|e|e.to_string())).transpose()?;
        Ok(SceneKeyObservation {event:event.clone(),wire,lease:RuleLease {motor:None,
            version:self.lease_version.clone(),expected:*version,boot:self.boot,revision:self.revision,epoch:self.epoch,
            captured_ms:now,expires_ms:None,observations:BTreeMap::new(),recorded_off_reads:std::collections::BTreeSet::new(),indicator_after:None,
        }})
    }
    pub(crate) fn enable_local_wires(&mut self,wires:BTreeMap<String,u64>)->Result<(),String> {
        if wires.keys().ne(self.registries.keys()) || self.stopped {return Err("local wire registry mismatch".into());}
        self.local_wires=Some(wires.into_iter().map(|(id,epoch)|(id,Some(epoch))).collect());Ok(())
    }
    pub(crate) fn invalidate_gateway(&mut self,gateway:&str)->Result<(),String> {
        let mut guard=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
        *guard=guard.checked_add(1).ok_or("feedback lease exhausted")?;
        let wires=self.local_wires.as_mut().ok_or("local wire owner not enabled")?;
        *wires.get_mut(gateway).ok_or("unknown gateway")?=None;
        self.panel_queries.reset_gateway(gateway);self.bulk_replies.reset_gateway(gateway);
        self.states.invalidate_gateway(gateway).map_err(|e|e.to_string())?;
        self.refresh_projection_versions()
    }
    pub(crate) fn attach_gateway_wire(&mut self,gateway:&str,epoch:u64)->Result<(),String> {
        if epoch==0 {return Err("zero local epoch".into());}
        let wires=self.local_wires.as_mut().ok_or("local wire owner not enabled")?;
        *wires.get_mut(gateway).ok_or("unknown gateway")?=Some(epoch);
        self.panel_queries.reset_gateway(gateway);self.bulk_replies.reset_gateway(gateway);Ok(())
    }
    pub(crate) fn gateway_ready(&mut self,gateway:&str)->Result<(),String> {
        let mut guard=self.lease_version.lock().map_err(|_|"feedback lease poisoned")?;
        *guard=guard.checked_add(1).ok_or("feedback lease exhausted")?;
        self.states.gateway_ready(gateway).map_err(|e|e.to_string())?;
        self.refresh_projection_versions()
    }
    /// True only for new query admission. Actual feedback and control writes
    /// retain their existing evidence, deadlines, and priority.
    pub(crate) fn query_deferred(&self,gateway:&str,now:u64,postwrite_or_retry:bool)->bool {
        self.panel_queries.deferred(gateway,now,postwrite_or_retry)
            || self.bulk_replies.deferred(gateway,&self.states,self.epoch,now)
    }
    pub(crate) fn bulk_module_written(&mut self,gateway:String,devices:&[DeviceId],at:u64) {
        if devices.len()>8 && !self.stopped && self.registries.contains_key(&gateway) {
            self.bulk_replies.written(gateway,devices,&self.states,self.epoch,at);
        }
    }
    /// A missing status reply is a port-evidence failure, not socket EOF.
    /// Retain received values/timestamps while invalidating only missing ports.
    pub(crate) fn query_error(&self,bus:&str,address:u8,channel:u8)->Option<crate::query_health::Failure> {
        self.query_health.alert(bus,address,channel)
    }
    pub(crate) fn missing_query_ports(&mut self,gateway:&str,address:u8,channels:&[u8],at:u64)->Result<(),String> {
        if self.stopped || !self.registries.contains_key(gateway) || at<self.now {
            return Err("inactive or backwards query timeout evidence".into());
        }
        if channels.is_empty() {return Ok(());}
        let version=self.lease_version.clone();let mut guard=version.lock().map_err(|_|"feedback lease poisoned")?;
        *guard=guard.checked_add(1).ok_or("feedback lease exhausted")?;
        for channel in channels {
            self.states.invalidate_port(gateway,address,*channel).map_err(|e|e.to_string())?;
            self.refresh_projection_port(gateway,address,*channel)?;
            self.query_health.missing(gateway,address,*channel,at);
        }
        self.now=at;Ok(())
    }
    pub fn apply(&mut self, read: &GatewayRead) -> Result<usize, String> {
        self.apply_at(read,read.received_at_ms)
    }
    pub(crate) fn apply_at(&mut self, read:&GatewayRead,applied_at_ms:u64)->Result<usize,String> {
        let version = self.lease_version.clone();
        let mut lease_guard = version.lock().map_err(|_| "feedback lease poisoned")?;
        // Idle polling changes no observation; lease expiry is checked at use.
        if !matches!(read.result, Receive::Idle) {
            *lease_guard = lease_guard.checked_add(1).ok_or("feedback lease exhausted")?;
        }

        let expected=self.local_wires.as_ref().map_or(Some(self.epoch),|wires|wires.get(&read.gateway_id).copied().flatten());
        if self.stopped || expected!=Some(read.transport_epoch) || applied_at_ms<self.now
            || read.received_at_ms>applied_at_ms {
            return Err("inactive feedback, stale local epoch or backwards application time".into());
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
                if *transport_epoch != read.transport_epoch || frames.len() > 103 {
                    return Err("invalid feedback batch".into());
                }
                let mut count = 0;
                for frame in frames {
                    let before=self.states.cursor();
                    if let Decoded::Unknown { reason: hb_protocol::UnknownReason::InvalidValue, .. }=registry.decode(*frame)
                        && homesense_core::BusAddress::new(frame.address).ok()
                            .and_then(|address|registry.module(address))
                            .is_some_and(|module|matches!(module.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer))
                        && let Some(channel)=frame.command.checked_sub(0xd0) {
                        self.states.invalidate_port(&read.gateway_id,frame.address,channel).map_err(|e|e.to_string())?;
                        self.refresh_projection_port(&read.gateway_id,frame.address,channel)?;
                    }
                    if let Decoded::Feedback {address,channel,observation}=registry.decode(*frame)
                        && self.states.observe_received(&read.gateway_id,address.get(),channel.get(),observation,self.state_epoch,read.received_at_ms,applied_at_ms).map_err(|e|e.to_string())? {
                        count+=1;
                        self.query_health.received(&read.gateway_id,address.get(),channel.get());
                        if registry.module(address).is_some_and(|module| module.profile()==homesense_core::ProtocolProfile::Ts686Rs485SixKey)
                            && let homesense_core::Observation::Button { pressed } = observation {
                            self.panel_queries.button(&read.gateway_id,address.get(),channel.get(),pressed,read.received_at_ms);
                        }
                        // Per-frame semantic change tracking also invalidates an
                        // Off→On→Off round trip within one received batch.
                        self.refresh_projection_port(&read.gateway_id,address.get(),channel.get())?;
                    }
                    if self.local_wires.is_some() {
                        for event in self.states.events_since(before).map_err(|e|e.to_string())? {
                            if matches!(event.change,homesense_state::Change::Button|homesense_state::Change::Observation) {
                                self.event_wires.insert(event.cursor.sequence,(read.gateway_id.clone(),read.transport_epoch));
                            }
                        }
                        while self.event_wires.len()>256 {self.event_wires.pop_first();}
                    }
                }
                self.now = applied_at_ms;
                Ok(count)
            }
            Receive::Idle => {
                self.now = applied_at_ms;
                Ok(0)
            }
            Receive::Disconnected { .. } => {
                if self.local_wires.is_some() {return Err("local disconnect must be audited by its owner before projection".into());}
                self.stopped = true;
                self.panel_queries.reset();
        self.bulk_replies.reset();
                self.states
                    .disconnect(
                        self.epoch.checked_add(1).ok_or("epoch exhausted")?,
                        read.received_at_ms,
                    )
                    .map_err(|e| e.to_string())?;
                self.state_epoch = self.epoch.checked_add(1).ok_or("epoch exhausted")?;
                self.refresh_projection_versions()?;
                self.now = applied_at_ms;
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

#[cfg(test)]
mod local_event_scope_tests {
    use super::*;
    #[test]
    fn old_confirmed_key_cannot_be_recaptured_after_same_service_gateway_recovery() {
        let bytes=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"}}]}],"devices":[{"id":"key","name":"Synthetic key","gateway":"bus","address":"41","channel":1,"enabled":true}]}"#;
        let config=Configuration::parse(bytes,ExecutionContext::Simulation).unwrap();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,123,1).unwrap();
        feedback.enable_local_wires([("bus".into(),1)].into()).unwrap();
        let before=feedback.snapshot().cursor;
        let read=|epoch,at|GatewayRead {gateway_id:"bus".into(),transport_epoch:epoch,received_at_ms:at,result:Receive::Frames {transport_epoch:epoch,frames:vec![hb_protocol::RawFrame {address:0x41,command:0xd1,value:0xf1}]}};
        feedback.apply(&read(1,10)).unwrap();
        let old=feedback.events_since(before).unwrap().into_iter().find(|event|event.change==homesense_state::Change::Button).unwrap();
        assert!(feedback.scene_key_observation(&old,10).is_ok());
        feedback.invalidate_gateway("bus").unwrap();feedback.attach_gateway_wire("bus",2).unwrap();feedback.gateway_ready("bus").unwrap();
        assert!(feedback.scene_key_observation(&old,11).is_err());
        assert!(feedback.events_since(before).unwrap().iter().all(|event|event.cursor!=old.cursor));
        let fresh_before=feedback.snapshot().cursor;feedback.apply(&read(2,20)).unwrap();
        let fresh=feedback.events_since(fresh_before).unwrap().into_iter().find(|event|event.change==homesense_state::Change::Button).unwrap();
        assert!(feedback.scene_key_observation(&fresh,20).is_ok());assert!(feedback.scene_key_observation(&old,20).is_err());
        assert_eq!(feedback.transport_epoch(),1,"global service cursor remains unchanged");
    }
}

#[cfg(test)]
mod adaptive_lease_tests {
    use super::*;
    #[test]
    fn retained_three_hour_observation_does_not_become_a_three_hour_execution_lease() {
        let config=Configuration::parse(include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation).unwrap();
        let age=config.refresh_policy().observation_max_age_ms();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,9,7).unwrap();
        feedback.set_managed_max_age_ms(age).unwrap();
        feedback.apply(&GatewayRead {gateway_id:"simulation".into(),transport_epoch:7,received_at_ms:1,result:Receive::Frames {transport_epoch:7,frames:vec![hb_protocol::RawFrame {address:0x15,command:0xd1,value:30}]}}).unwrap();
        let now=10_800_001;
        feedback.expire(now,age).unwrap();
        let id=DeviceId::new("simulation:light").unwrap();
        assert_eq!(feedback.snapshot().devices[&id].observed_at_ms,Some(1));
        let (_,_,lease)=feedback.native_scene_capture(&[id],now,age).unwrap();
        assert!(lease.lock(9,1,7,now+1999).is_ok());
        assert!(lease.lock(9,1,7,now+2000).is_err(),"long polling does not extend execution proof");
    }
}
