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
pub struct Router {
    admission: Admission,
    controller: Uuid,
    scheduler: Uuid,
    entries: BTreeMap<u64, Entry>,
    next: u64,
    capacity: usize,
    closed: bool,
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
        })
    }
    fn validate(&self, scheduler: &SceneScheduler) -> Result<(), String> {
        if self.scheduler != scheduler.identity() {
            return Err("foreign causal scheduler".into());
        }
        Ok(())
    }
    pub(crate) fn matches_owner(&self, admission:&Admission, controller:Uuid)->bool {
        self.admission.boot_id()==admission.boot_id() && self.controller==controller
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
        self.validate(scheduler)?;
        if self.closed {
            return Err("causal router closed".into());
        }
        if self.entries.len() >= self.capacity {
            return Err("causal router full".into());
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("causal identity exhausted")?;
        let mut session = Session::new(
            self.admission.clone(),
            self.controller,
            scheduler,
            budget,
            duration_ms,
        )
        .await?;
        let outcomes = session.start_rules(rules, scheduler, feedback).await?;
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
        for tick in scheduler.tick(self.admission.monotonic_ms()).await? {
            tick.result?;
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
            Entry::Active(session) => {
                let outcomes = session.resume(result, scheduler, feedback).await?;
                (outcomes, session.is_closed())
            }
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
