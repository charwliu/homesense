//! Explicit assembly from the admission owner's published configuration bundle.
//! Loading starts no connections, subscriptions, timers or startup actions.
use crate::{
    admission::{Admission, TaskGeneration},
    configured_rules::ConfiguredRules,
    event_rules::ScheduledRules,
    panel_rules::PanelRules,
    scene_scheduler::SceneScheduler,
};
use homesense_config::rule_document::Document;
use homesense_core::rule::Budget;
use uuid::Uuid;

pub struct AutomationRuntime {
    admission: Admission,
    control: crate::automation_control::Control,
    requests: crate::automation_control::Receiver,
    generation: TaskGeneration,
    publication_paused: bool,
    document: Document,
    pub(crate) panels: PanelRules,
    pub(crate) calendar: ScheduledRules,
    pub(crate) scenes: SceneScheduler,
    causal: crate::deferred_router::Router,
    budget: Budget,
}
impl AutomationRuntime {
    pub async fn load(
        admission: Admission,
        controller: Uuid,
        wall_unix_ms: i64,
        lateness_ms: u32,
        scene_capacity: usize,
        budget: Budget,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let bundle = admission
            .automation_bundle(controller, generation.clone())
            .await
            .map_err(|e| format!("{e:?}"))?;
        let document = bundle
            .automation
            .ok_or("published automation document required")?;
        // Validate limits before initialization can declare boot-local variables.
        homesense_core::calendar::Calendar::new(
            document.schedules.clone(),
            wall_unix_ms,
            admission.monotonic_ms(),
            None,
            lateness_ms,
        )
        .map_err(|e| format!("{e:?}"))?;
        let scenes = SceneScheduler::new(
            admission.clone(),
            controller,
            scene_capacity,
            admission.monotonic_ms(),
        )?;
        let causal = crate::deferred_router::Router::new(admission.clone(), controller, &scenes, 64)?;
        let configured =
            ConfiguredRules::load_causal_document(admission.clone(), controller, &document, budget)
                .await?;
        let calendar = ScheduledRules::new(
            configured.events,
            document.schedules.clone(),
            wall_unix_ms,
            lateness_ms,
        )
        .await?;
        let (control, requests) =
            crate::automation_control::channel(controller, generation.clone());
        let mut result = Self {
            control,
            requests,
            admission,
            generation,
            publication_paused: false,
            document,
            panels: configured.panels,
            calendar,
            scenes,
            causal,
            budget,
        };
        if let Err(error) = result.validate().await {
            result.stop();
            return Err(error);
        }
        Ok(result)
    }
    pub fn control(&self) -> crate::automation_control::Control {
        self.control.clone()
    }
    pub fn publication_paused(&self) -> bool {self.publication_paused}
    pub fn revision(&self) -> u64 {
        self.document.revision
    }
    async fn validate(&self) -> Result<(), String> {
        if self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?
            != self.generation
        {
            return Err("automation assembly generation expired".into());
        }
        Ok(())
    }
    /// Explicit rule root using the same bounded causal owner as loop results.
    pub async fn start_rule(&mut self, id: &str, feedback: Option<(&crate::feedback::Feedback,u64)>) -> Result<(u64,Vec<crate::deferred_rule::Outcome>),String> {
        self.validate().await?;
        self.causal.start(id, &mut self.scenes, feedback, self.budget, 60_000).await
    }
    pub async fn cancel_rule(&mut self, id:u64)->Result<usize,String> {
        self.validate().await?;
        self.causal.cancel(id,&mut self.scenes).await
    }
    /// Explicit manual entry; loading a document never starts any scene.
    pub async fn start_scene(&mut self, id: &str) -> Result<u64, String> {
        self.validate().await?;
        self.scenes
            .start_document(&self.document, id, self.admission.monotonic_ms())
            .await
    }
    /// Cancel future and queued output; already-writing commands cannot be undone.
    pub async fn cancel_scene(&mut self, run: u64) -> Result<usize, String> {
        self.validate().await?;
        self.scenes.cancel(run).await
    }
    pub fn scene_status(&self, run: u64) -> Option<homesense_core::scene::Status> {
        self.scenes.status(run)
    }
    /// Explicit retirement releases bounded scheduler capacity. Durable history
    /// remains available through Admission after the in-memory handle is retired.
    pub fn retire_scene(&mut self, run: u64) -> Result<homesense_core::scene::Status, String> {
        self.scenes.retire(run)
    }
    /// Graceful shutdown checkpoints cancellation before disabling admission.
    /// Dropping this future still disables all subsequent transport dispatch.
    pub async fn shutdown(&mut self) -> Vec<(u64, Result<usize, String>)> {
        struct Stop(Admission);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.stop_now();
            }
        }
        let _stop = Stop(self.admission.clone());
        self.requests.close();
        self.panels.disconnect();
        self.calendar.stop();
        if let Err(error) = self.causal.close(&mut self.scenes).await {
            log::warn!("causal shutdown failed: {error}");
        }
        self.scenes.stop().await
    }
    pub async fn drive<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration,
        max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>,
        wall_clock: impl FnMut() -> Result<i64, String>,
    ) -> Result<(), String> {
        self.validate().await?;
        let paused = crate::automation_drive::drive_controlled(
            &self.admission,
            owner,
            &mut self.panels,
            &mut self.scenes,
            Some(&mut self.calendar),
            cadence,
            max_age_ms,
            stop,
            wall_clock,
            Some((&mut self.requests, &self.document)),
            Some(&mut self.causal),
        )
        .await?;
        self.publication_paused = paused;
        Ok(())
    }
    fn stop(&mut self) {
        self.requests.close();
        self.panels.disconnect();
        self.calendar.stop();
        self.scenes.stop_now();
        if !self.publication_paused {
            self.admission.stop_now();
        }
    }
}
impl Drop for AutomationRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}
