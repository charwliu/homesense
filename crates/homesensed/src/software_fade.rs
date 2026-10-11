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
    generation: TaskGeneration,
    fence: u64,
    fade: Fade,
    expires: u64,
    lease: Option<RuleLease>,
    pending: Vec<u64>,
    closed: bool,
    in_flight: bool,
    failure: Option<String>,
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
            generation,
            fence,
            fade,
            expires,
            lease: Some(lease),
            pending: vec![],
            closed: false,
            in_flight: false,
            failure: None,
        })
    }
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
                .submit_scene_with_feedback_before(
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
                )
                .await;
            self.in_flight = false;
            match result {
                Ok(ids) => self.pending = ids,
                Err(error) => {
                    self.fade.cancel();
                    self.admission.stop_now();
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
            if statuses
                .iter()
                .any(|s| matches!(s, Status::Accepted | Status::Writing))
            {
                self.admission
                    .cancel_scene_run(self.controller, self.generation.clone(), run)
                    .await
                    .map_err(|e| format!("fade cancel: {e:?}"))?;
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
