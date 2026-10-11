//! Explicit event dispatch for reviewed immediate rules. No subscriptions or
//! schedules are installed here. Producers must supply real changes/results;
//! a root dispatch never means a physical action has completed.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
    rule_runtime::PreparedRule,
};
use homesense_core::{
    DeviceId,
    rule::{Budget, Node},
    rule_chain::{Cause, Chain},
    scene_expand::{self, Definition},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    StartupSynchronized,
    VariableChanged(String),
    DeviceChanged(DeviceId),
    SceneResult(String),
    Schedule(String),
}
pub struct Binding {
    pub id: String,
    pub trigger: Trigger,
    pub variables: Vec<String>,
    pub actions: Vec<Node>,
}
#[derive(Debug)]
pub struct Completed {
    pub rule: String,
    pub cause: Cause,
    pub causation_id: u64,
    pub commands: Vec<u64>,
    pub changed_variables: Vec<String>,
    pub unknown_branches: Vec<String>,
}
#[derive(Debug)]
pub struct Report {
    pub correlation_id: u128,
    pub completed: Vec<Completed>,
    pub failure: Option<String>,
}
pub struct EventRules {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    bindings: Vec<Binding>,
    scenes: BTreeMap<String, Definition>,
    budget: Budget,
    feedback_cursor: Option<homesense_state::Cursor>,
    stopped: bool,
    cache_bindings: BTreeMap<String, homesense_config::rule_document::FeedbackBinding>,
    cache_states: BTreeMap<DeviceId, homesense_state::DeviceState>,
}
impl EventRules {
    /// Build the non-panel router from the strict document. Panel triggers are
    /// rejected here, not dropped; they require the gesture-owning panel adapter.
    pub async fn from_document(
        admission: Admission,
        controller: Uuid,
        document: &homesense_config::rule_document::Document,
        scenes: BTreeMap<String, Definition>,
        schedules: BTreeSet<String>,
        budget: Budget,
    ) -> Result<Self, String> {
        use homesense_config::rule_document::Trigger as Input;
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if generation.epochs.configuration != document.revision {
            return Err("rule document revision expired".into());
        }
        let linked = document
            .linked_rules(&scenes, &schedules)
            .map_err(|e| e.to_string())?;
        let mut bindings = vec![];
        for rule in linked {
            let trigger = match rule.trigger {
                Input::StartupSynchronized => Trigger::StartupSynchronized,
                Input::VariableChanged { variable } => Trigger::VariableChanged(variable),
                Input::DeviceChanged { device } => {
                    Trigger::DeviceChanged(DeviceId::new(device).map_err(|e| e.to_string())?)
                }
                Input::SceneResult { scene } => Trigger::SceneResult(scene),
                Input::Schedule { schedule } => Trigger::Schedule(schedule),
                Input::Panel { .. } => return Err("panel rule requires gesture adapter".into()),
            };
            let names =
                homesense_core::rule::variable_references(&rule.actions).map_err(str::to_owned)?;
            for name in &names {
                if document.variables[name].lifetime
                    == homesense_core::rule::Lifetime::FeedbackCache
                {
                    return Err(format!("{name}: runtime scope not yet supported"));
                }
            }
            let names: Vec<_> = names.into_iter().collect();
            let stored = admission
                .variable_snapshot(controller, generation.clone(), names.clone())
                .await
                .map_err(|e| format!("declared variable snapshot: {e:?}"))?;
            for (name, value) in stored {
                if value.kind != document.variables[&name].value_type
                    || value.lifetime != document.variables[&name].lifetime
                {
                    return Err(format!("{name}: stored variable type mismatch"));
                }
            }
            bindings.push(Binding {
                id: rule.id,
                trigger,
                variables: names,
                actions: rule.actions,
            });
        }
        let mut runtime =
            Self::new(admission, controller, bindings, Default::default(), budget).await?;
        if runtime.generation != generation {
            return Err("rule document generation changed during load".into());
        }
        runtime.cache_bindings = document.feedback_bindings.clone();
        Ok(runtime)
    }

