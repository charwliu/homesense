//! One service-owned group of software fades. Limits are the bus capacity
//! allocated to this group, not a claim about traffic from other producers.
use crate::{admission::Admission, feedback::Feedback, software_fade::SoftwareFade};
use homesense_core::{DeviceId, Percent};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Clone, Copy)]
pub struct Limits {
    pub tasks: usize,
    pub samples: u64,
    pub targets_per_second: u64,
}
#[derive(Clone, Copy)]
pub struct Request {
    pub target: Percent,
    pub duration_ms: u64,
    pub interval_ms: u64,
    pub lateness_ms: u64,
}
struct Task {
    fade: SoftwareFade,
    samples: u64,
    rate: u64,
}
pub struct FadeGroup {
    admission: Admission,
    controller: Uuid,
    limits: Limits,
    tasks: BTreeMap<DeviceId, Task>,
    stopped: bool,
}
impl FadeGroup {
    pub fn new(admission: Admission, controller: Uuid, limits: Limits) -> Result<Self, String> {
        if limits.tasks == 0
            || limits.tasks > 8
            || limits.samples == 0
            || limits.samples > 1024
            || limits.targets_per_second == 0
            || limits.targets_per_second > 20
        {
            return Err("invalid fade group allocation".into());
        }
        Ok(Self {
            admission,
            controller,
            limits,
            tasks: BTreeMap::new(),
            stopped: false,
        })
    }
    pub fn active_count(&self) -> usize {
        self.tasks.len()
    }
    pub async fn start(
        &mut self,
        device: DeviceId,
        request: Request,
        feedback: &Feedback,
        max_age: u64,
    ) -> Result<(), String> {
        if self.stopped || self.tasks.contains_key(&device) || self.tasks.len() >= self.limits.tasks
        {
            return Err("fade group stopped, duplicate device or task limit".into());
        }
        // Validate the worst-case plan before reserving or reading live state.
        let plan = homesense_core::fade::Fade::new(
            Percent::new(0).unwrap(),
            Percent::new(100).unwrap(),
            0,
            request.duration_ms,
            request.interval_ms,
            request.lateness_ms,
            self.limits.samples,
        )
        .map_err(|e| format!("{e:?}"))?;
        let samples = plan.reserved_samples();
        let rate = 1000u64.div_ceil(request.interval_ms);
        if self.tasks.values().map(|t| t.samples).sum::<u64>() + samples > self.limits.samples
            || self.tasks.values().map(|t| t.rate).sum::<u64>() + rate
                > self.limits.targets_per_second
        {
            return Err("fade group bus allocation exhausted".into());
        }
        let fade = SoftwareFade::start(
            self.admission.clone(),
            self.controller,
            device.clone(),
            request.target,
            request.duration_ms,
            request.interval_ms,
            request.lateness_ms,
            samples,
            feedback,
            max_age,
        )
        .await?;
        self.tasks.insert(
            device,
            Task {
                fade,
                samples,
                rate,
            },
        );
        Ok(())
    }
    pub async fn cancel(&mut self, device: &DeviceId) -> Result<(), String> {
        if let Some(task) = self.tasks.get_mut(device) {
            task.fade.cancel().await?;
        }
        self.tasks.remove(device);
        Ok(())
    }
    pub async fn cancel_all(&mut self) -> Result<(), String> {
        self.stopped = true;
        let mut errors = vec![];
        for device in self.tasks.keys().cloned().collect::<Vec<_>>() {
            if let Err(error) = self.cancel(&device).await {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            self.admission.stop_now();
            Err(errors.join("; "))
        }
    }
    /// One coalesced sample per task at most. Any failure ends this group and
    /// attempts cancellation of every other task, never retries the failed target.
    pub async fn tick(&mut self) -> Result<Vec<DeviceId>, String> {
        if self.stopped {
            return Err("fade group stopped".into());
        }
        let mut completed = vec![];
        for device in self.tasks.keys().cloned().collect::<Vec<_>>() {
            match self.tasks.get_mut(&device).unwrap().fade.tick().await {
                Ok(true) => {
                    self.tasks.remove(&device);
                    completed.push(device);
                }
                Ok(false) => {}
                Err(error) => {
                    let cleanup = self.cancel_all().await.err();
                    return Err(format!("fade {device:?}: {error}; cleanup={cleanup:?}"));
                }
            }
        }
        Ok(completed)
    }
}
