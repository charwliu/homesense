//! Admission-thread-owned, opt-in inference. No per-HAP/Web timers or feedback.
use homesense_core::{
    Command, DeviceId, MotorAction,
    curtain_state::{Movement, Scope, Snapshot, Timing, Tracker},
    queue::{CancelReason, QueuedCommand},
};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A separate Web/diagnostic view. Never serialized as actual module position.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct View {
    pub source: &'static str,
    pub movement: Option<String>,
    pub position: Option<String>,
    pub percent_hundredths: Option<u16>,
    pub pending: Option<String>,
    pub stop_led_active: Option<bool>,
    pub last_written_command: Option<String>,
    pub last_written_id: Option<u64>,
    pub last_written_ms: Option<u64>,
}
impl From<Snapshot> for View {
    fn from(s: Snapshot) -> Self {
        Self {
            source: "inferred",
            movement: s.inferred_movement.map(|f| format!("{:?}", f.value)),
            position: s.inferred_position.map(|f| format!("{:?}", f.value)),
            percent_hundredths: s.inferred_percent.map(|f| f.value.hundredths()),
            pending: s.pending.map(|a| format!("{a:?}")),
            stop_led_active: s.stop_led_active.map(|f| f.value),
            last_written_command: s.last_written.map(|c| format!("{:?}", c.action)),
            last_written_id: s.last_written.map(|c| c.id),
            last_written_ms: s.last_written.map(|c| c.written_ms),
        }
    }
}
#[derive(Clone)]
struct Pending {
    action: MotorAction,
    controller: Uuid,
    deadline_ms: u64,
    source_authorization:Option<crate::source_auth::Source>,
}
struct Entry {
    model: Tracker,
    timing: Timing,
    direction_led_policy: homesense_core::curtain_state::DirectionLedPolicy,
    leds: homesense_config::CurtainLeds,
    pending: BTreeMap<u64, Pending>,
    held: Option<Snapshot>,
    source: Option<(Uuid, u64)>,
    source_authorization:Option<crate::source_auth::Source>,
    attempted: BTreeMap<DeviceId, bool>,
}
impl Entry {
    fn invalidate(&mut self) {
        self.pending.clear();
        self.discard_estimate();
    }
    fn discard_estimate(&mut self) {
        self.model.invalidate();
        self.held = None;
        self.source = None;
        self.source_authorization=None;
        self.attempted.clear();
    }
    fn snapshot(&mut self, scope: Scope, now: u64) -> Snapshot {
        let mut s = self
            .model
            .advance(scope, now)
            .expect("owner monotonic scope");
        if let Some((_, pending)) = self.pending.last_key_value() {
            s.pending = Some(pending.action);
            // Acceptance does not change the physical write trajectory. Hold
            // command motion/hints until the next real Written; never fire an
            // older endpoint or idle hint while a newer action is queued.
            if let Some(held) = self.held {
                s.inferred_movement = held.inferred_movement;
                s.inferred_position = held.inferred_position;
                s.stop_led_active = held.stop_led_active;
            }
        }
        s
    }
}
pub(crate) struct LedEffect {
    pub controller: Uuid,
    pub cause: u64,
    pub device: DeviceId,
    pub active: bool,
    pub source_authorization:Option<crate::source_auth::Source>,
}
pub(crate) struct Owner {
    revision: u64,
    scope: Scope,
    entries: BTreeMap<DeviceId, Entry>,
}
impl Owner {
    pub fn new(config: &homesense_config::Configuration, scope: Scope, now: u64) -> Self {
        let entries = config
            .devices()
            .iter()
            .filter_map(|(id, d)| {
                let spec = d.curtain_state.clone()?;
                Some((
                    id.clone(),
                    Entry {
                        model: Tracker::new(spec.timing, scope, now).expect("validated timing"),
                        timing: spec.timing,
                        direction_led_policy: spec.direction_led_policy,
                        leds: spec.leds,
                        pending: BTreeMap::new(),
                        held: None,
                        source: None,
                        source_authorization:None,
                        attempted: BTreeMap::new(),
                    },
                ))
            })
            .collect();
        Self {
            revision: config.revision(),
            scope,
            entries,
        }
    }
    pub fn synchronize(
        &mut self,
        config: &homesense_config::Configuration,
        scope: Scope,
        now: u64,
        ready: bool,
    ) {
        if self.revision != config.revision() || self.scope != scope {
            *self = Self::new(config, scope, now);
        } else if !ready {
            self.invalidate();
        }
    }
    pub fn invalidate(&mut self) {
        for entry in self.entries.values_mut() {
            entry.invalidate();
        }
    }
    /// Exact accepted-command ownership, separate from the public pending action
    /// label. Identical external actions must still invalidate a guarded read.
    pub(crate) fn pending_identity(&self,device:&DeviceId)->Vec<(u64,Uuid,MotorAction)> {
        self.entries.get(device).map(|entry|entry.pending.iter().map(|(id,p)|(*id,p.controller,p.action)).collect()).unwrap_or_default()
    }
    #[cfg(test)]
    pub fn accepted(&mut self,config:&homesense_config::Configuration,command:&QueuedCommand,controller:Uuid,now:u64) {
        self.accepted_with_source(config,command,controller,now,None);
    }
    pub fn accepted_with_source(
        &mut self,
        config: &homesense_config::Configuration,
        command: &QueuedCommand,
        controller: Uuid,
        now: u64,
        source_authorization:Option<crate::source_auth::Source>,
    ) {
        let Some(entry) = self.entries.get_mut(&command.device_id) else {
            return;
        };
        if command.epochs != self.scope.epochs {
            return;
        }
        let action = [MotorAction::Open, MotorAction::Close, MotorAction::Stop]
            .into_iter()
            .find(|&a| config.motor_command(&command.device_id, a).ok() == Some(command.command));
        let Some(action) = action else {
            return;
        };
        if entry.pending.is_empty() {
            entry.held = Some(entry.snapshot(self.scope, now));
        }
        entry.pending.insert(
            command.id,
            Pending {
                action,
                controller,
                deadline_ms: command.deadline_ms,
                source_authorization,
            },
        );
    }
    pub fn cancelled(&mut self, id: u64, reason: CancelReason) {
        for entry in self.entries.values_mut() {
            if entry.pending.remove(&id).is_some() {
                // STOP/coalescing replaces only never-written intents; it does
                // not erase the still-valid older physical command trajectory.
                if !matches!(reason, CancelReason::Stopped | CancelReason::Superseded) {
                    entry.discard_estimate();
                } else if entry.pending.is_empty() {
                    entry.held = None;
                }
            }
        }
    }
    /// Only the admission worker calls this after durable Writing -> Written.
    /// Earlier in-flight commands may complete while a later one is pending;
    /// the queue's single writer guarantees this physical order, not callers.
    pub fn written(&mut self, id: u64, now: u64) {
        for entry in self.entries.values_mut() {
            let Some(pending) = entry.pending.remove(&id) else {
                continue;
            };
            let result = entry
                .model
                .accepted(self.scope, id, pending.action, now)
                .and_then(|ticket| entry.model.written_at(ticket, now, now));
            if result.is_err() {
                entry.invalidate();
                continue;
            }
            entry.source = Some((pending.controller, id));
            entry.source_authorization=pending.source_authorization;
            if !matches!(entry.direction_led_policy,homesense_core::curtain_state::DirectionLedPolicy::Legacy) {
                // A previous attempted color is not actual feedback. Each real
                // motor write owns fresh immediate/delayed edges, once only.
                entry.attempted.clear();
            }
            entry.held = (!entry.pending.is_empty()).then(|| entry.model.snapshot());
        }
    }
    pub fn candidate(
        &mut self,
        id: DeviceId,
        timing: Timing,
        now: u64,
    ) -> Result<Snapshot, homesense_core::curtain_state::Error> {
        timing.validate()?;
        if let Some(entry) = self.entries.get(&id) {
            if entry.timing != timing {
                return Err(homesense_core::curtain_state::Error::InvalidTiming);
            }
        } else {
            self.entries.insert(
                id.clone(),
                Entry {
                    model: Tracker::new(timing, self.scope, now)?,
                    timing,
                    direction_led_policy: Default::default(),
                    leds: Default::default(),
                    pending: BTreeMap::new(),
                    held: None,
                    source: None,
                    source_authorization:None,
                    attempted: BTreeMap::new(),
                },
            );
        }
        Ok(self.snapshot(&id, now).expect("installed candidate"))
    }
    pub fn invalidate_device(&mut self, id: &DeviceId) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.invalidate();
        }
    }
    pub(crate) fn inference_source(&self,id:&DeviceId)->Option<crate::source_auth::Source> {
        self.entries.get(id).and_then(|entry|entry.source_authorization.clone())
    }
    pub fn snapshot(&mut self, id: &DeviceId, now: u64) -> Option<Snapshot> {
        Some(self.entries.get_mut(id)?.snapshot(self.scope, now))
    }
    pub fn audit(
        &mut self,
        store: &homesense_storage::Store,
        config: &homesense_config::Configuration,
        boot: &str,
        now: u64,
    ) -> Result<(), homesense_storage::Error> {
        use homesense_storage::journal::Status;
        for (device, entry) in &mut self.entries {
            if let Some(written) = entry.model.snapshot().last_written {
                let command = config
                    .motor_command(device, written.action)
                    .expect("current calibrated entry");
                let receipt =
                    store.motor_receipt(boot, written.id, device, command, self.scope.epochs)?;
                if receipt.is_none_or(|r| !matches!(r.status, Status::Written | Status::Observed)) {
                    entry.discard_estimate();
                }
            }
            let mut invalid = vec![];
            for (&id, pending) in &entry.pending {
                if pending.deadline_ms <= now
                    || !matches!(
                        store.command_status(boot, id)?,
                        Some(Status::Accepted | Status::Writing)
                    )
                {
                    invalid.push(id);
                }
            }
            if !invalid.is_empty() {
                for id in invalid {
                    entry.pending.remove(&id);
                }
                entry.discard_estimate();
            }
            // Queue expiry invalidates knowledge, never starts another action.
            let _ = entry.snapshot(self.scope, now);
        }
        Ok(())
    }
    pub fn retire_revoked(&mut self, authorize: &impl Fn(Uuid) -> bool) {
        for entry in self.entries.values_mut() {
            if entry.pending.values().any(|p| !authorize(p.controller) || p.source_authorization.as_ref().is_some_and(|source|!source.valid())) {
                entry.pending.retain(|_, p| authorize(p.controller) && p.source_authorization.as_ref().is_none_or(|source|source.valid()));
                entry.discard_estimate();
            }
            if entry
                .source
                .is_some_and(|(controller, _)| !authorize(controller)) || entry.source_authorization.as_ref().is_some_and(|source|!source.valid())
            {
                entry.discard_estimate();
            }
        }
    }
    pub fn effects(&mut self, now: u64, authorize: &impl Fn(Uuid) -> bool) -> Vec<LedEffect> {
        self.retire_revoked(authorize);
        let mut out = vec![];
        for entry in self.entries.values_mut() {
            let Some((controller, cause)) = entry.source else {
                continue;
            };
            let source_authorization=entry.source_authorization.clone();
            let snapshot=entry.snapshot(self.scope, now);
            // A pending write is not a new motion. Freeze the old hint edge at
            // acceptance until actual Written, so an old delay cannot fire over it.
            let snapshot=if entry.pending.is_empty() {snapshot} else {entry.held.unwrap_or(snapshot)};
            let intent = snapshot.indicators_for(entry.direction_led_policy);
            for (ids, color) in [
                (&entry.leds.open, intent.open_red),
                (&entry.leds.close, intent.close_red),
                (&entry.leds.stop, intent.stop_red),
            ] {
                if let Some(active) = color {
                    for device in ids {
                        if entry.attempted.get(device) != Some(&active) {
                            // At most one admission attempt per hint edge. No
                            // capacity/authorization failure can replay an edge.
                            entry.attempted.insert(device.clone(), active);
                            out.push(LedEffect {
                                controller,
                                cause,
                                device: device.clone(),
                                active,
                                source_authorization:source_authorization.clone(),
                            });
                        }
                    }
                }
            }
        }
        out
    }
    /// A queued delayed hint cannot outlive its motor trajectory. This is an
    /// additional dispatch fence for the opt-in policy, not physical readback.
    pub(crate) fn permits_queued_hint(&mut self, command:&QueuedCommand, now:u64)->bool {
        let Command::PanelLed(active)=command.command else {return true;};
        if command.origin!=homesense_core::queue::Origin::Rule {return true;}
        for entry in self.entries.values_mut() {
            if matches!(entry.direction_led_policy,homesense_core::curtain_state::DirectionLedPolicy::Legacy) {continue;}
            let role=if entry.leds.open.contains(&command.device_id) {0}
                else if entry.leds.close.contains(&command.device_id) {1}
                else if entry.leds.stop.contains(&command.device_id) {2} else {continue;};
            if entry.source.map(|(_,cause)|cause)!=command.run_id || command.run_id.is_none() {return false;}
            let snapshot=entry.snapshot(self.scope,now);
            let snapshot=if entry.pending.is_empty(){snapshot}else{entry.held.unwrap_or(snapshot)};
            let hint=snapshot.indicators_for(entry.direction_led_policy);
            return [hint.open_red,hint.close_red,hint.stop_red][role]==Some(active);
        }
        true
    }
    pub fn next_deadline(&mut self, now: u64) -> Option<u64> {
        self.entries
            .values_mut()
            .filter_map(|entry| {
                if !entry.pending.is_empty() {
                    return None;
                }
                let s = entry.snapshot(self.scope, now);
                let movement = s.inferred_movement?;
                let physical = match movement.value {
                    Movement::Opening => s.last_written.and_then(|w|entry.timing.open_travel_ms.and_then(|ms|w.written_ms.checked_add(ms))),
                    Movement::Closing => s.last_written.and_then(|w|entry.timing.close_travel_ms.and_then(|ms|w.written_ms.checked_add(ms))),
                    Movement::Stopped => entry.timing.stop_led_idle_ms.and_then(|ms|movement.at_ms.checked_add(ms)),
                }.filter(|deadline|*deadline>now);
                physical.into_iter().chain(s.direction_deadline(entry.direction_led_policy)).min()
            })
            .min()
    }
}

