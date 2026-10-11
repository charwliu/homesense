//! Explicitly driven panel rules. This component opens no socket and installs no
//! subscriptions. Live service must opt in after control ownership is configured.
use crate::{
    admission::{Admission, TaskGeneration},
    rule_runtime::PreparedRule,
};
use homesense_core::{
    DeviceId,
    gesture::{Event, KeyGesture, Settings},
    rule::{Budget, Node},
    scene_expand::Definition,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Observe,
    Execute,
}
pub struct Binding {
    pub id: String,
    pub event: Event,
    pub variables: Vec<String>,
    pub actions: Vec<Node>,
}
#[derive(Clone)]
pub struct HoldBinding {
    pub device: DeviceId,
    pub up: bool,
    pub release: crate::hold_dimming::ReleasePolicy,
}
pub struct Key {
    pub device: DeviceId,
    pub settings: Settings,
    pub bindings: Vec<Binding>,
}
#[derive(Debug)]
pub struct Fired {
    pub rule: String,
    pub cause: Option<homesense_core::rule_chain::Cause>,
    pub causation_id: Option<u64>,
    pub event: Event,
    pub commands: Vec<u64>,
    pub observed_only: bool,
}
/// Earlier admissions remain effective if a later rule fails. The failing
/// admission may be ambiguous; do not automatically replay this event.
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub completed: Vec<Fired>,
    pub failed_rule: Option<String>,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            completed: vec![],
            failed_rule: None,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Failure {}

