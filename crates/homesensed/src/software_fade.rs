//! Explicit single-device fade driver. Caller owns polling and transport progress.
//! No automatic registration, field connection, retry or physical completion claim.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
};
use homesense_core::{
    Command, DeviceId, LevelTransition, Percent,
    fade::Fade,
    rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::journal::Status;
use uuid::Uuid;

pub struct SoftwareFade {
    admission: Admission,
    controller: Uuid,
    device: DeviceId,
    target: Percent,
    output_possible: bool,
    rate: u64,
    generation: TaskGeneration,
    fence: u64,
    fade: Fade,
    expires: u64,
    lease: Option<RuleLease>,
    pending: Vec<u64>,
    closed: bool,
    in_flight: bool,
    failure: Option<String>,
    manually_cancelled: bool,
    scene: Option<homesense_storage::scene_run::StepLink>,
}
impl SoftwareFade {
    /// The caller must reserve `ceil(duration/interval)` output slots against
    /// the bus-wide budget before starting. This component permits one in-flight
    /// sample; the service must prevent multiple owners for the same device.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        admission: Admission,
        controller: Uuid,
        device: DeviceId,
        target: Percent,
        duration: u64,
        interval: u64,
        lateness: u64,
        budget: u64,
        feedback: &Feedback,
        max_age: u64,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let fence = admission
            .manual_fence()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let now = admission.monotonic_ms();
        let (plan, lease) = feedback.plan_rule(
            &[Node {
                source: "fade-initial".into(),
                action: Action::Set {
                    variable: "initial".into(),
                    value: Expr::DeviceLevel(device.clone()),
                },
            }],
            &[(
                "initial".into(),
                Variable {
                    value_type: ValueType::Integer,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            )]
            .into(),
            Budget::default(),
            now,
            max_age,
        )?;
        let Some(Value::Integer(initial)) = plan.variables["initial"].value else {
            return Err("fresh initial fade level required".into());
        };
        let initial = Percent::new(u8::try_from(initial).map_err(|_| "invalid fade level")?)
            .map_err(|e| e.to_string())?;
        let fade = Fade::new(initial, target, now, duration, interval, lateness, budget)
            .map_err(|e| format!("{e:?}"))?;
        let expires = now
            .checked_add(duration)
            .and_then(|v| v.checked_add(lateness))
            .ok_or("fade deadline overflow")?;
        Ok(Self {
            admission,
            controller,
            device,
            target,
            output_possible: false,
            rate: 1000u64.div_ceil(interval),
            generation,
            fence,
            fade,
            expires,
            lease: Some(lease),
            pending: vec![],
            closed: false,
            in_flight: false,
            failure: None,
            manually_cancelled: false,
            scene: None,
        })
    }
    /// Associate samples with an already accepted scene before any output.
    /// The scene owner retains responsibility for its durable lifecycle and
    /// cancellation. This association never creates or restarts a scene.
    pub fn bind_scene(&mut self, run: String, index: usize, source: String) -> Result<(), String> {
        if self.scene.is_some() || self.lease.is_none() || self.closed || self.failure.is_some()
            || run.is_empty() || run.len()>128 || run.chars().any(char::is_control)
            || index>=4096 || source.is_empty() || source.len()>8192 {
            return Err("invalid or late fade scene association".into());
        }
        self.scene=Some(homesense_storage::scene_run::StepLink {run,index,source,sample:Some(0)});
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inherit_scene(
        &mut self, generation: &TaskGeneration, controller: Uuid,
        fence: u64, deadline: Option<u64>, step: &homesense_core::scene::Step,
        run: String, index: usize,
    ) -> Result<(), String> {
        if &self.generation != generation || self.controller != controller
            || self.device != step.device
            || !matches!(step.command, Command::SetLevel {level, ..} if level == self.target) {
            return Err("fade does not match owning scene step".into());
        }
        self.bind_scene(run,index,step.source.clone())?;
        self.fence = fence;
        if let Some(deadline) = deadline { self.expires = self.expires.min(deadline); }
        Ok(())
    }
    pub(crate) fn reservation(&self) -> (u64,u64) {
        (self.fade.reserved_samples(),self.rate)
    }
    pub(crate) fn output_possible(&self) -> bool { self.output_possible }
    pub(crate) fn manually_cancelled(&self) -> bool { self.manually_cancelled }
    /// Returns true when quiescent after successful writes or explicit cancellation. Device
    /// activity must still be derived from fresh physical feedback separately.
    pub async fn tick(&mut self) -> Result<bool, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.closed {
            return Ok(true);
        }
        if self.in_flight {
            self.admission.stop_now();
            self.closed = true;
            self.failure = Some("fade admission outcome unknown".into());
            return Err(self.failure.clone().unwrap());
        }
        if let Some(scene) = &self.scene {
            let result = self.admission.scene_history(
                self.controller, self.generation.clone(),
                homesense_storage::scene_run::Query::Run {
                    boot: self.admission.boot_id().into(), run: scene.run.clone(),
                    after: 0, limit: 1,
                },
            ).await;
            match result {
                Ok(Some(page)) if !page.record.status.terminal() => {}
                Ok(_) => {
                    self.failure = Some("fade scene no longer active".into());
                    self.cancel().await?;
                    return Err(self.failure.clone().unwrap());
                }
                Err(error) => {
                    self.failure = Some(format!("fade scene lookup: {error:?}"));
                    self.admission.stop_now();
                    self.closed = true;
                    return Err(self.failure.clone().unwrap());
                }
            }
        }
        let superseded = self.admission.manual_superseded(self.device.clone(), self.fence).await.map_err(|e| {
            let error = format!("fade manual fence: {e:?}");
            self.failure = Some(error.clone());
            error
        })?;
        if superseded {
            self.failure = Some("fade superseded by manual control".into());
            self.cancel().await?;
            self.manually_cancelled = true;
            return Err(self.failure.clone().unwrap());
        }
        if !self.pending.is_empty() {
            let statuses = self
                .admission
                .command_statuses(
                    self.controller,
                    self.generation.clone(),
                    self.pending.clone(),
                )
                .await
                .map_err(|e| {
                    let error = format!("fade status: {e:?}");
                    self.failure = Some(error.clone());
                    error
                })?;
            self.output_possible |= statuses.iter().any(|s| matches!(s, Status::Writing | Status::Written | Status::Observed | Status::Failed | Status::Uncertain));
            if statuses
                .iter()
                .any(|s| matches!(s, Status::Failed | Status::Uncertain | Status::Cancelled))
            {
                self.failure = Some("fade output interrupted".into());
                self.cancel().await?;
                return Err(self.failure.clone().unwrap());
            }
            if statuses
                .iter()
                .any(|s| matches!(s, Status::Accepted | Status::Writing))
            {
                if self.admission.monotonic_ms() > self.expires {
                    self.failure = Some("fade output expired".into());
                    self.cancel().await?;
                    return Err(self.failure.clone().unwrap());
                }
                return Ok(false);
            }
            self.pending.clear();
        }
        if self.fade.finished() {
            self.closed = true;
            return Ok(true);
        }
        let sample = self
            .fade
            .sample(self.admission.monotonic_ms())
            .map_err(|e| {
                let error = format!("fade sample: {e:?}");
                self.failure = Some(error.clone());
                error
            })?;
        if let Some(level) = sample {
            // Cancellation of this await may lose a receipt: the Drop guard closes
            // admission unless the caller completes or explicitly cancels us.
            self.in_flight = true;
            let result = self
                .admission
                .submit_scene_sample(
                    self.generation.clone(),
                    Batch {
                        controller: self.controller,
                        intents: vec![Intent {
                            device: self.device.clone(),
                            command: Command::SetLevel {
                                level,
                                transition: LevelTransition::Immediate,
                            },
                        }],
                        subscriptions: vec![],
                    },
                    self.fence,
                    self.lease.take(),
                    Some(self.expires),
                    self.scene.clone(),
                )
                .await;
            self.in_flight = false;
            match result {
                Ok(ids) => {
                    self.pending = ids;
                    if let Some(scene) = &mut self.scene {
                        scene.sample = scene.sample.map(|sample| sample + 1);
                    }
                },
                Err(error) => {
                    self.fade.cancel();
                    // These replies prove this submission did not commit:
                    // queue capacity or precondition/transaction validation.
                    // A lost reply or storage/transport failure remains fatal.
                    if !matches!(error, hap::server::WriteStatus::ResourceBusy | hap::server::WriteStatus::InvalidValueInRequest) {
                        self.admission.stop_now();
                    }
                    self.closed = true;
                    self.failure = Some(format!("fade admission: {error:?}"));
                    return Err(self.failure.clone().unwrap());
                }
            }
        }
        Ok(false)
    }
    pub async fn cancel(&mut self) -> Result<(), String> {
        self.fade.cancel();
        if self.in_flight {
            self.admission.stop_now();
            self.closed = true;
            self.failure = Some("fade admission outcome unknown".into());
            return Err(self.failure.clone().unwrap());
        }
        if let Some(run) = self.pending.first().copied() {
            let statuses = self
                .admission
                .command_statuses(
                    self.controller,
                    self.generation.clone(),
                    self.pending.clone(),
                )
                .await
                .map_err(|e| format!("fade cancellation status: {e:?}"))?;
            self.output_possible |= statuses.iter().any(|s| matches!(s, Status::Writing | Status::Written | Status::Observed | Status::Failed | Status::Uncertain));
            if statuses
                .iter()
                .any(|s| matches!(s, Status::Accepted | Status::Writing))
            {
                self.admission
                    .cancel_scene_run(self.controller, self.generation.clone(), run)
                    .await
                    .map_err(|e| format!("fade cancel: {e:?}"))?;
            }
            if statuses.contains(&Status::Writing) {
                return Err("fade write still in flight".into());
            }
        }
        self.pending.clear();
        self.closed = true;
        Ok(())
    }
}
impl Drop for SoftwareFade {
    fn drop(&mut self) {
        if !self.closed {
            self.admission.stop_now();
        }
    }
}
