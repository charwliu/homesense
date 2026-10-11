//! Bounded ownership of causal sessions and cancelled-result tombstones.
//! Unknown results remain the caller's responsibility; never create a new root.
use crate::{
    admission::Admission,
    deferred_rule::Outcome,
    deferred_session::Session,
    feedback::Feedback,
    scene_scheduler::{SceneResult, SceneScheduler},
};
use homesense_core::rule::Budget;
use std::collections::BTreeMap;
use uuid::Uuid;

enum Entry {
    Active(Box<Session>),
    Cancelled(BTreeMap<u64, String>),
}
struct RootFailure {
    message: String,
    isolated: bool,
}
impl From<String> for RootFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            isolated: false,
        }
    }
}
impl From<&str> for RootFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
pub struct Router {
    admission: Admission,
    controller: Uuid,
    scheduler: Uuid,
    entries: BTreeMap<u64, Entry>,
    next: u64,
    capacity: usize,
    closed: bool,
    feedback_candidates: Option<(crate::admission::TaskGeneration, homesense_core::feedback_cause::Candidates)>,
}
impl Router {
    pub fn new(
        admission: Admission,
        controller: Uuid,
        scheduler: &SceneScheduler,
        capacity: usize,
    ) -> Result<Self, String> {
        if capacity == 0
            || capacity > 64
            || !scheduler.matches_admission(&admission)
            || !scheduler.matches_controller(controller)
        {
            return Err("invalid causal router capacity or owner".into());
        }
        Ok(Self {
            admission,
            controller,
            scheduler: scheduler.identity(),
            entries: BTreeMap::new(),
            next: 1,
            capacity,
            closed: false,
            feedback_candidates: None,
        })
    }
    /// Enable once before any roots start. State is scoped to this runtime and
    /// discarded on reconnect; history is never replayed as new input.
    pub async fn enable_feedback_candidates(&mut self) -> Result<(),String> {
        if self.feedback_candidates.is_some() {return Ok(());}
        if self.closed || !self.entries.is_empty() || self.next!=1 {return Err("feedback inference must be enabled before roots".into());}
        let generation=self.admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
        let candidates=homesense_core::feedback_cause::Candidates::new(generation.epochs,5000,4096).map_err(|e|format!("{e:?}"))?;
        self.feedback_candidates=Some((generation,candidates));
        Ok(())
    }
    /// Called only after the transport owner durably completes a write. The
    /// database, not the planned output list, supplies target and causal parent.
    pub async fn observe_written(&mut self, command_id:u64) -> Result<(),String> {
        let Some((generation,candidates))=&mut self.feedback_candidates else{return Ok(());};
        let Some(written)=self.admission.written_feedback_target(self.controller,generation.clone(),command_id).await.map_err(|e|format!("feedback write lookup: {e:?}"))? else{return Ok(());};
        match candidates.record(written.epochs,written.device,written.target,written.candidate) {
            Ok(())=>Ok(()),
            Err(homesense_core::feedback_cause::Error::Capacity)=>{log::warn!("feedback candidate capacity exceeded; feedback triggers constrained until window expires");Ok(())},
            Err(e)=>Err(format!("feedback candidate write: {e:?}")),
        }
    }
    pub(crate) async fn dispatch_feedback(
        &mut self, rules:&[String], event:&homesense_state::Event,
        scheduler:&mut SceneScheduler, feedback:(&Feedback,u64), budget:Budget,
    )->Result<Vec<Outcome>,String> {
        self.validate(scheduler)?;
        if self.closed {return Err("causal router closed".into());}
        let Some((generation,candidates))=&mut self.feedback_candidates else {
            return self.dispatch_rules(rules,scheduler,Some(feedback),budget,5000).await;
        };
        if event.change!=homesense_state::Change::Observation || event.state.quality!=homesense_state::Quality::Confirmed {
            return self.dispatch_rules(rules,scheduler,Some(feedback),budget,5000).await;
        }
        if event.cursor.boot_id!=Uuid::parse_str(self.admission.boot_id()).map_err(|e|e.to_string())?.as_u128()
            || feedback.0.transport_epoch()!=generation.epochs.transport {
            return Err("foreign candidate feedback generation".into());
        }
        let observed=event.state.observed.ok_or("candidate feedback missing value")?;
        let observed_ms=event.state.observed_at_ms.ok_or("candidate feedback missing time")?;
        use homesense_core::feedback_cause::{Attribution,Error};
        let candidate=match candidates.attribute(generation.epochs,&event.device_id,observed,observed_ms) {
            Ok(Attribution::Independent)=>return self.dispatch_rules(rules,scheduler,Some(feedback),budget,5000).await,
            Ok(Attribution::Inferred(candidate))=>candidate,
            Ok(Attribution::Ambiguous)|Err(Error::Capacity)=>{log::warn!("inferred feedback constrained: ambiguous parent or capacity");return Ok(vec![]);},
            Err(e)=>return Err(format!("feedback attribution: {e:?}")),
        };
        let generation=generation.clone();
        let owner=self.entries.iter_mut().find_map(|(_,entry)|match entry {
            Entry::Active(session) if session.correlation()==candidate.correlation=>Some(entry),
            _=>None,
        });
        let Some(entry)=owner else {
            log::warn!("inferred feedback suppressed: original chain is no longer active correlation={}",candidate.correlation);
            return Ok(vec![]);
        };
        let Entry::Active(session)=entry else {unreachable!()};
        if session.expire(scheduler).await? {
            *entry=Entry::Cancelled(session.pending_identities());
            return Ok(vec![]);
        }
        self.admission.record_feedback_inference(self.controller,generation.clone(),
            homesense_storage::feedback_inference::Inference {
                sequence:event.cursor.sequence, epochs:generation.epochs,
                correlation:candidate.correlation,parent_event:candidate.parent_event,
                command_id:candidate.command_id,device:event.device_id.clone(),observed,observed_ms,window_ms:5000,
            }).await.map_err(|e|format!("feedback inference audit: {e:?}"))?;
        match session.resume_inferred(rules,&candidate,scheduler,Some(feedback)).await {
            Ok(outcomes)=>Ok(outcomes),
            Err(error) if session.failure_isolated()=>{
                log::warn!("isolated inferred feedback chain: {error}");
                *entry=Entry::Cancelled(session.pending_identities());
                Ok(vec![])
            }
            Err(error)=>Err(error),
        }
    }
    fn validate(&self, scheduler: &SceneScheduler) -> Result<(), String> {
        if self.scheduler != scheduler.identity() {
            return Err("foreign causal scheduler".into());
        }
        Ok(())
    }
    pub(crate) fn matches_owner(&self, admission: &Admission, controller: Uuid) -> bool {
        self.admission.boot_id() == admission.boot_id() && self.controller == controller
    }
    pub fn retained_count(&self) -> usize {
        self.entries.len()
    }
    pub async fn start(
        &mut self,
        rule: &str,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<(u64, Vec<Outcome>), String> {
        self.start_rules(&[rule.to_owned()], scheduler, feedback, budget, duration_ms)
            .await
    }
    pub async fn start_rules(
        &mut self,
        rules: &[String],
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<(u64, Vec<Outcome>), String> {
        self.start_inner(rules, scheduler, feedback, budget, duration_ms)
            .await
            .map_err(|e| e.message)
    }
    /// Event producer entry: isolated failures are already durable diagnostics.
    /// Returned outcomes are completed rules, never a success claim for a failed chain.
    pub async fn dispatch_rules(
        &mut self,
        rules: &[String],
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<Vec<Outcome>, String> {
        match self
            .start_inner(rules, scheduler, feedback, budget, duration_ms)
            .await
        {
            Ok((_, outcomes)) => Ok(outcomes),
            Err(error) if error.isolated => {
                log::warn!("isolated causal root: {}", error.message);
                Ok(vec![])
            }
            Err(error) => Err(error.message),
        }
    }
    async fn start_inner(
        &mut self,
        rules: &[String],
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
        budget: Budget,
        duration_ms: u64,
    ) -> Result<(u64, Vec<Outcome>), RootFailure> {
        self.validate(scheduler)?;
        if self.closed {
            return Err("causal router closed".into());
        }
        if self.entries.len() >= self.capacity {
            // No session, variable delta or command exists for this trigger.
            // Isolate overload without closing existing automation or manual control.
            return Err(RootFailure {
                message: "causal router full; new root rejected before execution".into(),
                isolated: true,
            });
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("causal identity exhausted")?;
        let mut session = if self.feedback_candidates.is_some() {
            Session::new_for_feedback(self.admission.clone(),self.controller,scheduler,budget,duration_ms).await?
        } else {Session::new(
            self.admission.clone(),
            self.controller,
            scheduler,
            budget,
            duration_ms,
        )
        .await?};
        let outcomes = match session.start_rules(rules, scheduler, feedback).await {
            Ok(outcomes) => outcomes,
            Err(message) => {
                let isolated = session.failure_isolated();
                if isolated {
                    let pending = session.pending_identities();
                    if !pending.is_empty() {
                        self.entries.insert(id, Entry::Cancelled(pending));
                    }
                }
                return Err(RootFailure { message, isolated });
            }
        };
        if !session.is_closed() {
            self.entries.insert(id, Entry::Active(Box::new(session)));
        }
        Ok((id, outcomes))
    }
    pub async fn cancel(
        &mut self,
        id: u64,
        scheduler: &mut SceneScheduler,
    ) -> Result<usize, String> {
        self.validate(scheduler)?;
        let Some(Entry::Active(session)) = self.entries.get_mut(&id) else {
            return Err("unknown or cancelled causal session".into());
        };
        let pending = session.pending_identities();
        let count = session.cancel(scheduler).await?;
        self.entries.insert(id, Entry::Cancelled(pending));
        Ok(count)
    }
    /// Close root admission and attempt every active cancellation. Does not tick
    /// scenes or execute result rules; caller still owns transport shutdown.
    pub async fn close(&mut self, scheduler: &mut SceneScheduler) -> Result<(), String> {
        self.validate(scheduler)?;
        self.closed = true;
        struct Guard(Admission, bool);
        impl Drop for Guard {
            fn drop(&mut self) {
                if !self.1 {
                    self.0.stop_now();
                }
            }
        }
        let mut guard = Guard(self.admission.clone(), false);
        let active: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| matches!(entry, Entry::Active(_)).then_some(*id))
            .collect();
        let mut failures = Vec::new();
        for id in active {
            if let Some(Entry::Active(session))=self.entries.get_mut(&id) {
                match session.finish_retained_idle(scheduler).await {
                    Ok(true)=>{self.entries.remove(&id);continue;},
                    Err(error)=>{failures.push(format!("session {id}: {error}"));continue;},
                    Ok(false)=>{},
                }
            }
            if let Err(error) = self.cancel(id, scheduler).await {
                failures.push(format!("session {id}: {error}"));
            }
        }
        if !failures.is_empty() {
            return Err(failures.join("; "));
        }
        guard.1 = true;
        Ok(())
    }
    /// Advance scenes, route owned terminals, and release their scheduler slots.
    /// Unowned (for example manually started) results must be handled by caller.
    pub async fn poll(
        &mut self,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<(Vec<Outcome>, Vec<SceneResult>), String> {
        self.validate(scheduler)?;
        struct Guard(Admission, bool);
        impl Drop for Guard {
            fn drop(&mut self) {
                if !self.1 {
                    self.0.stop_now();
                }
            }
        }
        let mut guard = Guard(self.admission.clone(), false);
        for entry in self.entries.values_mut() {
            if let Entry::Active(session) = entry
                && session.expire(scheduler).await?
            {
                let pending = session.pending_identities();
                *entry = Entry::Cancelled(pending);
            }
        }
        self.entries.retain(|_,entry| !matches!(entry,Entry::Cancelled(pending) if pending.is_empty()));
        for tick in scheduler.tick_with_feedback(self.admission.monotonic_ms(),feedback).await? {
            if let Err(error) = tick.result {
                // Only a durably settled scene can be routed as a failed result.
                // Unknown output, audit failure or closed admission stays fatal.
                if !scheduler.settled_terminal(tick.run) {
                    return Err(error);
                }
                self.admission.task_generation().await.map_err(|e|format!("scene failure admission: {e:?}"))?;
                log::warn!("settled scene run={} failed: {error}",tick.run);
            }
        }
        let mut outcomes = Vec::new();
        let mut unowned = Vec::new();
        for result in scheduler.take_results() {
            let owned = self.entries.values().any(|entry| match entry {
                Entry::Active(session) => {
                    session.pending_identities().get(&result.run) == Some(&result.audit_id)
                }
                Entry::Cancelled(pending) => pending.get(&result.run) == Some(&result.audit_id),
            });
            if owned {
                // Release the terminal slot before a successor reserves capacity.
                scheduler.retire(result.run)?;
            }
            match self.route(&result, scheduler, feedback).await? {
                Some(mut completed) => outcomes.append(&mut completed),
                None => unowned.push(result),
            }
        }
        guard.1 = true;
        Ok((outcomes, unowned))
    }
    /// Some(empty) consumes a cancelled chain's result. None is unowned.
    pub async fn route(
        &mut self,
        result: &SceneResult,
        scheduler: &mut SceneScheduler,
        feedback: Option<(&Feedback, u64)>,
    ) -> Result<Option<Vec<Outcome>>, String> {
        self.validate(scheduler)?;
        let owner = self.entries.iter().find_map(|(id, entry)| {
            let pending = match entry {
                Entry::Active(session) => session.pending_identities(),
                Entry::Cancelled(pending) => pending.clone(),
            };
            pending.get(&result.run).map(|audit| (*id, audit.clone()))
        });
        let Some((id, audit)) = owner else {
            return Ok(None);
        };
        if audit != result.audit_id {
            return Err("causal result identity mismatch".into());
        }
        let entry = self.entries.get_mut(&id).expect("located entry");
        let (outcomes, done) = match entry {
            Entry::Active(session) => match session.resume(result, scheduler, feedback).await {
                Ok(outcomes) => (outcomes, session.is_closed()),
                Err(error) if session.failure_isolated() => {
                    log::warn!("isolated causal continuation session={id}: {error}");
                    let pending = session.pending_identities();
                    let done = pending.is_empty();
                    *entry = Entry::Cancelled(pending);
                    (vec![], done)
                }
                Err(error) => return Err(error),
            },
            Entry::Cancelled(pending) => {
                pending.remove(&result.run);
                (vec![], pending.is_empty())
            }
        };
        if done {
            self.entries.remove(&id);
        }
        Ok(Some(outcomes))
    }
}