    /// Construction is explicit execution authorization by the caller. Only
    /// reviewed enabled bindings may be passed; importing a rule does not do so.
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        bindings: Vec<Binding>,
        scenes: BTreeMap<String, Definition>,
        budget: Budget,
    ) -> Result<Self, String> {
        if bindings.len() > 256 {
            return Err("event rule capacity exceeded".into());
        }
        let mut ids = BTreeSet::new();
        for b in &bindings {
            if b.id.is_empty() || !ids.insert(b.id.clone()) {
                return Err("invalid or duplicate event rule id".into());
            }
            match &b.trigger {
                Trigger::VariableChanged(id) | Trigger::SceneResult(id) | Trigger::Schedule(id)
                    if id.is_empty() =>
                {
                    return Err("empty trigger reference".into());
                }
                _ => {}
            }
            scene_expand::expand(&b.actions, &scenes, 1024)
                .map_err(|e| format!("{}: {e:?}", b.id))?;
        }
        Chain::new(1, 0, 5000, budget, 32).map_err(|e| format!("{e:?}"))?;
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            admission,
            controller,
            generation,
            bindings,
            scenes,
            budget,
            feedback_cursor: None,
            stopped: false,
            cache_bindings: BTreeMap::new(),
            cache_states: BTreeMap::new(),
        })
    }
    fn validate_feedback(&self, feedback: &Feedback) -> Result<(), String> {
        if self.stopped
            || !feedback.is_active()
            || feedback.snapshot().cursor.boot_id
                != Uuid::parse_str(self.admission.boot_id())
                    .map_err(|e| e.to_string())?
                    .as_u128()
            || feedback.configuration_revision() != self.generation.epochs.configuration
            || feedback.transport_epoch() != self.generation.epochs.transport
        {
            return Err("event feedback scope invalidated".into());
        }
        Ok(())
    }
    /// Begin at the current cursor: startup snapshots are not historical change events.
    pub fn bind_feedback(&mut self, feedback: &Feedback) -> Result<(), String> {
        self.validate_feedback(feedback)?;
        if self.feedback_cursor.is_some() {
            return Err("event feedback already bound".into());
        }
        let snapshot = feedback.snapshot();
        self.feedback_cursor = Some(snapshot.cursor);
        self.cache_states = snapshot.devices;
        Ok(())
    }
    /// Read only actual switch/level changes. Duplicate polls produce no event.
    /// Validate the entire batch before dispatch; a gap or stale batch invalidates
    /// this subscription rather than replaying incomplete history.
    async fn feedback_batches(
        &mut self, feedback:&Feedback, max_age:u64,
    )->Result<Vec<Vec<Trigger>>,String> {
        let batch = async {
            self.validate_feedback(feedback)?;
            if max_age == 0
                || self
                    .admission
                    .task_generation()
                    .await
                    .map_err(|e| format!("{e:?}"))?
                    != self.generation
            {
                return Err("event feedback generation expired".into());
            }
            let events =
                feedback.events_since(self.feedback_cursor.ok_or("event feedback not bound")?)?;
            let now = self.admission.monotonic_ms();
            let mut states = self.cache_states.clone();
            let mut batches = vec![];
            for event in &events {
                let observation = event.change == homesense_state::Change::Observation
                    && event.state.quality == homesense_state::Quality::Confirmed
                    && matches!(
                        event.state.observed,
                        Some(
                            homesense_core::Observation::Switch(_)
                                | homesense_core::Observation::Level(_)
                        )
                    );
                if observation {
                    let at = event
                        .state
                        .observed_at_ms
                        .ok_or("device event missing time")?;
                    if now.checked_sub(at).is_none_or(|age| age >= max_age) {
                        return Err("stale device event".into());
                    }
                }
                let affected = feedback.affected_devices(&event.device_id);
                let previous: BTreeMap<_, _> = affected
                    .iter()
                    .map(|id| (id.clone(), logical_state(feedback, &states, id)))
                    .collect();
                states.insert(event.device_id.clone(), event.state.clone());
                let mut triggers = vec![];
                for id in affected {
                    use homesense_config::rule_document::FeedbackBinding;
                    let before = previous[&id].as_ref();
                    let after = logical_state(feedback, &states, &id);
                    let before_time = before.and_then(|s| s.observed_at_ms).unwrap_or(now);
                    let on = FeedbackBinding::On {
                        device: id.as_str().into(),
                    };
                    let level = FeedbackBinding::Level {
                        device: id.as_str().into(),
                    };
                    let changed = [&on, &level].iter().any(|binding| {
                        cache_value(before, binding, before_time, max_age)
                            != cache_value(after.as_ref(), binding, now, max_age)
                    });
                    let known = cache_value(after.as_ref(), &on, now, max_age).is_some();
                    if observation && known && (feedback.supply_relay(&id).is_none() || changed) {
                        triggers.push(Trigger::DeviceChanged(id.clone()));
                    }
                    for (name, binding) in &self.cache_bindings {
                        let source = match binding {
                            FeedbackBinding::On { device } | FeedbackBinding::Level { device } => {
                                device
                            }
                        };
                        if source == id.as_str()
                            && cache_value(before, binding, before_time, max_age)
                                != cache_value(after.as_ref(), binding, now, max_age)
                        {
                            triggers.push(Trigger::VariableChanged(name.clone()));
                        }
                    }
                }
                if !triggers.is_empty() {
                    batches.push(triggers);
                }
            }
            Ok((events.last().map(|e| e.cursor), batches, states))
        }
        .await;
        let (cursor, batches, states) = match batch {
            Ok(batch) => batch,
            Err(error) => {
                self.stopped = true;
                return Err(error);
            }
        };
        if let Some(cursor) = cursor {
            self.feedback_cursor = Some(cursor);
        }
        self.cache_states = states;
        Ok(batches)
    }
    pub async fn consume_feedback(
        &mut self, feedback:&Feedback, max_age:u64,
    )->Result<Vec<Report>,String> {
        let batches=self.feedback_batches(feedback,max_age).await?;
        let mut reports = vec![];
        for triggers in batches {
            reports.push(
                self.dispatch_batch(triggers, Some((feedback, max_age)))
                    .await,
            );
        }
        Ok(reports)
    }
    /// Each independent producer event starts a fresh chain. Variable changes
    /// produced inside this dispatch retain their parent cause and shared budget.
    /// Failed or ambiguous commits are never retried or emitted as child events.
    pub async fn dispatch(&mut self, event: Trigger, feedback: Option<(&Feedback, u64)>) -> Report {
        self.dispatch_batch(vec![event], feedback).await
    }
    async fn dispatch_batch(
        &mut self,
        events: Vec<Trigger>,
        feedback: Option<(&Feedback, u64)>,
    ) -> Report {
        let correlation_id = Uuid::new_v4().as_u128();
        let mut report = Report {
            correlation_id,
            completed: vec![],
            failure: None,
        };
        let outcome = self.dispatch_inner(events, feedback, &mut report).await;
        if let Err(e) = outcome {
            report.failure = Some(e);
        }
        if let Err(error) = self
            .admission
            .record_rule_outcome(
                self.controller,
                self.generation.clone(),
                correlation_id,
                report.failure.clone(),
            )
            .await
        {
            self.stopped = true;
            report.failure = Some(format!(
                "{}; outcome persistence: {error:?}",
                report
                    .failure
                    .unwrap_or_else(|| "dispatch completed".into())
            ));
        }
        report
    }
    async fn dispatch_inner(
        &self,
        events: Vec<Trigger>,
        feedback: Option<(&Feedback, u64)>,
        report: &mut Report,
    ) -> Result<(), String> {
        if self.stopped {
            return Err("event router stopped".into());
        }
        if self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?
            != self.generation
        {
            return Err("event router generation expired".into());
        }
        let mut chain = Chain::new(
            report.correlation_id,
            self.admission.monotonic_ms(),
            5000,
            self.budget,
            32,
        )
        .map_err(|e| format!("{e:?}"))?;
        let root = chain.root();
        let mut pending: VecDeque<_> = events
            .into_iter()
            .map(|event| (event, root.clone()))
            .collect();
        while let Some((event, parent)) = pending.pop_front() {
            for binding in self.bindings.iter().filter(|b| b.trigger == event) {
                let nodes = scene_expand::expand(&binding.actions, &self.scenes, 1024)
                    .map_err(|e| format!("{}: {e:?}", binding.id))?;
                let mut identity = None;
                let mut lease = None;
                let mut prepared = PreparedRule::prepare_using(
                    &self.admission,
                    self.controller,
                    self.generation.clone(),
                    binding.variables.clone(),
                    |variables| {
                        let planned = chain
                            .plan_using(
                                &parent,
                                &binding.id,
                                self.admission.monotonic_ms(),
                                |budget| {
                                    if let Some((feedback, age)) = feedback {
                                        let (plan, read) = feedback
                                            .plan_rule(
                                                &nodes,
                                                variables,
                                                budget,
                                                self.admission.monotonic_ms(),
                                                age,
                                            )
                                            .map_err(homesense_core::rule_chain::Error::Planning)?;
                                        lease = Some(read);
                                        Ok(plan)
                                    } else {
                                        homesense_core::rule::evaluate(&nodes, variables, budget)
                                            .map_err(homesense_core::rule_chain::Error::Rule)
                                    }
                                },
                            )
                            .map_err(|e| format!("{}: {e:?}", binding.id))?;
                        identity = Some((planned.cause, planned.causation_id));
                        Ok(planned.plan)
                    },
                )
                .await?;
                prepared.attach_feedback(lease);
                let changed_variables = prepared.changed_variables.clone();
                let unknown_branches = prepared.unknown_branches.clone();
                let (cause, causation_id) = identity.ok_or("missing planned cause")?;
                prepared.attach_cause(&binding.id, &cause, causation_id);
                let commands = prepared.commit(&self.admission).await?;
                report.completed.push(Completed {
                    rule: binding.id.clone(),
                    cause: cause.clone(),
                    causation_id,
                    commands,
                    changed_variables: changed_variables.clone(),
                    unknown_branches,
                });
                // Reserve capacity only after preserving the successful commit.
                if pending.len() + changed_variables.len() > 256 {
                    return Err("event queue capacity exceeded after commit".into());
                }
                for name in changed_variables {
                    pending.push_back((Trigger::VariableChanged(name), cause.clone()));
                }
            }
        }
        Ok(())
    }
}

