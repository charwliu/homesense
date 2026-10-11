//! Explicitly driven timeline -> durable rule admission adapter. No subscriptions
//! or sockets are started by construction. Transport completion is reported by
//! the existing owner, never inferred from successful admission.
use crate::admission::{Admission, TaskGeneration};
use homesense_core::{
    DeviceId,
    scene::{Status, Step, Timeline},
};
use homesense_hap::requests::{Batch, Intent};
use uuid::Uuid;
pub struct SceneRuntime {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    manual_fence: u64,
    timeline: Timeline,
    targets: crate::scene_activity::Targets,
    audit_id: String,
    definition: Option<String>,
    result_taken: bool,
    audit_version: u64,
    audit_status: homesense_storage::scene_run::Status,
    audit_failed: bool,
    admitted: Vec<u64>,
    pending: Option<Pending>,
}
struct Pending {
    device: DeviceId,
    index: usize,
    run: u64,
    ids: Vec<u64>,
    written: usize,
}
impl SceneRuntime {
    pub(crate) fn take_result(&mut self) -> Option<(String, String, Status)> {
        if self.audit_failed
            || self.result_taken
            || self.pending.is_some()
            || matches!(
                self.audit_status,
                homesense_storage::scene_run::Status::Pending
                    | homesense_storage::scene_run::Status::Running
            )
        {
            return None;
        }
        let definition = self.definition.clone()?;
        self.result_taken = true;
        Some((definition, self.audit_id.clone(), self.status()))
    }
    pub(crate) fn configuration_revision(&self) -> u64 {
        self.generation.epochs.configuration
    }
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        steps: Vec<Step>,
        now: u64,
        max_lateness: u64,
    ) -> Result<Self, String> {
        Self::new_named(admission, controller, steps, now, max_lateness, None).await
    }
    pub(crate) async fn new_named(
        admission: Admission,
        controller: Uuid,
        steps: Vec<Step>,
        now: u64,
        max_lateness: u64,
        definition: Option<String>,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let targets = crate::scene_activity::Targets::new(&steps);
        let timeline = Timeline::new(steps, generation.epochs, now, max_lateness)
            .map_err(|e| format!("{e:?}"))?;
        let manual_fence = admission
            .manual_fence()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let audit_id = Uuid::new_v4().to_string();
        admission
            .scene_update(
                controller,
                generation.clone(),
                crate::admission::SceneUpdate::Begin {
                    run: audit_id.clone(),
                    definition: definition.clone(),
                },
            )
            .await
            .map_err(|e| format!("scene receipt: {e:?}"))?;
        Ok(Self {
            audit_id,
            definition,
            result_taken: false,
            audit_version: 0,
            audit_status: homesense_storage::scene_run::Status::Pending,
            audit_failed: false,
            manual_fence,
            admission,
            controller,
            generation,
            timeline,
            targets,
            admitted: vec![],
            pending: None,
        })
    }
    /// Caller uses the same boot-relative monotonic clock for construction and polling.
    pub async fn poll(&mut self, now: u64) -> Result<Option<Vec<u64>>, String> {
        if self.audit_failed {
            return Err("scene audit unavailable".into());
        }
        let result = self.poll_inner(now).await;
        self.checkpoint().await?;
        result
    }
    async fn poll_inner(&mut self, now: u64) -> Result<Option<Vec<u64>>, String> {
        self.reconcile().await?;
        if self.pending.is_some() {
            return Err("scene action still in flight".into());
        }
        let current = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if current != self.generation {
            self.timeline.cancel();
            return Err("scene generation changed".into());
        }
        let Some(due) = self
            .timeline
            .next(now, current.epochs)
            .map_err(|e| format!("{e:?}"))?
        else {
            return Ok(None);
        };
        let device = due.step.device.clone();
        let batch = Batch {
            controller: self.controller,
            intents: vec![Intent {
                device: due.step.device,
                command: due.step.command,
            }],
            subscriptions: vec![],
        };
        match self
            .admission
            .submit_scene_step(
                self.generation.clone(),
                batch,
                self.manual_fence,
                homesense_storage::scene_run::StepLink {
                    run: self.audit_id.clone(),
                    index: due.index,
                    source: due.step.source,
                },
            )
            .await
        {
            Ok(ids) => {
                self.admitted.push(ids[0]);
                self.pending = Some(Pending {
                    device,
                    index: due.index,
                    run: ids[0],
                    ids: ids.clone(),
                    written: 0,
                });
                Ok(Some(ids))
            }
            Err(e) => {
                self.timeline
                    .resolve(due.index, false)
                    .map_err(|e| format!("{e:?}"))?;
                Err(format!("scene admission: {e:?}"))
            }
        }
    }
    /// Resolve overwritten/cancelled output even when no transport callback will
    /// arrive. Durable Writing remains pending; it is never treated as success.
    pub async fn reconcile(&mut self) -> Result<(), String> {
        use homesense_storage::journal::Status as Audit;
        let Some(pending) = &self.pending else {
            return self.checkpoint().await;
        };
        let states = self
            .admission
            .command_statuses(
                self.controller,
                self.generation.clone(),
                pending.ids.clone(),
            )
            .await
            .map_err(|e| format!("scene audit: {e:?}"))?;
        if states
            .iter()
            .any(|s| matches!(s, Audit::Accepted | Audit::Writing))
        {
            return Ok(());
        }
        let written = pending.written
            + states
                .iter()
                .filter(|s| matches!(s, Audit::Written | Audit::Observed))
                .count();
        let failed = states
            .iter()
            .any(|s| matches!(s, Audit::Failed | Audit::Uncertain));
        if failed {
            self.timeline.cancel();
        } else if states.contains(&Audit::Cancelled) {
            self.timeline.cancel_device(&pending.device);
        }
        let result = if written == pending.written + states.len() {
            self.timeline.resolve(pending.index, true)
        } else if written > 0 || states.contains(&Audit::Uncertain) {
            self.timeline.resolve_partial(pending.index)
        } else if failed {
            self.timeline.resolve(pending.index, false)
        } else {
            self.timeline.cancel_offered(pending.index)
        };
        result.map_err(|e| format!("{e:?}"))?;
        self.pending = None;
        self.checkpoint().await
    }
    /// Report each durable TransportOwner result. Failed output is never retried.
    pub async fn written(&mut self, id: u64, success: bool) -> Result<(), String> {
        let pending = self.pending.as_mut().ok_or("no scene action pending")?;
        if pending.ids.first() != Some(&id) {
            return Err("unexpected scene completion".into());
        }
        if !success {
            self.timeline.cancel();
            // Admission::finish_write(false), called by TransportOwner before
            // this notification, durably cancels all accepted output and gates
            // the failed transport. A second cancellation would be rejected by
            // that gate; never use this notification before finish_write.
            if pending.written > 0 {
                self.timeline.resolve_partial(pending.index)
            } else {
                self.timeline.resolve(pending.index, false)
            }
            .map_err(|e| format!("{e:?}"))?;
            self.pending = None;
            return self.checkpoint().await;
        }
        pending.ids.remove(0);
        pending.written += 1;
        if pending.ids.is_empty() {
            self.timeline
                .resolve(pending.index, true)
                .map_err(|e| format!("{e:?}"))?;
            self.pending = None;
        }
        self.checkpoint().await
    }
    pub fn cancel_device_timers(&mut self, device: &DeviceId) -> usize {
        self.timeline.cancel_device(device)
    }
    /// Call after an accepted manual override or before a replacement scene is
    /// started. Invalidate future work before awaiting durable cancellation.
    /// A Writing command cannot be recalled and remains pending until settled.
    pub async fn cancel_device(&mut self, device: &DeviceId) -> Result<usize, String> {
        let mut count = self.timeline.cancel_device(device);
        if let Some(pending) = &self.pending
            && &pending.device == device
        {
            count += self
                .admission
                .cancel_scene_run(self.controller, self.generation.clone(), pending.run)
                .await
                .map_err(|e| format!("scene device cancellation: {e:?}"))?;
            self.reconcile().await?;
        }
        self.checkpoint().await?;
        Ok(count)
    }
    pub async fn cancel(&mut self) -> Result<usize, String> {
        // Invalidate volatile timers even if durable cancellation fails; caller
        // may retry cancellation, but no further timed action can be admitted.
        let mut count = self.timeline.cancel();
        for run in &self.admitted {
            let cancelled = self
                .admission
                .cancel_scene_run(self.controller, self.generation.clone(), *run)
                .await
                .map_err(|e| format!("{e:?}"))?;
            count += cancelled;
            if let Some(pending) = &self.pending
                && pending.run == *run
                && cancelled == pending.ids.len()
            {
                if pending.written > 0 {
                    self.timeline.resolve_partial(pending.index)
                } else {
                    self.timeline.cancel_offered(pending.index)
                }
                .map_err(|e| format!("{e:?}"))?;
                self.pending = None;
            }
        }
        self.reconcile().await?;
        Ok(count)
    }
    pub(crate) fn cancel_timers(&mut self) -> usize {
        self.timeline.cancel()
    }
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
    /// Read-only projection; successful writes are not observed target evidence.
    pub async fn activity(
        &self,
        feedback: &crate::feedback::Feedback,
        max_age: u64,
    ) -> Result<crate::scene_activity::SceneActivity, String> {
        use crate::scene_activity::SceneActivity as Activity;
        use homesense_core::rule::{Budget, Lifetime, Value, ValueType, Variable};
        if max_age == 0
            || !feedback.is_active()
            || !self
                .admission
                .task_generation()
                .await
                .is_ok_and(|generation| generation == self.generation)
        {
            return Ok(Activity::Unknown);
        }
        let Some(nodes) = self.targets.nodes(feedback) else {
            return Ok(Activity::Unknown);
        };
        let variables = ["unknown", "mismatch"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    Variable {
                        value_type: ValueType::Bool,
                        lifetime: Lifetime::Temporary,
                        value: Some(Value::Bool(false)),
                    },
                )
            })
            .collect();
        let (plan, lease) = feedback.plan_rule(
            &nodes,
            &variables,
            Budget::default(),
            self.admission.monotonic_ms(),
            max_age,
        )?;
        if plan.variables["unknown"].value == Some(Value::Bool(true)) {
            return Ok(Activity::Unknown);
        }
        match self
            .admission
            .feedback_settled(self.controller, self.generation.clone(), lease)
            .await
        {
            Ok(false) => Ok(Activity::Pending),
            Ok(true) => Ok(
                if plan.variables["mismatch"].value == Some(Value::Bool(true)) {
                    Activity::Inactive
                } else {
                    Activity::Active
                },
            ),
            Err(hap::server::WriteStatus::ResourceBusy) => Ok(Activity::Unknown),
            Err(error) => Err(format!("scene activity: {error:?}")),
        }
    }
    pub fn audit_id(&self) -> &str {
        &self.audit_id
    }
    async fn checkpoint(&mut self) -> Result<(), String> {
        use homesense_storage::scene_run::Status as Audit;
        if self.audit_failed {
            return Err("scene audit unavailable".into());
        }
        let status = if self.pending.is_some() {
            Audit::Running
        } else {
            match self.timeline.status() {
                Status::Pending if self.audit_status == Audit::Running => Audit::Running,
                Status::Pending => Audit::Pending,
                Status::Running => Audit::Running,
                Status::CommandsWritten => Audit::CommandsWritten,
                Status::Partial => Audit::Partial,
                Status::Cancelled => Audit::Cancelled,
                Status::Failed => Audit::Failed,
            }
        };
        if status == self.audit_status {
            return Ok(());
        }
        // Cancellation of this await leaves the run gated, never resubmitted.
        self.audit_failed = true;
        let version = self
            .admission
            .scene_update(
                self.controller,
                self.generation.clone(),
                crate::admission::SceneUpdate::Checkpoint {
                    run: self.audit_id.clone(),
                    expected: self.audit_version,
                    status,
                },
            )
            .await
            .map_err(|e| format!("scene checkpoint: {e:?}"))?;
        self.audit_version = version;
        self.audit_status = status;
        self.audit_failed = false;
        Ok(())
    }
    pub fn status(&self) -> Status {
        self.timeline.status()
    }
}