/// Fixed reviewed LED effects retain the originating controller and causal
/// motor id; they are never submitted under an administrator proxy.
#[allow(clippy::too_many_arguments)]
pub(crate) fn admit_leds(
    effects: Vec<LedEffect>,
    config: &homesense_config::Configuration,
    arbiter: &mut homesense_core::arbiter::Arbiter,
    store: &mut homesense_storage::Store,
    boot: &str,
    now: u64,
    ttl: u64,
    next_id: &mut u64,
    principals: &mut BTreeMap<u64, Uuid>,
    context: homesense_core::ExecutionContext,
    authorize: &impl Fn(Uuid) -> bool,
    sources:&mut BTreeMap<u64,crate::source_auth::Source>,
    gateways:Option<&crate::gateway_runtime::State>,
) -> Result<(), homesense_storage::Error> {
    use homesense_core::{admission::Access, arbiter::BatchCommitError, queue::Origin};
    for effect in effects {
        if !authorize(effect.controller) || effect.source_authorization.as_ref().is_some_and(|source|!source.valid_for(effect.controller,config.revision())) {
            continue;
        }
        // LED effects inherit the motor's original durable causal scope. A
        // reconnected bus cannot revive an old motor timer or capture new epochs.
        let requirements=if let Some(g)=gateways {
            let requirements=store.command_gateway_requirements(boot,effect.cause)?;
            if requirements.iter().any(|wire|!g.table.ready(wire)){continue;}
            if g.wire_for(config,&effect.device).is_err(){continue;}
            requirements
        } else {vec![]};
        let Some((binding, module)) = config.binding(&effect.device) else {
            continue;
        };
        let command = Command::PanelLed(effect.active);
        if hb_protocol::encode_in_context(module, binding.channel, command, context).is_err() {
            continue;
        }
        let Some(end) = next_id.checked_add(1) else {
            continue;
        };
        let Some(deadline_ms) = now.checked_add(ttl) else {
            continue;
        };
        let id = *next_id;
        let command = QueuedCommand {
            id,
            device_id: effect.device,
            command,
            origin: Origin::Rule,
            epochs: arbiter.policy().epochs,
            deadline_ms,
            run_id: Some(effect.cause),
        };
        let access = Access {
            authorized: true,
            device_exists: true,
            device_verified: matches!(
                module.verification(),
                homesense_core::Verification::Verified(_)
            ),
            existing_direct_manual: false,
        };
        let gateway_scope=gateways.map(|g|g.scope(config,std::slice::from_ref(&command),&BTreeMap::new(),&requirements,vec![]))
            .transpose().map_err(|_|homesense_storage::Error::InvalidAudit)?;
        match arbiter.submit_batch_committed(vec![(command, access)], now, |commands, cancelled| {
            match &gateway_scope {
                Some(scope)=>store.accept_queue_batch_scoped(boot,commands,cancelled,now,None,&[],None,None,&[],&[],None,&[],scope),
                None=>store.accept_queue_batch(boot, commands, cancelled, now),
            }
        }) {
            Ok(cancelled) => {
                principals.insert(id, effect.controller);
                if let Some(source)=effect.source_authorization {sources.insert(id,source);}
                *next_id = end;
                for c in cancelled {
                    principals.remove(&c.command.id);
                }
            }
            Err(BatchCommitError::Persistence(e)) => return Err(e),
            Err(BatchCommitError::Admission(_)) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> serde_json::Value {
        serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"26","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"panel-module","address":"4A","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"stop_led_idle_ms":200,"evidence":"synthetic review","reviewed_at_unix_ms":1,"leds":{"open":["open-led"],"close":["close-led"],"stop":["stop-led"]}}},{"id":"open-led","name":"Open LED","module_id":"panel-module","channel":2,"enabled":true},{"id":"stop-led","name":"Stop LED","module_id":"panel-module","channel":4,"enabled":true},{"id":"close-led","name":"Close LED","module_id":"panel-module","channel":6,"enabled":true}]})
    }

    fn setup() -> (homesense_config::Configuration, Owner, Scope) {
        let config = homesense_config::Configuration::parse(
            &serde_json::to_vec(&document()).unwrap(),
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        let scope = Scope {
            boot: 1,
            epochs: homesense_core::queue::Epochs {
                configuration: 1,
                transport: 1,
                control: 1,
            },
        };
        let owner = Owner::new(&config, scope, 0);
        (config, owner, scope)
    }
    fn accept(
        config: &homesense_config::Configuration,
        owner: &mut Owner,
        scope: Scope,
        id: u64,
        action: MotorAction,
        controller: Uuid,
        at: u64,
    ) {
        let device = DeviceId::new("curtain").unwrap();
        owner.accepted(
            config,
            &QueuedCommand {
                id,
                device_id: device.clone(),
                command: config.motor_command(&device, action).unwrap(),
                origin: homesense_core::queue::Origin::Hap,
                epochs: scope.epochs,
                deadline_ms: 10000,
                run_id: None,
            },
            controller,
            at,
        );
    }
    #[test]
    fn one_owner_cross_principal_last_written_and_old_endpoint_cannot_revive() {
        let (config, mut owner, scope) = setup();
        let device = DeviceId::new("curtain").unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        accept(&config, &mut owner, scope, 1, MotorAction::Open, a, 0);
        owner.written(1, 10);
        accept(&config, &mut owner, scope, 2, MotorAction::Close, b, 100);
        owner.written(2, 110);
        let view = owner.view_for_test(&device, 1111);
        assert_eq!(view.position.as_deref(), Some("Closed"));
        assert_eq!(view.last_written_id, Some(2));
        assert_eq!(
            owner.view_for_test(&device, 3000).position.as_deref(),
            Some("Closed")
        );
        assert!(
            owner
                .effects(3000, &|_| true)
                .iter()
                .all(|e| e.controller == b && e.cause == 2)
        );
    }
    #[test]
    fn actual_order_when_two_actions_accepted_before_first_written_holds_motion_until_second_written()
     {
        let (config, mut owner, scope) = setup();
        let device = DeviceId::new("curtain").unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        accept(&config, &mut owner, scope, 1, MotorAction::Open, a, 0);
        accept(&config, &mut owner, scope, 2, MotorAction::Stop, b, 1);
        owner.written(1, 10);
        let queued = owner.snapshot(&device, 1011).unwrap();
        assert_eq!(queued.pending, Some(MotorAction::Stop));
        assert_eq!(queued.inferred_movement.unwrap().value, Movement::Opening);
        assert!(queued.stop_led_active.unwrap().value);
        owner.written(2, 1020);
        let stopped = owner.snapshot(&device, 1219).unwrap();
        assert!(stopped.stop_led_active.unwrap().value);
        assert!(
            !owner
                .snapshot(&device, 1220)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
    }
    #[test]
    fn indicator_edges_do_not_retry_and_fault_epoch_and_revocation_never_fake_green() {
        let (config, mut owner, scope) = setup();
        let device = DeviceId::new("curtain").unwrap();
        let a = Uuid::new_v4();
        accept(&config, &mut owner, scope, 1, MotorAction::Open, a, 0);
        owner.written(1, 10);
        let effects = owner.effects(11, &|_| true);
        assert_eq!(effects.len(), 3);
        assert!(
            effects
                .iter()
                .find(|e| e.device.as_str() == "stop-led")
                .unwrap()
                .active
        );
        assert!(owner.effects(12, &|_| true).is_empty());
        assert_eq!(owner.next_deadline(12), Some(1010));
        let terminal = owner.effects(1010, &|_| true);
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].device.as_str(), "close-led");
        assert_eq!(owner.next_deadline(1010), Some(1210));
        let idle = owner.effects(1210, &|_| true);
        assert_eq!(idle.len(), 1);
        assert!(!idle[0].active);
        assert!(owner.effects(1211, &|_| true).is_empty());
        accept(&config, &mut owner, scope, 2, MotorAction::Close, a, 1300);
        owner.written(2, 1301);
        assert!(owner.effects(1302, &|_| false).is_empty());
        assert_eq!(owner.snapshot(&device, 1303).unwrap().last_written, None);
        let mut changed = scope;
        changed.epochs.transport = 2;
        owner.synchronize(&config, changed, 1400, true);
        assert_eq!(
            owner.snapshot(&device, 1401).unwrap().inferred_movement,
            None
        );
        assert!(owner.effects(1402, &|_| true).is_empty());
    }
    #[test]
    fn stopped_cancels_only_queued_intent_and_timing_conflict_cannot_fork_owner() {
        let (config, mut owner, scope) = setup();
        let device = DeviceId::new("curtain").unwrap();
        let a = Uuid::new_v4();
        accept(&config, &mut owner, scope, 1, MotorAction::Open, a, 0);
        owner.written(1, 10);
        accept(&config, &mut owner, scope, 2, MotorAction::Close, a, 100);
        owner.cancelled(2, CancelReason::Stopped);
        accept(&config, &mut owner, scope, 3, MotorAction::Stop, a, 101);
        assert!(
            owner
                .snapshot(&device, 102)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
        assert!(
            owner
                .candidate(device.clone(), Timing::default(), 103)
                .is_err()
        );
        owner.written(3, 104);
        assert_eq!(
            owner
                .snapshot(&device, 105)
                .unwrap()
                .last_written
                .unwrap()
                .id,
            3
        );
        owner.invalidate_device(&device);
        assert_eq!(owner.snapshot(&device, 110).unwrap().last_written, None);
        assert!(owner.effects(111, &|_| true).is_empty());
    }
    #[test]
    fn revoking_old_writer_preserves_another_principals_pending_stop_receipt() {
        let (config, mut owner, scope) = setup();
        let device = DeviceId::new("curtain").unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        accept(&config, &mut owner, scope, 1, MotorAction::Open, a, 0);
        owner.written(1, 10);
        accept(&config, &mut owner, scope, 2, MotorAction::Stop, b, 11);
        assert!(owner.effects(12, &|id| id == b).is_empty());
        assert_eq!(owner.snapshot(&device, 13).unwrap().last_written, None);
        assert_eq!(
            owner.snapshot(&device, 13).unwrap().pending,
            Some(MotorAction::Stop)
        );
        owner.written(2, 20);
        let snapshot = owner.snapshot(&device, 21).unwrap();
        assert_eq!(snapshot.last_written.unwrap().id, 2);
        assert_eq!(snapshot.inferred_movement.unwrap().value, Movement::Stopped);
        assert!(snapshot.stop_led_active.unwrap().value);
        assert!(
            owner
                .effects(22, &|id| id == b)
                .iter()
                .all(|e| e.controller == b && e.cause == 2)
        );
        assert!(
            !owner
                .snapshot(&device, 220)
                .unwrap()
                .stop_led_active
                .unwrap()
                .value
        );
    }
    #[test]
    fn delayed_direction_hints_keep_opposite_untouched_and_schedule_without_reads() {
        let mut value=document();
        value["devices"][0]["curtain_state"]["open_travel_ms"]=serde_json::json!(10000);
        value["devices"][0]["curtain_state"]["close_travel_ms"]=serde_json::json!(10000);
        value["devices"][0]["curtain_state"]["stop_led_idle_ms"]=serde_json::json!(1);
        value["devices"][0]["curtain_state"]["direction_led_policy"]=serde_json::json!({"kind":"written_direction_delay","opposite_red_after_ms":2000});
        let config=homesense_config::Configuration::parse(&serde_json::to_vec(&value).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let (_,_,scope)=setup();let mut owner=Owner::new(&config,scope,0);let controller=Uuid::new_v4();
        assert!(owner.effects(0,&|_|true).is_empty());
        accept(&config,&mut owner,scope,1,MotorAction::Open,controller,0);
        assert_eq!(owner.next_deadline(1),None);assert!(owner.effects(1,&|_|true).is_empty());
        owner.written(1,10);
        let effects=owner.effects(10,&|_|true);
        assert_eq!(effects.iter().map(|e|(e.device.as_str(),e.active)).collect::<Vec<_>>(),vec![("open-led",true),("stop-led",true)]);
        assert_eq!(owner.next_deadline(10),Some(2010));
        assert!(owner.effects(2009,&|_|true).is_empty());
        let effects=owner.effects(2010,&|_|true);
        assert_eq!(effects.iter().map(|e|(e.device.as_str(),e.active)).collect::<Vec<_>>(),vec![("close-led",true)]);
        let old_hint=QueuedCommand{id:50,device_id:DeviceId::new("open-led").unwrap(),command:Command::PanelLed(true),origin:homesense_core::queue::Origin::Rule,epochs:scope.epochs,deadline_ms:20000,run_id:Some(1)};
        assert!(owner.permits_queued_hint(&old_hint,2010));
        assert_eq!(owner.next_deadline(2010),Some(10010));
        let end=owner.effects(10010,&|_|true);
        assert_eq!(end.iter().map(|e|(e.device.as_str(),e.active)).collect::<Vec<_>>(),vec![("open-led",false)]);
        assert!(!owner.permits_queued_hint(&old_hint,10010),"queued old red cannot overwrite endpoint green");
        assert_eq!(owner.next_deadline(10010),Some(10011));
        assert_eq!(owner.effects(10011,&|_|true).iter().map(|e|(e.device.as_str(),e.active)).collect::<Vec<_>>(),vec![("stop-led",false)]);
        assert_eq!(owner.next_deadline(10011),None);
    }
    #[test]
    fn pending_reverse_stop_revocation_cancel_old_direction_hint_edge() {
        let mut value=document();
        value["devices"][0]["curtain_state"]["open_travel_ms"]=serde_json::json!(10000);
        value["devices"][0]["curtain_state"]["close_travel_ms"]=serde_json::json!(10000);
        value["devices"][0]["curtain_state"]["stop_led_idle_ms"]=serde_json::json!(1);
        value["devices"][0]["curtain_state"]["direction_led_policy"]=serde_json::json!({"kind":"written_direction_delay","opposite_red_after_ms":2000});
        let config=homesense_config::Configuration::parse(&serde_json::to_vec(&value).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let (_,_,scope)=setup();let mut owner=Owner::new(&config,scope,0);let controller=Uuid::new_v4();
        accept(&config,&mut owner,scope,1,MotorAction::Open,controller,0);owner.written(1,10);owner.effects(10,&|_|true);
        accept(&config,&mut owner,scope,2,MotorAction::Close,controller,1000);
        assert!(owner.effects(2010,&|_|true).is_empty(),"old opposite edge frozen while next motor is only Accepted");
        assert_eq!(owner.next_deadline(2010),None);
        owner.written(2,2100);owner.effects(2100,&|_|true);assert_eq!(owner.next_deadline(2100),Some(4100));
        accept(&config,&mut owner,scope,3,MotorAction::Stop,controller,2200);owner.written(3,2201);owner.effects(2201,&|_|true);
        assert_eq!(owner.next_deadline(2201),Some(2202));owner.effects(2202,&|_|true);
        assert!(owner.effects(4100,&|_|true).is_empty());assert_eq!(owner.next_deadline(4100),None);
        accept(&config,&mut owner,scope,4,MotorAction::Open,controller,4200);owner.written(4,4201);owner.effects(4201,&|_|true);
        assert!(owner.effects(6201,&|_|false).is_empty());assert_eq!(owner.next_deadline(6201),None);
    }
    #[test]
    fn new_written_cause_reissues_hints_without_assuming_old_attempted_color() {
        let mut value=document();value["devices"][0]["curtain_state"]["open_travel_ms"]=serde_json::json!(10000);
        value["devices"][0]["curtain_state"]["direction_led_policy"]=serde_json::json!({"kind":"written_direction_delay","opposite_red_after_ms":2000});
        let config=homesense_config::Configuration::parse(&serde_json::to_vec(&value).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let (_,_,scope)=setup();let mut owner=Owner::new(&config,scope,0);let controller=Uuid::new_v4();
        accept(&config,&mut owner,scope,1,MotorAction::Open,controller,0);owner.written(1,10);owner.effects(10,&|_|true);owner.effects(2010,&|_|true);
        accept(&config,&mut owner,scope,2,MotorAction::Open,controller,2100);owner.written(2,2110);
        let immediate=owner.effects(2110,&|_|true);
        assert_eq!(immediate.iter().map(|e|(e.device.as_str(),e.active,e.cause)).collect::<Vec<_>>(),vec![("open-led",true,2),("stop-led",true,2)]);
        assert!(owner.effects(4109,&|_|true).is_empty());
        let delayed=owner.effects(4110,&|_|true);
        assert_eq!(delayed.iter().map(|e|(e.device.as_str(),e.active,e.cause)).collect::<Vec<_>>(),vec![("close-led",true,2)]);
        assert!(owner.effects(4111,&|_|true).is_empty(),"same edge is never retried");
    }
    impl Owner {
        fn view_for_test(&mut self, id: &DeviceId, now: u64) -> View {
            self.snapshot(id, now).unwrap().into()
        }
    }
}