/// Calendar dispatch commits the high-water mark before any rule admission.
/// Crash recovery is at-most-once: a crash after that commit can skip an action.
/// It never silently retries a partially dispatched calendar batch.
pub struct ScheduledRules {
    rules: EventRules,
    calendar: homesense_core::calendar::Calendar,
    watermark: Option<i64>,
    startup_attempted: bool,
}
/// In-memory clock state and the checkpoint successfully acknowledged by storage.
#[derive(Debug, PartialEq, Eq)]
pub struct CalendarStatus {
    pub clock: homesense_core::calendar::Status,
    pub persisted_watermark_unix_ms: Option<i64>,
    pub startup_attempted: bool,
}
impl ScheduledRules {
    pub fn status(&self) -> CalendarStatus {
        CalendarStatus {
            clock: self.calendar.status(),
            persisted_watermark_unix_ms: self.watermark,
            startup_attempted: self.startup_attempted,
        }
    }

    pub(crate) fn matches_admission(&self, admission: &Admission) -> bool {
        self.rules.admission.boot_id() == admission.boot_id()
    }
    pub(crate) fn bind_feedback(&mut self, feedback:&Feedback)->Result<(),String> {
        self.rules.bind_feedback(feedback)
    }
    pub(crate) async fn consume_feedback_causal(
        &mut self, feedback:&Feedback, max_age:u64,
        router:&mut crate::deferred_router::Router,
        scheduler:&mut crate::scene_scheduler::SceneScheduler,
    )->Result<Vec<crate::deferred_rule::Outcome>,String> {
        if !router.matches_owner(&self.rules.admission,self.rules.controller) {
            return Err("foreign feedback causal owner".into());
        }
        let mut reports=Vec::new();
        for triggers in self.rules.feedback_batches(feedback,max_age).await? {
            let ids:Vec<_>=self.rules.bindings.iter().filter(|b|triggers.contains(&b.trigger)).map(|b|b.id.clone()).collect();
            if !ids.is_empty() {
                let mut outcomes=router.dispatch_rules(&ids,scheduler,Some((feedback,max_age)),self.rules.budget,5000).await?;
                reports.append(&mut outcomes);
            }
        }
        Ok(reports)
    }
    pub(crate) fn suppress_startup(&mut self) { self.startup_attempted = true; }
    /// One startup event per assembled synchronized runtime. Mark before await:
    /// ambiguous acceptance is never retried by another loop iteration.
    pub(crate) async fn startup_causal(
        &mut self, feedback:Option<(&Feedback,u64)>,
        router:&mut crate::deferred_router::Router,
        scheduler:&mut crate::scene_scheduler::SceneScheduler,
    )->Result<Vec<crate::deferred_rule::Outcome>,String> {
        if !router.matches_owner(&self.rules.admission,self.rules.controller) {
            return Err("foreign startup causal owner".into());
        }
        if self.startup_attempted {return Ok(vec![]);}
        self.startup_attempted=true;
        let ids:Vec<_>=self.rules.bindings.iter().filter(|b|b.trigger==Trigger::StartupSynchronized).map(|b|b.id.clone()).collect();
        if ids.is_empty() {return Ok(vec![]);}
        let outcomes=router.dispatch_rules(&ids,scheduler,feedback,self.rules.budget,5000).await?;
        Ok(outcomes)
    }
    pub(crate) async fn scene_result_causal(
        &mut self, result: crate::scene_scheduler::SceneResult,
        feedback: Option<(&Feedback,u64)>, router:&mut crate::deferred_router::Router,
        scheduler:&mut crate::scene_scheduler::SceneScheduler,
    )->Result<Vec<crate::deferred_rule::Outcome>,String> {
        use homesense_core::scene::Status;
        use homesense_storage::scene_run::{Query,Status as Stored};
        if !router.matches_owner(&self.rules.admission,self.rules.controller) {
            return Err("foreign scene causal owner".into());
        }
        let expected=match result.status {
            Status::CommandsWritten=>Stored::CommandsWritten,
            Status::Partial=>Stored::Partial,
            Status::Cancelled=>Stored::Cancelled,
            Status::Failed=>Stored::Failed,
            _=>return Err("nonterminal manual scene result".into()),
        };
        let page=self.rules.admission.scene_history(self.rules.controller,self.rules.generation.clone(),Query::Run {
            boot:self.rules.admission.boot_id().into(),run:result.audit_id.clone(),after:0,limit:1,
        }).await.map_err(|e|format!("{e:?}"))?.ok_or("manual scene receipt missing")?;
        if page.cause.is_some() || page.record.status!=expected || page.record.definition.as_deref()!=Some(result.definition.as_str()) {
            return Err("manual scene receipt mismatch or orphaned causal result".into());
        }
        let ids:Vec<_>=self.rules.bindings.iter().filter(|b|b.trigger==Trigger::SceneResult(result.definition.clone())).map(|b|b.id.clone()).collect();
        if ids.is_empty() {return Ok(vec![]);}
        let outcomes=router.dispatch_rules(&ids,scheduler,feedback,self.rules.budget,5000).await?;
        Ok(outcomes)
    }
    pub(crate) async fn scene_result(
        &mut self,
        result: crate::scene_scheduler::SceneResult,
        feedback: Option<(&Feedback, u64)>,
    ) -> Report {
        log::info!(
            "scene_result run={} definition={} audit={} status={:?}",
            result.run,
            result.definition,
            result.audit_id,
            result.status
        );
        self.rules
            .dispatch(Trigger::SceneResult(result.definition), feedback)
            .await
    }
    pub fn stop(&mut self) {
        self.startup_attempted = true;
        self.calendar.stop();
    }

