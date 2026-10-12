//! Fresh stream consumption only. No scene or physical command submission.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
};
use hap::server::WriteStatus;
use homesense_config::rule_document::Document;
use homesense_core::scene_observer::Scope;
use homesense_state::{Change, Cursor};

#[derive(Default)]
pub(crate) struct Consumer {
    scope: Option<Scope>,
    cursor: Option<Cursor>,
}
impl Consumer {
    async fn begin(
        &mut self,
        admission: &Admission,
        controller: uuid::Uuid,
        generation: TaskGeneration,
        feedback: &Feedback,
    ) -> Result<bool, String> {
        admission.invalidate_scene_observation_now()?;
        let proof = feedback.scene_observation_start(admission.monotonic_ms())?;
        let cursor = proof.snapshot().cursor;
        match admission
            .begin_scene_observation(controller, generation, proof)
            .await
        {
            Ok(scope) => {
                self.scope = Some(scope);
                self.cursor = Some(cursor);
                Ok(true)
            }
            Err(WriteStatus::ResourceBusy) => Ok(false),
            Err(error) => Err(format!("scene observation start: {error:?}")),
        }
    }
    pub(crate) async fn tick(
        &mut self,
        admission: &Admission,
        controller: uuid::Uuid,
        document: &Document,
        feedback: &Feedback,
    ) -> Result<(), String> {
        let Some(settings) = document.scene_observation.as_ref().filter(|s| s.enabled) else {
            return Ok(());
        };
        let generation = match admission.task_generation().await {
            Ok(value) => value,
            Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(error) => return Err(format!("scene observation generation: {error:?}")),
        };
        if self.scope.is_none_or(|scope| {
            scope.epochs != generation.epochs || scope.boot_id != feedback.snapshot().cursor.boot_id
        }) {
            self.scope = None;
            self.cursor = None;
            self.begin(admission, controller, generation, feedback)
                .await?;
            return Ok(()); // Never replay frames seen before synchronization.
        }
        let events =
            match feedback.events_since(self.cursor.ok_or("scene observation cursor missing")?) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!("scene observation stream gap; old inference invalidated: {error}");
                    self.scope = None;
                    self.cursor = None;
                    self.begin(admission, controller, generation, feedback)
                        .await?;
                    return Ok(());
                }
            };
        for event in events {
            if event.change != Change::Button
                || !settings
                    .bindings
                    .iter()
                    .any(|b| b.device == event.device_id.as_str())
            {
                self.cursor = Some(event.cursor);
                continue;
            }
            let proof = feedback.scene_key_observation(&event, admission.monotonic_ms())?;
            // Mark consumed before await; cancellation or uncertain persistence
            // is never permission to replay this key against a later selection.
            let previous = self.cursor;
            self.cursor = Some(event.cursor);
            match admission
                .observe_scene_key(controller, generation.clone(), self.scope.unwrap(), proof)
                .await
            {
                Ok(()) => {}
                // The actor did not consume/commit a ResourceBusy request. A new
                // proof may retry it only while the original event is still fresh.
                Err(WriteStatus::ResourceBusy) => {
                    self.cursor = previous;
                    return Ok(());
                }
                Err(WriteStatus::InvalidValueInRequest) => {
                    log::warn!("scene key inference rejected; event discarded");
                }
                Err(error) => return Err(format!("scene observation commit: {error:?}")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "scene_observation_tests.rs"]
mod tests;
