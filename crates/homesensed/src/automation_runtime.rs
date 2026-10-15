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

/// Supervisors must distinguish a process start from connection recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPolicy {
    Run,
    Suppress,
}

pub struct AutomationRuntime {
    admission: Admission,
    control: crate::automation_control::Control,
    requests: crate::automation_control::Receiver,
    generation: TaskGeneration,
    publication_paused: bool,
    handoff_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
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
        Self::load_with_startup(admission, controller, wall_unix_ms, lateness_ms, scene_capacity, budget, StartupPolicy::Run).await
    }
    /// Suppress startup actions when rebuilding after a connection recovery.
    /// This does not recover a stopped Admission or restore previous tasks.
    pub async fn load_with_startup(
        admission: Admission,
        controller: Uuid,
        wall_unix_ms: i64,
        lateness_ms: u32,
        scene_capacity: usize,
        budget: Budget,
        startup: StartupPolicy,
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
        let mut calendar = ScheduledRules::new(
            configured.events,
            document.schedules.clone(),
            wall_unix_ms,
            lateness_ms,
        )
        .await?;
        if startup == StartupPolicy::Suppress { calendar.suppress_startup(); }
        let (control, requests) =
            crate::automation_control::channel(controller, generation.clone());
        let mut result = Self {
            control,
            requests,
            admission,
            generation,
            publication_paused: false,
            handoff_gate: std::sync::Arc::new(tokio::sync::Mutex::new(())),
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
    pub(crate) fn handoff_gate(&self) -> std::sync::Arc<tokio::sync::Mutex<()>> {self.handoff_gate.clone()}
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
        if crate::scene_switches::related(&self.document,id){crate::scene_switches::activate_forced(&self.document,id,&self.admission,&mut self.scenes,self.requests.controller()).await}
        else{self.scenes.start_document(&self.document,id,self.admission.monotonic_ms()).await}
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
        match self.drive_classified(owner, cadence, max_age_ms, stop, wall_clock).await? {
            crate::automation_drive::Exit::Disconnected { gateway } => Err(format!("gateway disconnected: {gateway}")),
            _ => Ok(()),
        }
    }
    /// Distinguishes audited EOF from other failures; the old owner remains stopped.
    pub async fn drive_classified<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration,
        max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>,
        wall_clock: impl FnMut() -> Result<i64, String>,
    ) -> Result<crate::automation_drive::Exit, String> {
        self.validate().await?;
        let outcome = crate::automation_drive::drive_with_panel_feeds(
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
            Some(&mut self.causal), false, None, vec![], Some(self.handoff_gate.clone()),
        )
        .await?;
        self.publication_paused = matches!(outcome, crate::automation_drive::Exit::PublicationPaused);
        Ok(outcome)
    }
    /// Return an audited disconnect without permanently closing Admission.
    /// The supervisor must replace streams and synchronize fresh feedback before
    /// loading a new runtime with StartupPolicy::Suppress. Cancellation stays terminal.
    pub async fn drive_recoverable<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration,
        max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>,
        wall_clock: impl FnMut() -> Result<i64, String>,
    ) -> Result<crate::automation_drive::Exit, String> {
        self.drive_recoverable_ready(owner,cadence,max_age_ms,stop,wall_clock,None).await
    }
    pub(crate) async fn drive_recoverable_ready<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self, owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration, max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>, wall_clock: impl FnMut() -> Result<i64,String>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<crate::automation_drive::Exit,String> {
        self.drive_recoverable_projected(owner,cadence,max_age_ms,stop,wall_clock,ready,None).await
    }
    /// Recoverable driver with optional independent HAP projection. The caller
    /// owns graph retirement and must synchronize the replacement before reuse.
    #[allow(clippy::too_many_arguments)]
    pub async fn drive_recoverable_projected<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self, owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration, max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>, wall_clock: impl FnMut() -> Result<i64,String>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
        projection: Option<tokio::sync::watch::Sender<homesense_state::Snapshot>>,
    ) -> Result<crate::automation_drive::Exit,String> {
        self.drive_recoverable_published(owner,cadence,max_age_ms,stop,wall_clock,ready,projection,vec![]).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn drive_recoverable_published<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self, owner: &mut crate::observed_transport::ObservedTransport<S>,
        cadence: std::time::Duration, max_age_ms: u64,
        stop: tokio::sync::watch::Receiver<bool>, wall_clock: impl FnMut() -> Result<i64,String>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
        projection: Option<tokio::sync::watch::Sender<homesense_state::Snapshot>>,
        panel_feeds:Vec<crate::runtime_panel_hap::Feed>,
    ) -> Result<crate::automation_drive::Exit,String> {
        self.validate().await?;
        if let Some(ready)=ready {let _=ready.send(());}

        let outcome = crate::automation_drive::drive_with_panel_feeds(
            &self.admission, owner, &mut self.panels, &mut self.scenes,
            Some(&mut self.calendar), cadence, max_age_ms, stop, wall_clock,
            Some((&mut self.requests, &self.document)), Some(&mut self.causal), true, projection, panel_feeds, Some(self.handoff_gate.clone()),
        ).await?;
        self.publication_paused = matches!(outcome,
            crate::automation_drive::Exit::PublicationPaused | crate::automation_drive::Exit::Disconnected { .. });
        Ok(outcome)
    }
    /// Drive one transport owner and publish light/scene updates outside HAP locks.
    pub async fn drive_hap<S:std::io::Read+std::io::Write+Send+'static>(
        &mut self,owner:&mut crate::observed_transport::ObservedTransport<S>,
        projection:tokio::sync::watch::Sender<homesense_state::Snapshot>,stop:tokio::sync::watch::Receiver<bool>,
        wall_clock:impl FnMut()->Result<i64,String>,
    )->Result<(),String> {
        self.validate().await?;
        let result=crate::automation_drive::drive_with_panel_feeds(&self.admission,owner,&mut self.panels,&mut self.scenes,
            Some(&mut self.calendar),std::time::Duration::from_millis(10),5000,stop,wall_clock,
            Some((&mut self.requests,&self.document)),Some(&mut self.causal),false,Some(projection),vec![],Some(self.handoff_gate.clone())).await?;
        self.publication_paused=matches!(result,crate::automation_drive::Exit::PublicationPaused);
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