    pub async fn new(
        rules: EventRules,
        schedules: Vec<homesense_core::calendar::Schedule>,
        wall_unix_ms: i64,
        lateness_ms: u32,
    ) -> Result<Self, String> {
        let watermark = rules
            .admission
            .calendar_watermark(rules.controller, rules.generation.clone(), None)
            .await
            .map_err(|e| format!("calendar load: {e:?}"))?;
        let calendar = homesense_core::calendar::Calendar::new(
            schedules,
            wall_unix_ms,
            rules.admission.monotonic_ms(),
            watermark,
            lateness_ms,
        )
        .map_err(|e| format!("calendar: {e:?}"))?;
        Ok(Self {
            rules,
            calendar,
            watermark,
            startup_attempted: false,
        })
    }
    async fn occurrences(&mut self, wall_unix_ms:i64)->Result<Vec<String>,String> {
        let poll = self
            .calendar
            .poll(wall_unix_ms, self.rules.admission.monotonic_ms())
            .map_err(|e| format!("calendar: {e:?}"))?;
        let persisted = self
            .rules
            .admission
            .calendar_watermark(
                self.rules.controller,
                self.rules.generation.clone(),
                Some(homesense_storage::calendar::Checkpoint {
                    expected: self.watermark,
                    next: poll.watermark_unix_ms,
                }),
            )
            .await;
        match persisted {
            Ok(value) => self.watermark = value,
            Err(error) => {
                self.calendar.stop();
                return Err(format!("calendar checkpoint: {error:?}"));
            }
        }
        Ok(poll.occurrences.into_iter().map(|o|o.schedule).collect())
    }
    pub async fn poll_causal(
        &mut self, wall_unix_ms:i64, feedback:Option<(&Feedback,u64)>,
        router:&mut crate::deferred_router::Router,
        scheduler:&mut crate::scene_scheduler::SceneScheduler,
    )->Result<Vec<crate::deferred_rule::Outcome>,String> {
        if !router.matches_owner(&self.rules.admission,self.rules.controller) {
            return Err("foreign calendar causal owner".into());
        }
        let mut reports=Vec::new();
        for schedule in self.occurrences(wall_unix_ms).await? {
            let ids:Vec<_>=self.rules.bindings.iter().filter(|b|b.trigger==Trigger::Schedule(schedule.clone())).map(|b|b.id.clone()).collect();
            if !ids.is_empty() {
                let mut outcomes=router.dispatch_rules(&ids,scheduler,feedback,self.rules.budget,5000).await?;
                reports.append(&mut outcomes);
            }
        }
        Ok(reports)
    }
    pub async fn poll(
        &mut self, wall_unix_ms:i64, feedback:Option<(&Feedback,u64)>,
    )->Result<Vec<Report>,String> {
        let mut reports=vec![];
        for schedule in self.occurrences(wall_unix_ms).await? {
            reports.push(self.rules.dispatch(Trigger::Schedule(schedule),feedback).await);
        }
        Ok(reports)
    }

}

