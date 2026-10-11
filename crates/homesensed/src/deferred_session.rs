//! One bounded causal session retained across explicitly driven scene results.
//! Construction does not subscribe to events or start any scene.
use crate::{
    admission::{Admission, TaskGeneration},
    deferred_rule::{self, Outcome},
    feedback::Feedback,
    scene_scheduler::{SceneResult, SceneScheduler},
};
use homesense_core::{
    rule::Budget,
    rule_chain::{Cause, Chain},
};
use std::collections::BTreeMap;
use uuid::Uuid;
pub struct Session {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    scheduler: Uuid,
    chain: Chain,
    pending: BTreeMap<u64, (String, Cause)>,
    started: bool,
    closed: bool,
}
impl Session {
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        scheduler: &SceneScheduler,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<Self, String> {
        if !scheduler.matches_admission(&admission) || !scheduler.matches_controller(controller) {
            return Err("foreign scheduler".into());
        }
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        admission
            .variable_snapshot(controller, generation.clone(), vec![])
            .await
            .map_err(|e| format!("{e:?}"))?;
        let chain = Chain::new(
            Uuid::new_v4().as_u128(),
            admission.monotonic_ms(),
            duration_ms,
            budget,
            32,
        )
        .map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            admission,
            controller,
            generation,
            scheduler: scheduler.identity(),
            chain,
            pending: BTreeMap::new(),
            started: false,
            closed: false,
        })
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    async fn validate(&self, scheduler: &SceneScheduler) -> Result<(), String> {
        if self.closed
            || scheduler.identity() != self.scheduler
            || self
                .admission
                .task_generation()
                .await
                .map_err(|e| format!("{e:?}"))?
                != self.generation
        {
            return Err("causal session expired or foreign scheduler".into());
        }
        Ok(())
    }
    fn track(&mut self, outcome: &Outcome, scheduler: &SceneScheduler) -> Result<(), String> {
        if self.pending.len() + outcome.scene_runs.len() > 64 {
            return Err("causal session capacity exceeded".into());
        }
        for run in &outcome.scene_runs {
            let audit = scheduler
                .audit_id(*run)
                .ok_or("accepted scene not installed")?;
            if self
                .pending
                .insert(*run, (audit.into(), outcome.cause.clone()))
                .is_some()
            {
                return Err("duplicate causal run".into());
            }
        }
        Ok(())
    }
    async fn cascade(
        &mut self,
        mut outcomes: Vec<Outcome>,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        let document = self
            .admission
            .automation_bundle(self.controller, self.generation.clone())
            .await
            .map_err(|e| format!("{e:?}"))?
            .automation
            .ok_or("automation missing")?;
        let rules = document
            .linked_rules_with_deferred()
            .map_err(|e| e.to_string())?;
        for outcome in &outcomes {
            self.track(outcome, scheduler)?;
        }
        let mut index = 0;
        while index < outcomes.len() {
            let parent = outcomes[index].cause.clone();
            let changed = &outcomes[index].changed_variables;
            let triggered:Vec<_>=rules.iter().filter(|r|matches!(&r.trigger,homesense_config::rule_document::Trigger::VariableChanged{variable} if changed.contains(variable))).map(|r|r.id.clone()).collect();
            for rule in triggered {
                if outcomes.len() >= 1024 {
                    return Err("causal report capacity exceeded".into());
                }
                let outcome = deferred_rule::execute(
                    &self.admission,
                    self.controller,
                    &rule,
                    &mut self.chain,
                    &parent,
                    scheduler,
                    feedback,
                )
                .await?;
                self.track(&outcome, scheduler)?;
                outcomes.push(outcome);
            }
            index += 1;
        }
        self.closed = self.pending.is_empty();
        if self.closed {
            self.admission
                .record_rule_outcome(
                    self.controller,
                    self.generation.clone(),
                    self.chain.root().correlation_id(),
                    None,
                )
                .await
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(outcomes)
    }
    async fn record_failure(&mut self, error: String) -> String {
        self.closed = true;
        self.chain.cancel();
        let mut length = error.len().min(8192);
        while !error.is_char_boundary(length) {
            length -= 1;
        }
        match self
            .admission
            .record_rule_outcome(
                self.controller,
                self.generation.clone(),
                self.chain.root().correlation_id(),
                Some(error[..length].into()),
            )
            .await
        {
            Ok(()) => error,
            Err(audit) => format!("{error}; failure audit unavailable: {audit:?}"),
        }
    }
    /// One explicit root rule. Errors/cancellation close the session and gate
    /// admission; accepted actions are not undone or retried.
    pub async fn start(
        &mut self,
        rule: &str,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        self.validate(scheduler).await?;
        if self.started {
            return Err("causal root already started".into());
        }
        self.started = true;
        self.closed = true;
        let mut guard = Guard(self.admission.clone(), false);
        let execution: Result<Vec<Outcome>, String> = async {
            let parent = self.chain.root();
            let outcome = deferred_rule::execute(
                &self.admission,
                self.controller,
                rule,
                &mut self.chain,
                &parent,
                scheduler,
                feedback,
            )
            .await?;
            let outcomes = self.cascade(vec![outcome], scheduler, feedback).await?;
            Ok(outcomes)
        }
        .await;
        match execution {
            Ok(outcomes) => {
                guard.1 = true;
                Ok(outcomes)
            }
            Err(error) => Err(self.record_failure(error).await),
        }
    }
    /// Results are correlated by both scheduler handle and durable run identity.
    pub async fn resume(
        &mut self,
        result: &SceneResult,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        self.validate(scheduler).await?;
        let (audit, parent) = self
            .pending
            .get(&result.run)
            .ok_or("untracked scene result")?;
        if audit != &result.audit_id {
            return Err("scene result identity mismatch".into());
        }
        let parent = parent.clone();
        self.closed = true;
        let mut guard = Guard(self.admission.clone(), false);
        let execution: Result<Vec<Outcome>, String> = async {
            let outcomes = deferred_rule::resume_scene(
                &self.admission,
                self.controller,
                &mut self.chain,
                &parent,
                result,
                scheduler,
                feedback,
            )
            .await?;
            self.pending.remove(&result.run);
            let outcomes = self.cascade(outcomes, scheduler, feedback).await?;
            Ok(outcomes)
        }
        .await;
        match execution {
            Ok(outcomes) => {
                guard.1 = true;
                Ok(outcomes)
            }
            Err(error) => Err(self.record_failure(error).await),
        }
    }
}
struct Guard(Admission, bool);
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.1 {
            self.0.stop_now();
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.chain.cancel();
        if !self.pending.is_empty() {
            self.admission.stop_now();
        }
    }
}
