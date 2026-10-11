//! Bounded, explicitly driven scene scheduler. Construction does not enable live
//! automation. The service owns tick cadence and transport dispatch separately.
use crate::{admission::Admission, scene_runtime::SceneRuntime};
use homesense_core::{
    DeviceId,
    scene::{Status, Step},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub struct SceneScheduler {
    admission: Admission,
    controller: Uuid,
    runs: BTreeMap<u64, SceneRuntime>,
    next: u64,
    now: u64,
    capacity: usize,
    stopped: bool,
    results: Vec<SceneResult>,
}
#[derive(Debug)]
pub struct SceneResult {
    pub run: u64,
    pub definition: String,
    pub audit_id: String,
    pub status: Status,
}
#[derive(Debug)]
pub struct Tick {
    pub run: u64,
    pub result: Result<Vec<u64>, String>,
    pub status: Status,
}
impl SceneScheduler {
    pub fn new(
        admission: Admission,
        controller: Uuid,
        capacity: usize,
        now: u64,
    ) -> Result<Self, String> {
        if capacity == 0 || capacity > 64 {
            return Err("scene capacity must be 1..64".into());
        }
        Ok(Self {
            admission,
            controller,
            runs: BTreeMap::new(),
            next: 1,
            now,
            capacity,
            stopped: false,
            results: vec![],
        })
    }
    fn clock(&mut self, now: u64) -> Result<(), String> {
        if self.stopped {
            return Err("scene scheduler stopped".into());
        }
        if now < self.now {
            return Err("scene clock moved backwards".into());
        }
        self.now = now;
        Ok(())
    }
    /// Explicit start of an enabled, reviewed file definition. Loading a document
    /// never calls this method and therefore never starts timers or commands.
    pub async fn start_document(
        &mut self,
        document: &homesense_config::rule_document::Document,
        id: &str,
        now: u64,
    ) -> Result<u64, String> {
        let generation = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if document.revision != generation.epochs.configuration {
            return Err("timed scene document revision expired".into());
        }
        let scene = document.timed_scenes.get(id).ok_or("unknown timed scene")?;
        if !scene.enabled
            || scene
                .review_evidence
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err("timed scene disabled or unreviewed".into());
        }
        self.start_inner(
            scene.steps.clone(),
            now,
            scene.max_lateness_ms,
            Some(document.revision),
            Some(id.to_owned()),
        )
        .await
    }
    /// New scene targets supersede future and queued output for the same device.
    /// Cancellation failure prevents installation of the new scene; already
    /// cancelled old actions remain cancelled and are never restored.
    pub async fn start(
        &mut self,
        steps: Vec<Step>,
        now: u64,
        lateness: u64,
    ) -> Result<u64, String> {
        self.start_inner(steps, now, lateness, None, None).await
    }
    async fn start_inner(
        &mut self,
        steps: Vec<Step>,
        now: u64,
        lateness: u64,
        revision: Option<u64>,
        definition: Option<String>,
    ) -> Result<u64, String> {
        self.clock(now)?;
        if self.runs.len() + self.results.len() >= self.capacity {
            return Err("scene capacity reached; retire terminal runs".into());
        }
        let next = self.next.checked_add(1).ok_or("scene id exhausted")?;
        let devices: BTreeSet<_> = steps.iter().map(|s| s.device.clone()).collect();
        let scene = SceneRuntime::new_named(
            self.admission.clone(),
            self.controller,
            steps,
            now,
            lateness,
            definition,
        )
        .await?;
        if revision.is_some_and(|r| r != scene.configuration_revision()) {
            return Err("timed scene generation changed during start".into());
        }
        for old in self.runs.values_mut() {
            for device in &devices {
                old.cancel_device(device).await?;
            }
        }
        let id = self.next;
        self.next = next;
        self.runs.insert(id, scene);
        Ok(id)
    }
    /// Called after a manual control has been admitted by the command owner.
    pub async fn manual_override(&mut self, device: &DeviceId) -> Result<usize, String> {
        let mut cancelled = self
            .runs
            .values_mut()
            .map(|run| run.cancel_device_timers(device))
            .sum::<usize>();
        for run in self.runs.values_mut() {
            cancelled += run.cancel_device(device).await?;
        }
        Ok(cancelled)
    }
    /// At most one admission per scene per tick. Transport remains the sole
    /// writer; waiting for Writing is normal and does not fail other scenes.
    pub async fn tick(&mut self, now: u64) -> Result<Vec<Tick>, String> {
        self.clock(now)?;
        let mut results = vec![];
        for (id, run) in &mut self.runs {
            if !matches!(run.status(), Status::Pending | Status::Running) {
                continue;
            }
            let result = match run.reconcile().await {
                Err(error) => Err(error),
                Ok(()) if run.has_pending() => Ok(vec![]),
                Ok(()) => run.poll(now).await.map(|ids| ids.unwrap_or_default()),
            };
            results.push(Tick {
                run: *id,
                result,
                status: run.status(),
            });
        }
        Ok(results)
    }
    pub(crate) fn matches_admission(&self, admission: &Admission) -> bool {
        self.admission.boot_id() == admission.boot_id()
    }
    pub fn stop_now(&mut self) {
        self.stopped = true;
        for run in self.runs.values_mut() {
            run.cancel_timers();
        }
    }
    /// Disable admission synchronously before the first await. All volatile
    /// timers are invalidated even if cancellation fails or this future is dropped.
    /// Call again to retry cleanup; reopening requires a fresh scheduler.
    pub async fn stop(&mut self) -> Vec<(u64, Result<usize, String>)> {
        self.stopped = true;
        let mut counts = BTreeMap::new();
        for (id, run) in &mut self.runs {
            counts.insert(*id, run.cancel_timers());
        }
        let mut results = vec![];
        for (id, run) in &mut self.runs {
            let cancelled = run.cancel().await.map(|count| count + counts[id]);
            // A changed generation may reject cancellation after the transport
            // owner already made the commands terminal. Still settle from audit.
            let reconciled = run.reconcile().await;
            results.push((
                *id,
                match (cancelled, reconciled) {
                    (Err(error), _) | (_, Err(error)) => Err(error),
                    (Ok(count), Ok(())) => Ok(count),
                },
            ));
        }
        results
    }
    /// Consume terminal named results once. No replay after restart; the durable
    /// receipt remains the authoritative historical record.
    pub fn take_results(&mut self) -> Vec<SceneResult> {
        let mut results = std::mem::take(&mut self.results);
        for (id, run) in &mut self.runs {
            if let Some((definition, audit_id, status)) = run.take_result() {
                results.push(SceneResult {
                    run: *id,
                    definition,
                    audit_id,
                    status,
                });
            }
        }
        results
    }
    /// Observed target projection for a retained run; never inferred from status.
    pub async fn activity(&self, id:u64, feedback:&crate::feedback::Feedback, max_age_ms:u64) -> Result<Option<crate::scene_activity::SceneActivity>,String> {
        match self.runs.get(&id) {
            Some(run)=>run.activity(feedback,max_age_ms).await.map(Some),
            None=>Ok(None),
        }
    }
    pub fn status(&self, id: u64) -> Option<Status> {
        self.runs.get(&id).map(SceneRuntime::status)
    }
    pub async fn cancel(&mut self, id: u64) -> Result<usize, String> {
        self.runs
            .get_mut(&id)
            .ok_or("unknown scene run")?
            .cancel()
            .await
    }
    /// Explicit retirement prevents silently losing a terminal run's result.
    pub fn retire(&mut self, id: u64) -> Result<Status, String> {
        let status = self.status(id).ok_or("unknown scene run")?;
        if matches!(status, Status::Pending | Status::Running) {
            return Err("scene still active".into());
        }
        if let Some((definition, audit_id, status)) =
            self.runs.get_mut(&id).and_then(SceneRuntime::take_result)
        {
            self.results.push(SceneResult {
                run: id,
                definition,
                audit_id,
                status,
            });
        }
        self.runs.remove(&id);
        Ok(status)
    }
}
