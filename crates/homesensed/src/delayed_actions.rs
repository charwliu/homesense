//! Bounded delayed light actions. Scheduling is volatile; only admission is durable.
//! No detached tasks or startup replay. Cancellation cannot undo an admitted action.
use crate::admission::{Admission, TaskGeneration};
use hap::server::WriteStatus;
use homesense_hap::requests::Batch;
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// Owned by one scene/rule run. Dropping or cancelling it cancels its waiting work.
/// This supplies the timer boundary; it does not interpret Control4 programs.
pub struct DelayedActions {
    admission: Admission,
    slots: Arc<Semaphore>,
    cancellation: watch::Sender<bool>,
}
pub struct PendingAction {
    admission: Admission,
    generation: TaskGeneration,
    generations: watch::Receiver<Option<TaskGeneration>>,
    batch: Batch,
    deadline: tokio::time::Instant,
    cancellation: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
}
impl DelayedActions {
    pub fn new(admission: Admission, capacity: usize) -> Result<Self, &'static str> {
        if !(1..=4096).contains(&capacity) {
            return Err("delayed action capacity must be 1..=4096");
        }
        let (cancellation, _) = watch::channel(false);
        Ok(Self {
            admission,
            slots: Arc::new(Semaphore::new(capacity)),
            cancellation,
        })
    }
    /// Capture the generation before the delay. The caller drives PendingAction::wait;
    /// dropping that future drops the timer and releases capacity.
    pub async fn schedule(
        &self,
        delay: Duration,
        batch: Batch,
    ) -> Result<PendingAction, WriteStatus> {
        if *self.cancellation.borrow() {
            return Err(WriteStatus::ServiceCommunicationFailure);
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(delay)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| WriteStatus::ResourceBusy)?;
        let generation = self.admission.task_generation().await?;
        Ok(PendingAction {
            admission: self.admission.clone(),
            generation,
            generations: self.admission.generation_updates(),
            batch,
            deadline,
            cancellation: self.cancellation.subscribe(),
            _slot: slot,
        })
    }
    /// Terminal for this run. Callers must create a new run for new work.
    pub fn cancel(&self) {
        self.cancellation.send_replace(true);
    }
}
impl Drop for DelayedActions {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl PendingAction {
    /// Success means durable command admission, not physical execution.
    /// Once submission starts, cancellation must be handled by command arbitration;
    /// do not report it as a cancelled timer or retry after a lost acknowledgement.
    pub async fn wait(mut self) -> Result<Vec<u64>, WriteStatus> {
        if *self.cancellation.borrow() {
            return Err(WriteStatus::ServiceCommunicationFailure);
        }
        tokio::select! {
            biased;
            _ = self.cancellation.changed() => return Err(WriteStatus::ServiceCommunicationFailure),
            _ = self.generations.wait_for(|current| current.as_ref() != Some(&self.generation)) => return Err(WriteStatus::ServiceCommunicationFailure),
            _ = tokio::time::sleep_until(self.deadline) => {}
        }
        if *self.cancellation.borrow() {
            return Err(WriteStatus::ServiceCommunicationFailure);
        }
        self.admission
            .submit_delayed(self.generation, self.batch)
            .await
    }
}