struct Registered {
    gesture: KeyGesture,
    bindings: Vec<Binding>,
}
pub struct PanelRules {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    mode: Mode,
    keys: BTreeMap<DeviceId, Registered>,
    scenes: BTreeMap<String, Definition>,
    stopped: bool,
    feedback_cursor: Option<homesense_state::Cursor>,
    holds: Option<(
        crate::hold_dimming::HoldDimming,
        BTreeMap<DeviceId, HoldBinding>,
    )>,
}
impl PanelRules {
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        mode: Mode,
        keys: Vec<Key>,
        scenes: BTreeMap<String, Definition>,
        now: u64,
    ) -> Result<Self, String> {
        if keys.len() > 256 || scenes.len() > 1024 {
            return Err("panel rule capacity exceeded".into());
        }
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut registered = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for key in keys {
            if key.bindings.len() > 32
                || key.bindings.iter().any(|b| {
                    b.id.is_empty() || !ids.insert(b.id.clone()) || b.variables.len() > 1024
                })
            {
                return Err("invalid rule bindings".into());
            }
            if !key.settings.double_click_enabled
                && key.bindings.iter().any(|b| b.event == Event::DoubleClick)
            {
                return Err("double-click binding requires deferred single-click mode".into());
            }
            let value = Registered {
                gesture: KeyGesture::new(key.settings, now).map_err(|e| format!("{e:?}"))?,
                bindings: key.bindings,
            };
            if registered.insert(key.device, value).is_some() {
                return Err("duplicate panel key".into());
            }
        }
        Ok(Self {
            admission,
            controller,
            generation,
            mode,
            keys: registered,
            scenes,
            stopped: false,
            feedback_cursor: None,
            holds: None,
        })
    }
    pub fn configure_holds(
        &mut self,
        bindings: BTreeMap<DeviceId, HoldBinding>,
        now: u64,
        interval_ms: u64,
        increment: u8,
    ) -> Result<(), String> {
        if self.stopped
            || self.holds.is_some()
            || bindings.len() > 256
            || bindings.keys().any(|id| !self.keys.contains_key(id))
        {
            return Err("invalid hold bindings".into());
        }
        self.holds = Some((
            crate::hold_dimming::HoldDimming::new(
                self.admission.clone(),
                self.controller,
                now,
                interval_ms,
                increment,
            )?,
            bindings,
        ));
        Ok(())
    }
    pub async fn tick_holds(&mut self, now: u64) -> Result<Vec<u64>, String> {
        self.current().await?;
        if self.mode == Mode::Observe {
            return Ok(vec![]);
        }
        match &mut self.holds {
            Some((tasks, _)) => tasks.tick(now).await,
            None => Ok(vec![]),
        }
    }
    /// Start at the current cursor: never replay a historical button snapshot.
    pub fn bind_feedback(&mut self, feedback: &crate::feedback::Feedback) -> Result<(), String> {
        if self.stopped
            || self.feedback_cursor.is_some()
            || !feedback.is_active()
            || feedback.configuration_revision() != self.generation.epochs.configuration
            || feedback.transport_epoch() != self.generation.epochs.transport
        {
            return Err("invalid feedback subscription generation".into());
        }
        let cursor = feedback.snapshot().cursor;
        if Uuid::parse_str(self.admission.boot_id())
            .map_err(|e| e.to_string())?
            .as_u128()
            != cursor.boot_id
        {
            return Err("foreign feedback boot".into());
        }
        self.feedback_cursor = Some(cursor);
        Ok(())
    }
    pub async fn consume_feedback(
        &mut self,
        feedback: &crate::feedback::Feedback,
        now: u64,
        max_age: u64,
    ) -> Result<Vec<Fired>, Failure> {
        let result = async {
            self.current().await?;
            if !feedback.is_active()
                || feedback.configuration_revision() != self.generation.epochs.configuration
                || feedback.transport_epoch() != self.generation.epochs.transport
                || max_age == 0
            {
                return Err(Failure::from("feedback subscription invalidated"));
            }
            let cursor = self
                .feedback_cursor
                .ok_or("feedback subscription not bound")?;
            let events = feedback.events_since(cursor)?;
            // Consume this batch before executing: any failure stops this router,
            // so admitted output and remaining historical edges cannot replay.
            if let Some(last) = events.last() {
                self.feedback_cursor = Some(last.cursor);
            }
            let mut completed = vec![];
            for event in events {
                if event.change != homesense_state::Change::Button
                    || !self.keys.contains_key(&event.device_id)
                {
                    continue;
                }
                let at = event
                    .state
                    .observed_at_ms
                    .ok_or("button missing timestamp")?;
                if now.checked_sub(at).is_none_or(|age| age >= max_age) {
                    return Err(Failure {
                        message: "stale button event".into(),
                        completed,
                        failed_rule: None,
                    });
                }
                let Some(homesense_core::Observation::Button { pressed }) = event.state.observed
                else {
                    continue;
                };
                match self
                    .edge_with_feedback(&event.device_id, pressed, at, Some((feedback, max_age)))
                    .await
                {
                    Ok(items) => completed.extend(items),
                    Err(mut failure) => {
                        completed.append(&mut failure.completed);
                        failure.completed = completed;
                        return Err(failure);
                    }
                }
            }
            match self
                .advance_with_feedback(now, Some((feedback, max_age)))
                .await
            {
                Ok(items) => {
                    completed.extend(items);
                    Ok(completed)
                }
                Err(mut failure) => {
                    completed.append(&mut failure.completed);
                    failure.completed = completed;
                    Err(failure)
                }
            }
        }
        .await;
        if result.is_err() {
            self.disconnect();
        }
        result
    }
    pub fn disconnect(&mut self) {
        self.stopped = true;
        if let Some((tasks, _)) = &mut self.holds {
            tasks.invalidate();
        }
        for key in self.keys.values_mut() {
            key.gesture.disconnect();
        }
    }
    async fn current(&mut self) -> Result<(), String> {
        if self.stopped {
            return Err("panel rules stopped".into());
        }
        match self.admission.task_generation().await {
            Ok(g) if g == self.generation => Ok(()),
            _ => {
                self.disconnect();
                Err("panel rule generation unavailable or changed".into())
            }
        }
    }
    pub async fn edge(
        &mut self,
        key: &DeviceId,
        pressed: bool,
        now: u64,
    ) -> Result<Vec<Fired>, Failure> {
        self.edge_with_feedback(key, pressed, now, None).await
    }
    pub async fn edge_with_feedback(
        &mut self,
        key: &DeviceId,
        pressed: bool,
        now: u64,
        feedback: Option<(&crate::feedback::Feedback, u64)>,
    ) -> Result<Vec<Fired>, Failure> {
        self.current().await?;
        let events = self
            .keys
            .get_mut(key)
            .ok_or("unregistered panel key")?
            .gesture
            .edge(pressed, now)
            .map_err(|e| format!("{e:?}"))?;
        self.fire(key, events, feedback, now).await
    }
    pub async fn advance(&mut self, now: u64) -> Result<Vec<Fired>, Failure> {
        self.advance_with_feedback(now, None).await
    }
    pub async fn advance_with_feedback(
        &mut self,
        now: u64,
        feedback: Option<(&crate::feedback::Feedback, u64)>,
    ) -> Result<Vec<Fired>, Failure> {
        self.current().await?;
        let mut due = vec![];
        for (id, key) in &mut self.keys {
            due.push((
                id.clone(),
                key.gesture.advance(now).map_err(|e| format!("{e:?}"))?,
            ));
        }
        let mut fired = vec![];
        for (id, events) in due {
            match self.fire(&id, events, feedback, now).await {
                Ok(items) => fired.extend(items),
                Err(mut failure) => {
                    fired.append(&mut failure.completed);
                    failure.completed = fired;
                    return Err(failure);
                }
            }
        }
        Ok(fired)
    }
    async fn fire(
        &mut self,
        key: &DeviceId,
        events: Vec<Event>,
        feedback: Option<(&crate::feedback::Feedback, u64)>,
        now: u64,
    ) -> Result<Vec<Fired>, Failure> {
        let mut fired = vec![];
        for event in events {
            if self.mode == Mode::Execute
                && let Some((tasks, bindings)) = &mut self.holds
                && let Some(binding) = bindings.get(key)
            {
                let outcome: Result<Option<Vec<u64>>, String> = async {
                    Ok(match event {
                        Event::LongStart => {
                            let (feedback, max_age) =
                                feedback.ok_or("hold requires actual feedback")?;
                            tasks
                                .start_with_feedback(
                                    key.clone(),
                                    binding.device.clone(),
                                    binding.up,
                                    feedback,
                                    now,
                                    max_age,
                                    binding.release,
                                )
                                .await?;
                            Some(vec![])
                        }
                        Event::Released | Event::LongEnd | Event::Cancelled => {
                            Some(tasks.release(key).await?)
                        }
                        _ => None,
                    })
                }
                .await;
                let commands = match outcome {
                    Ok(commands) => commands,
                    Err(message) => {
                        return Err(Failure {
                            message,
                            completed: fired,
                            failed_rule: Some(format!("hold:{}", key.as_str())),
                        });
                    }
                };
                if let Some(commands) = commands {
                    fired.push(Fired {
                        rule: format!("hold:{}", key.as_str()),
                        cause: None,
                        causation_id: None,
                        event,
                        commands,
                        observed_only: false,
                    });
                }
            }
            let mut chain = homesense_core::rule_chain::Chain::new(
                Uuid::new_v4().as_u128(),
                self.admission.monotonic_ms(),
                5000,
                Budget::default(),
                32,
            )
            .map_err(|e| format!("chain: {e:?}"))?;
            let root = chain.root();
            for binding in &self.keys[key].bindings {
                if binding.event != event {
                    continue;
                }
                let mut cause = None;
                let mut causation_id = None;
                let outcome: Result<Vec<u64>, String> = async {
                    let commands = if self.mode == Mode::Observe {
                        vec![]
                    } else {
                        let expanded = homesense_core::scene_expand::expand(
                            &binding.actions,
                            &self.scenes,
                            1024,
                        )
                        .map_err(|e| format!("rule {}: {e:?}", binding.id))?;
                        let mut lease = None;
                        let mut prepared = PreparedRule::prepare_using(
                            &self.admission,
                            self.controller,
                            self.generation.clone(),
                            binding.variables.clone(),
                            |variables| {
                                let run = chain
                                    .plan_using(
                                        &root,
                                        &binding.id,
                                        self.admission.monotonic_ms(),
                                        |budget| {
                                            if let Some((feedback, max_age)) = feedback {
                                                let (plan, read) = feedback
                                                    .plan_rule(
                                                        &expanded,
                                                        variables,
                                                        budget,
                                                        self.admission.monotonic_ms(),
                                                        max_age,
                                                    )
                                                    .map_err(
                                                        homesense_core::rule_chain::Error::Planning,
                                                    )?;
                                                lease = Some(read);
                                                Ok(plan)
                                            } else {
                                                homesense_core::rule::evaluate(
                                                    &expanded, variables, budget,
                                                )
                                                .map_err(homesense_core::rule_chain::Error::Rule)
                                            }
                                        },
                                    )
                                    .map_err(|e| format!("rule chain {}: {e:?}", binding.id))?;
                                cause = Some(run.cause);
                                causation_id = Some(run.causation_id);
                                Ok(run.plan)
                            },
                        )
                        .await?;
                        prepared.attach_feedback(lease);
                        prepared.attach_cause(
                            &binding.id,
                            cause.as_ref().ok_or("missing rule cause")?,
                            causation_id.ok_or("missing parent cause")?,
                        );
                        prepared.commit(&self.admission).await?
                    };
                    Ok(commands)
                }
                .await;
                let commands = match outcome {
                    Ok(commands) => commands,
                    Err(mut message) => {
                        if self.mode == Mode::Execute
                            && let Err(error) = self
                                .admission
                                .record_rule_outcome(
                                    self.controller,
                                    self.generation.clone(),
                                    root.correlation_id(),
                                    Some(message.clone()),
                                )
                                .await
                        {
                            message.push_str(&format!("; outcome persistence: {error:?}"));
                        }
                        return Err(Failure {
                            message,
                            completed: fired,
                            failed_rule: Some(binding.id.clone()),
                        });
                    }
                };
                fired.push(Fired {
                    rule: binding.id.clone(),
                    cause,
                    causation_id,
                    event,
                    commands,
                    observed_only: self.mode == Mode::Observe,
                });
            }
            if self.mode == Mode::Execute
                && let Err(error) = self
                    .admission
                    .record_rule_outcome(
                        self.controller,
                        self.generation.clone(),
                        root.correlation_id(),
                        None,
                    )
                    .await
            {
                return Err(Failure {
                    message: format!("outcome persistence: {error:?}"),
                    completed: fired,
                    failed_rule: None,
                });
            }
        }
        Ok(fired)
    }
}
