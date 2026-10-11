//! Explicitly driven long-press tasks. No subscriptions, reconnect or replay.
use crate::admission::{Admission, TaskGeneration};
use homesense_core::{Command, DeviceId, LevelTransition, Observation, Percent};
use homesense_hap::requests::{Batch, Intent};
use homesense_state::{DeviceState, Quality};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Clone, Copy)]
pub enum ReleasePolicy {
    CancelPendingOnly,
    PauseRampFieldVerified,
}
struct Task {
    panel: DeviceId,
    generation: TaskGeneration,
    manual_fence: u64,
    level: u8,
    up: bool,
    next: u64,
    deadline: u64,
    runs: Vec<u64>,
    policy: ReleasePolicy,
    stopping: bool,
    initial_feedback: Option<crate::feedback::RuleLease>,
}
#[derive(Debug, Default)]
pub struct ReleaseReport {
    pub commands: Vec<u64>,
    pub errors: Vec<(DeviceId, String)>,
}
pub struct HoldDimming {
    admission: Admission,
    controller: Uuid,
    tasks: BTreeMap<DeviceId, Task>,
    now: u64,
    interval: u64,
    increment: u8,
}
impl HoldDimming {
    pub fn new(
        admission: Admission,
        controller: Uuid,
        now: u64,
        interval_ms: u64,
        increment: u8,
    ) -> Result<Self, String> {
        if !(100..=1000).contains(&interval_ms) || !(1..=20).contains(&increment) {
            return Err("invalid dimming bus budget".into());
        }
        Ok(Self {
            admission,
            controller,
            tasks: BTreeMap::new(),
            now,
            interval: interval_ms,
            increment,
        })
    }
    pub async fn start(
        &mut self,
        panel: DeviceId,
        device: DeviceId,
        up: bool,
        state: &DeviceState,
        now: u64,
        policy: ReleasePolicy,
    ) -> Result<(), String> {
        if now < self.now || now > u64::MAX - 10000 {
            return Err("invalid hold clock".into());
        }
        if self.tasks.contains_key(&device) || self.tasks.len() >= 8 {
            return Err("dimming task already active or capacity reached".into());
        }
        if state.kind != homesense_core::ModuleKind::Dimmer
            || state.quality != Quality::Confirmed
            || state.pending_command.is_some()
            || !state
                .observed_at_ms
                .is_some_and(|t| t <= now && now - t <= 2000)
        {
            return Err("fresh settled brightness required".into());
        }
        let Some(Observation::Level(level)) = state.observed else {
            return Err("brightness unavailable".into());
        };
        let generation = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let manual_fence = self
            .admission
            .manual_fence()
            .await
            .map_err(|e| format!("{e:?}"))?;
        self.now = now;
        self.tasks.insert(
            device,
            Task {
                panel,
                generation,
                manual_fence,
                level: level.get(),
                up,
                next: now,
                deadline: now + 10000,
                runs: vec![],
                policy,
                stopping: false,
                initial_feedback: None,
            },
        );
        Ok(())
    }
    /// Live adapter entry: initial level and first command share the normal
    /// feedback lease/pending-journal checks at the point of admission.
    // Explicit control and feedback lease inputs belong to one admission boundary.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_feedback(
        &mut self,
        panel: DeviceId,
        device: DeviceId,
        up: bool,
        feedback: &crate::feedback::Feedback,
        now: u64,
        max_age: u64,
        policy: ReleasePolicy,
    ) -> Result<(), String> {
        use homesense_core::rule::{
            Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable,
        };
        if max_age == 0 {
            return Err("hold feedback age must be positive".into());
        }
        let snapshot = feedback.snapshot();
        let state = snapshot.devices.get(&device).ok_or("hold device missing")?;
        let (plan, lease) = feedback.plan_rule(
            &[Node {
                source: "hold-initial-level".into(),
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
            self.admission.monotonic_ms(),
            max_age.min(2000),
        )?;
        if !matches!(plan.variables["initial"].value, Some(Value::Integer(_))) {
            return Err("fresh settled initial brightness unavailable".into());
        }
        self.start(panel, device.clone(), up, state, now, policy)
            .await?;
        self.tasks
            .get_mut(&device)
            .ok_or("hold task missing")?
            .initial_feedback = Some(lease);
        Ok(())
    }
    /// At most one sample per active channel per call; missed samples never burst.
    pub async fn tick(&mut self, now: u64) -> Result<Vec<u64>, String> {
        if now < self.now {
            return Err("hold clock reversed".into());
        }
        self.now = now;
        let generation = match self.admission.task_generation().await {
            Ok(g) => g,
            Err(e) => {
                self.tasks.clear();
                return Err(format!("hold disconnected: {e:?}"));
            }
        };
        // Mark every expired timer before awaiting any stop operation. A
        // rejected pause must not leave another expired channel generating work.
        for task in self.tasks.values_mut() {
            if now >= task.deadline {
                task.stopping = true;
            }
        }
        let devices: Vec<_> = self.tasks.keys().cloned().collect();
        let mut written = vec![];
        let mut errors = vec![];
        for device in devices {
            if self.tasks[&device].generation != generation {
                self.tasks.remove(&device);
                continue;
            }
            if now >= self.tasks[&device].deadline {
                match self.stop(&device).await {
                    Ok(ids) => written.extend(ids),
                    Err(error) => errors.push((device, error)),
                }
                continue;
            }
            let task = self.tasks.get_mut(&device).unwrap();
            if task.stopping || now < task.next {
                continue;
            }
            let level = if task.up {
                task.level.saturating_add(self.increment).min(100)
            } else {
                task.level.saturating_sub(self.increment)
            };
            if level == task.level {
                continue;
            }
            let batch = Batch {
                controller: self.controller,
                intents: vec![Intent {
                    device: device.clone(),
                    command: Command::SetLevel {
                        level: Percent::new(level).map_err(|e| e.to_string())?,
                        transition: LevelTransition::Immediate,
                    },
                }],
                subscriptions: vec![],
            };
            // If the caller cancels this await, the outcome may be ambiguous.
            // Leave the task stopped rather than issuing the same target again.
            task.stopping = true;
            let ids = self
                .admission
                .submit_scene_with_feedback(
                    task.generation.clone(),
                    batch,
                    task.manual_fence,
                    task.initial_feedback.take(),
                )
                .await
                .map_err(|e| {
                    task.stopping = true;
                    format!("hold admission: {e:?}; task stopped without replay")
                })?;
            task.stopping = false;
            task.runs.push(ids[0]);
            task.level = level;
            task.next = now + self.interval;
            written.extend(ids);
        }
        if errors.is_empty() {
            Ok(written)
        } else {
            Err(format!(
                "hold deadline errors={errors:?}; admitted commands={written:?}"
            ))
        }
    }
    pub async fn release(&mut self, panel: &DeviceId) -> Result<Vec<u64>, String> {
        let report = self.release_detailed(panel).await;
        if report.errors.is_empty() {
            Ok(report.commands)
        } else {
            Err(format!(
                "hold release errors={:?}; admitted commands={:?}",
                report.errors, report.commands
            ))
        }
    }
    /// Stop every matching timer before awaiting cancellation. A failed device
    /// cannot leave another matching channel continuing to generate samples.
    pub async fn release_detailed(&mut self, panel: &DeviceId) -> ReleaseReport {
        let devices: Vec<_> = self
            .tasks
            .iter_mut()
            .filter_map(|(id, task)| {
                if &task.panel == panel {
                    task.stopping = true;
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut report = ReleaseReport::default();
        for device in devices {
            match self.stop(&device).await {
                Ok(ids) => report.commands.extend(ids),
                Err(error) => report.errors.push((device, error)),
            }
        }
        report
    }
    async fn stop(&mut self, device: &DeviceId) -> Result<Vec<u64>, String> {
        let task = self.tasks.get_mut(device).ok_or("hold task missing")?;
        task.stopping = true;
        for run in &task.runs {
            self.admission
                .cancel_scene_run(self.controller, task.generation.clone(), *run)
                .await
                .map_err(|e| format!("hold cancel: {e:?}"))?;
        }
        let ids = match task.policy {
            ReleasePolicy::CancelPendingOnly => vec![],
            ReleasePolicy::PauseRampFieldVerified => self
                .admission
                .submit_scene(
                    task.generation.clone(),
                    Batch {
                        controller: self.controller,
                        intents: vec![Intent {
                            device: device.clone(),
                            command: Command::PauseRamp,
                        }],
                        subscriptions: vec![],
                    },
                    task.manual_fence,
                )
                .await
                .map_err(|e| format!("hold pause: {e:?}"))?,
        };
        self.tasks.remove(device);
        Ok(ids)
    }
    pub fn invalidate(&mut self) {
        for task in self.tasks.values_mut() {
            task.stopping = true;
        }
    }
    pub fn active_count(&self) -> usize {
        self.tasks.len()
    }
}