fn cache_value(
    state: Option<&homesense_state::DeviceState>,
    binding: &homesense_config::rule_document::FeedbackBinding,
    now: u64,
    max_age: u64,
) -> Option<homesense_core::rule::Value> {
    use homesense_config::rule_document::FeedbackBinding;
    use homesense_core::{Observation, rule::Value};
    let state = state?;
    if state.quality != homesense_state::Quality::Confirmed
        || state.pending_command.is_some()
        || now
            .checked_sub(state.observed_at_ms?)
            .is_none_or(|age| age >= max_age)
    {
        return None;
    }
    match (binding, state.observed?) {
        (FeedbackBinding::On { .. }, Observation::Switch(value)) => Some(Value::Bool(value)),
        (FeedbackBinding::On { .. }, Observation::Level(value)) => {
            Some(Value::Bool(value.get() > 0))
        }
        (FeedbackBinding::Level { .. }, Observation::Level(value)) => {
            Some(Value::Integer(i64::from(value.get())))
        }
        _ => None,
    }
}

// Reconstruct each event's logical state from its two physical components.
// Keep physical states in the cursor cache so a relay-only event can update it.
fn logical_state(
    feedback: &Feedback,
    states: &BTreeMap<DeviceId, homesense_state::DeviceState>,
    id: &DeviceId,
) -> Option<homesense_state::DeviceState> {
    use homesense_core::{Observation, Percent};
    use homesense_state::Quality;
    let mut state = states.get(id)?.clone();
    if let Some(relay) = feedback.supply_relay(id) {
        let relay = states.get(relay)?;
        state.observed_at_ms = state
            .observed_at_ms
            .zip(relay.observed_at_ms)
            .map(|(a, b)| a.min(b));
        state.pending_command = state.pending_command.or(relay.pending_command);
        if relay.quality != Quality::Confirmed {
            state.quality = relay.quality;
        }
        state.observed = match (state.observed, relay.observed) {
            (Some(Observation::Level(level)), Some(Observation::Switch(on))) => {
                Some(Observation::Level(if on {
                    level
                } else {
                    Percent::new(0).unwrap()
                }))
            }
            _ => None,
        };
    }
    Some(state)
}
