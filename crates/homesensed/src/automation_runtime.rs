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
    generation: TaskGeneration,
    document: Document,
    pub(crate) panels: PanelRules,
    pub(crate) calendar: ScheduledRules,
    pub(crate) scenes: SceneScheduler,
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
        let configured =
            ConfiguredRules::load_document(admission.clone(), controller, &document, budget)
                .await?;
        let calendar = ScheduledRules::new(
            configured.events,
            document.schedules.clone(),
            wall_unix_ms,
            lateness_ms,
        )
        .await?;
        let mut result = Self {
            admission,
            generation,
            document,
            panels: configured.panels,
            calendar,
            scenes,
        };
        if let Err(error) = result.validate().await {
            result.stop();
            return Err(error);
        }
        Ok(result)
    }
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
    /// Explicit manual entry; loading a document never starts any scene.
    pub async fn start_scene(&mut self, id: &str) -> Result<u64, String> {
        self.validate().await?;
        self.scenes
            .start_document(&self.document, id, self.admission.monotonic_ms())
            .await
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
        crate::automation_drive::drive_with_calendar(
            &self.admission,
            owner,
            &mut self.panels,
            &mut self.scenes,
            Some(&mut self.calendar),
            cadence,
            max_age_ms,
            stop,
            wall_clock,
        )
        .await
    }
    fn stop(&mut self) {
        self.panels.disconnect();
        self.calendar.stop();
        self.scenes.stop_now();
        self.admission.stop_now();
    }
}
impl Drop for AutomationRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}
