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
    planning_failure: bool,
    isolated_failure: bool,
    retain_for_feedback: bool,
    manual_admission_deadline:Option<u64>,
    causes: BTreeMap<u64, Cause>,
}
impl Session {
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        scheduler: &SceneScheduler,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<Self, String> {
        if !scheduler.matches_controller(controller){return Err("foreign scheduler".into());}
        Self::new_for_caller(admission,controller,scheduler,budget,duration_ms).await
    }
    pub(crate) async fn new_for_caller(admission:Admission,controller:Uuid,scheduler:&SceneScheduler,budget:Budget,duration_ms:u64)->Result<Self,String> {
        Self::new_for_caller_before(admission,controller,scheduler,budget,duration_ms,None).await
    }
    pub(crate) async fn new_for_caller_before(admission:Admission,controller:Uuid,scheduler:&SceneScheduler,budget:Budget,duration_ms:u64,deadline:Option<u64>)->Result<Self,String> {
        if !scheduler.matches_admission(&admission){return Err("foreign scheduler".into());}
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        admission
            .variable_snapshot(controller, generation.clone(), vec![])
            .await
            .map_err(|e| format!("{e:?}"))?;
        let now=admission.monotonic_ms();
        if deadline.is_some_and(|deadline|deadline<=now){return Err("manual scene request expired".into());}
        let chain = Chain::new(
            Uuid::new_v4().as_u128(),
            now,
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
            planning_failure: false,
            isolated_failure: false,
            retain_for_feedback: false,
            manual_admission_deadline:deadline,
            causes: BTreeMap::new(),
        })
    }
    /// Retain the original budget until its original deadline, even when all
    /// current commands/scenes have settled. No feedback subscription is opened.
    pub async fn new_for_feedback(
        admission: Admission, controller: Uuid, scheduler: &SceneScheduler,
        budget: Budget, duration_ms: u64,
    ) -> Result<Self, String> {
        let mut session = Self::new(admission, controller, scheduler, budget, duration_ms).await?;
        session.retain_for_feedback = true;
        Ok(session)
    }
    pub(crate) fn generation(&self)->&TaskGeneration {&self.generation}
    pub(crate) fn correlation(&self) -> u128 { self.chain.root().correlation_id() }
    pub fn deadline_ms(&self) -> u64 { self.chain.deadline_ms() }
    pub async fn expire(&mut self, scheduler: &mut SceneScheduler) -> Result<bool, String> {
        if !self.closed && !self.admission.source_valid() {
            self.cancel_with_reason(scheduler,homesense_storage::scene_run::CancellationReason::CancelRequested).await?;
            self.isolated_failure=true;
            return Ok(true);
        }
        if self.closed || self.admission.monotonic_ms() < self.chain.deadline_ms() {
            return Ok(false);
        }
        self.validate(scheduler).await?;
        let mut guard = Guard(self.admission.clone(), false);
        if self.retain_for_feedback && self.pending.is_empty() {
            self.closed = true;
            self.chain.cancel();
            self.admission.record_rule_outcome(
                self.controller, self.generation.clone(),
                self.chain.root().correlation_id(), None,
            ).await.map_err(|e| format!("feedback retention completion audit: {e:?}"))?;
            guard.1 = true;
            return Ok(true);
        }
        self.planning_failure = true;
        let error = self
            .record_failure("causal deadline exceeded".into(), scheduler)
            .await;
        guard.1 = self.isolated_failure;
        if self.isolated_failure {
            Ok(true)
        } else {
            Err(error)
        }
    }
    pub(crate) async fn finish_retained_idle(&mut self, scheduler:&SceneScheduler) -> Result<bool,String> {
        if !self.retain_for_feedback || self.closed || !self.started || !self.pending.is_empty() {return Ok(false);}
        if scheduler.identity()!=self.scheduler {return Err("foreign causal scheduler".into());}
        let mut guard=Guard(self.admission.clone(),false);
        self.closed=true;
        self.chain.cancel();
        self.admission.record_rule_outcome(self.controller,self.generation.clone(),self.chain.root().correlation_id(),None).await.map_err(|e|format!("feedback close audit: {e:?}"))?;
        guard.1=true;
        Ok(true)
    }
    pub fn failure_isolated(&self) -> bool {
        self.isolated_failure
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub(crate) fn pending_identities(&self) -> BTreeMap<u64, String> {
        self.pending
            .iter()
            .map(|(id, (audit, _))| (*id, audit.clone()))
            .collect()
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
        if self.retain_for_feedback { self.causes.insert(outcome.cause.event_id(), outcome.cause.clone()); }
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
        let mut index = 0;
        while index < outcomes.len() {
            let parent = outcomes[index].cause.clone();
            let changed = &outcomes[index].changed_variables;
            let triggered:Vec<_>=rules.iter().filter(|r|matches!(&r.trigger,homesense_config::rule_document::Trigger::VariableChanged{variable} if changed.contains(variable))).map(|r|r.id.clone()).collect();
            for rule in triggered {
                if outcomes.len() >= 1024 {
                    return Err("causal report capacity exceeded".into());
                }
                let outcome = deferred_rule::execute_caller_rule_classified(
                    deferred_rule::Caller{admission:&self.admission,controller:self.controller,generation:&self.generation,admit_before:None},
                    &rule,
                    &mut self.chain,
                    &parent,
                    scheduler,
                    feedback,
                )
                .await
                .map_err(|error| {
                    self.planning_failure = error.is_chain_rejection();
                    error.to_string()
                })?;
                self.track(&outcome, scheduler)?;
                outcomes.push(outcome);
            }
            index += 1;
        }
        self.closed = self.pending.is_empty() && !self.retain_for_feedback;
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
    async fn record_failure(&mut self, error: String, scheduler: &mut SceneScheduler) -> String {
        self.closed = true;
        self.chain.cancel();
        // Known source loss cancels only this chain. It is not a storage or
        // transport failure and never renews authority for derived output.
        self.planning_failure |= !self.admission.source_valid();
        if self.planning_failure {
            for (run, (audit, _)) in &self.pending {
                if scheduler.audit_id(*run) != Some(audit.as_str()) {
                    return format!("{error}; failure cleanup identity unavailable");
                }
                if let Err(cleanup) = scheduler.cancel(*run).await {
                    return format!("{error}; failure cleanup unavailable: {cleanup}");
                }
            }
        }
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
            Ok(()) => {
                self.isolated_failure = self.planning_failure;
                error
            }
            Err(audit) => format!("{error}; failure audit unavailable: {audit:?}"),
        }
    }
    /// One explicit root rule. Verified planning rejection may be isolated after
    /// cleanup and audit; other errors/cancellation gate admission. No rollback.
    pub async fn start(
        &mut self,
        rule: &str,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        self.start_rules(&[rule.to_owned()], scheduler, feedback)
            .await
    }
    pub(crate) fn retain_feedback(&mut self) {self.retain_for_feedback=true;}
    /// One immediate action batch, followed by the ordinary variable/result
    /// cascade in the same bounded chain and under the original caller.
    pub(crate) async fn start_manual_scenes(&mut self,writes:&[(String,Option<bool>)],scheduler:&mut SceneScheduler,feedback:Option<(&Feedback,u64)>)->Result<Vec<Outcome>,String> {
        self.validate(scheduler).await?;
        if self.started{return Err("causal root already started".into());}
        self.started=true;self.closed=true;
        let mut guard=Guard(self.admission.clone(),false);
        let parent=self.chain.root();
        let mut rejected_before_acceptance=false;
        let result=async {
            let outcome=deferred_rule::execute_manual_scenes_classified(deferred_rule::Caller{admission:&self.admission,controller:self.controller,generation:&self.generation,admit_before:self.manual_admission_deadline},writes,&mut self.chain,&parent,scheduler,feedback).await.map_err(|e|{rejected_before_acceptance=matches!(e,deferred_rule::ExecutionFailure::RejectedBeforeAcceptance(_));self.planning_failure=matches!(e,deferred_rule::ExecutionFailure::Planning(_)) || e.is_chain_rejection();e.to_string()})?;
            self.track(&outcome,scheduler)?;
            self.cascade(vec![outcome],scheduler,feedback).await
        }.await;
        match result {
            Ok(outcomes)=>{guard.1=true;Ok(outcomes)},
            Err(error) if rejected_before_acceptance=>{
                // No command, variable or run was accepted. A revoked caller
                // cannot persist a new audit as a privileged owner fallback.
                self.chain.cancel();self.closed=true;self.isolated_failure=true;
                log::warn!("manual root rejected before acceptance correlation={}: {error}",self.chain.root().correlation_id());
                guard.1=true;Err(error)
            },
            Err(error)=>{let error=self.record_failure(error,scheduler).await;guard.1=self.isolated_failure;Err(error)},
        }
    }
    /// All rules matching one producer event share the same root and budget.
    pub async fn start_rules(
        &mut self,
        rules: &[String],
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        Self::validate_rules(rules)?;
        self.validate(scheduler).await?;
        if self.started {
            return Err("causal root already started".into());
        }
        self.started = true;
        let parent = self.chain.root();
        self.execute_rules(rules, &parent, scheduler, feedback).await
    }
    fn validate_rules(rules: &[String]) -> Result<(), String> {
        if rules.is_empty() || rules.len() > 256
            || rules.iter().collect::<std::collections::BTreeSet<_>>().len() != rules.len() {
            return Err("invalid causal rule batch".into());
        }
        Ok(())
    }
    /// Continue a retained chain using an explicitly inferred parent. Caller
    /// must match a completed write and persist the inference before invoking.
    /// This never renews the deadline or constructs a fresh budget.
    pub async fn resume_inferred(
        &mut self, rules: &[String], candidate: &homesense_core::feedback_cause::Candidate,
        scheduler: &mut SceneScheduler, feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        Self::validate_rules(rules)?;
        self.validate(scheduler).await?;
        if !self.retain_for_feedback || !self.started || candidate.command_id == 0
            || candidate.parent_event == 0
            || candidate.correlation != self.chain.root().correlation_id()
            || candidate.written_ms > self.admission.monotonic_ms() {
            return Err("invalid inferred feedback continuation".into());
        }
        let parent = self.causes.get(&candidate.parent_event).cloned().ok_or("unknown inferred parent")?;
        log::info!("inferred feedback continuation correlation={} parent={} command={}",
            candidate.correlation, candidate.parent_event, candidate.command_id);
        self.execute_rules(rules, &parent, scheduler, feedback).await
    }
    async fn execute_rules(
        &mut self, rules: &[String], parent: &Cause,
        scheduler: &mut SceneScheduler, feedback: Option<(&Feedback, u64)>,
    ) -> Result<Vec<Outcome>, String> {
        self.closed = true;
        let mut guard = Guard(self.admission.clone(), false);
        let execution: Result<Vec<Outcome>, String> = async {
            let mut roots = Vec::new();
            for rule in rules {
                let outcome = deferred_rule::execute_caller_rule_classified(
                    deferred_rule::Caller{admission:&self.admission,controller:self.controller,generation:&self.generation,admit_before:None},
                    rule,
                    &mut self.chain,
                    parent,
                    scheduler,
                    feedback,
                )
                .await
                .map_err(|error| {
                    self.planning_failure = error.is_chain_rejection();
                    error.to_string()
                })?;
                self.track(&outcome, scheduler)?;
                roots.push(outcome);
            }
            let outcomes = self.cascade(roots, scheduler, feedback).await?;
            Ok(outcomes)
        }
        .await;
        match execution {
            Ok(outcomes) => {
                guard.1 = true;
                Ok(outcomes)
            }
            Err(error) => {
                let error = self.record_failure(error, scheduler).await;
                guard.1 = self.isolated_failure;
                Err(error)
            }
        }
    }
    /// Cancel this chain without deriving scene-result rules. Commands already
    /// writing cannot be undone. Failure or abandonment gates admission.
    pub async fn cancel(&mut self, scheduler: &mut SceneScheduler) -> Result<usize, String> {
        self.cancel_with_reason(scheduler,homesense_storage::scene_run::CancellationReason::CancelRequested).await
    }
    pub(crate) async fn cancel_with_reason(&mut self,scheduler:&mut SceneScheduler,reason:homesense_storage::scene_run::CancellationReason)->Result<usize,String> {
        // Cancellation must remain possible while transport admission is closed.
        // Durable cancellation and outcome writes still validate authority/scope.
        if self.closed || scheduler.identity() != self.scheduler {
            return Err("causal session expired or foreign scheduler".into());
        }
        if !self.started {
            return Err("causal root not started".into());
        }
        self.closed = true;
        self.chain.cancel();
        let mut guard = Guard(self.admission.clone(), false);
        let execution: Result<usize, String> = async {
            let mut cancelled = 0;
            for (run, (audit, _)) in &self.pending {
                if scheduler.audit_id(*run) != Some(audit.as_str()) {
                    return Err("cancelled scene identity unavailable".into());
                }
                cancelled += scheduler.cancel_with_reason(*run,reason).await?;
            }
            self.admission
                .record_rule_outcome(
                    self.controller,
                    self.generation.clone(),
                    self.chain.root().correlation_id(),
                    Some("causal session cancelled".into()),
                )
                .await
                .map_err(|e| format!("cancellation audit unavailable: {e:?}"))?;
            Ok(cancelled)
        }
        .await;
        match execution {
            Ok(cancelled) => {
                self.pending.clear();
                guard.1 = true;
                Ok(cancelled)
            }
            Err(error) => Err(error),
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
        if !self.admission.source_valid() {
            // Router already retired this verified terminal; do not attempt
            // cancellation through a scheduler handle that no longer exists.
            self.pending.remove(&result.run);
            self.cancel_with_reason(scheduler,homesense_storage::scene_run::CancellationReason::CancelRequested).await?;
            self.isolated_failure=true;
            return Err("source authority revoked before scene continuation".into());
        }
        self.closed = true;
        let mut guard = Guard(self.admission.clone(), false);
        let execution: Result<Vec<Outcome>, String> = async {
            let continuation = deferred_rule::resume_scene_for_caller(
                deferred_rule::Caller{admission:&self.admission,controller:self.controller,generation:&self.generation,admit_before:None},
                &mut self.chain,
                &parent,
                result,
                scheduler,
                feedback,
            )
            .await;
            let outcomes = match continuation {
                Ok(outcomes) => outcomes,
                Err(deferred_rule::ExecutionFailure::Partial { completed, error }) => {
                    self.planning_failure = error.is_chain_rejection();
                    // The terminal was verified and claimed before these commits.
                    self.pending.remove(&result.run);
                    for outcome in &completed {
                        self.track(outcome, scheduler)?;
                    }
                    return Err(format!(
                        "{} accepted successor rules before failure: {error}",
                        completed.len()
                    ));
                }
                Err(error) => {
                    self.planning_failure = error.is_chain_rejection();
                    if self.planning_failure {
                        // Planning errors occur only after receipt verification.
                        // The scheduler may already have retired this terminal.
                        self.pending.remove(&result.run);
                    }
                    return Err(error.to_string());
                }
            };
            self.pending.remove(&result.run);
            for outcome in &outcomes {
                self.track(outcome, scheduler)?;
            }
            let outcomes = self.cascade(outcomes, scheduler, feedback).await?;
            Ok(outcomes)
        }
        .await;
        match execution {
            Ok(outcomes) => {
                guard.1 = true;
                Ok(outcomes)
            }
            Err(error) => {
                let error = self.record_failure(error, scheduler).await;
                guard.1 = self.isolated_failure;
                Err(error)
            }
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
        if !self.pending.is_empty() && !self.isolated_failure {
            self.admission.stop_now();
        }
    }
}
