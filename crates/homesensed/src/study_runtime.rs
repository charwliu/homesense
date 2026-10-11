//! Explicit study scene runs. No panel subscription and no direct transport writes.
use crate::admission::{Admission, TaskGeneration};
use hap::serde_json::Value;
use homesense_core::{Command, DeviceId, LevelTransition, Percent};
use homesense_hap::requests::{Batch, Intent};
use homesense_import::study::{Action, Evaluation, Rule};
use uuid::Uuid;

/// The selected policy deliberately uses the module's existing ramp, not an
/// invented conversion from Control4 rate=1000 to a protocol duration.
pub enum TimingPolicy {
    ModuleNative,
}
pub struct StudyRuntime {
    admission: Admission,
    controller: Uuid,
    document: Value,
    rules: Vec<Rule>,
    active: Option<SceneRun>,
}
#[derive(Clone)]
pub struct SceneRun {
    pub command_ids: Vec<u64>,
    pub mode: u8,
    generation: TaskGeneration,
}
impl StudyRuntime {
    pub fn new(
        admission: Admission,
        controller: Uuid,
        document: &[u8],
        _timing: TimingPolicy,
    ) -> Result<Self, String> {
        if document.len() > 1024 * 1024 {
            return Err("migration too large".into());
        }
        let document: Value = hap::serde_json::from_slice(document).map_err(|e| e.to_string())?;
        if document["automatic_execution"] != false
            || document["variable_id"] != "411"
            || document["keypad_address"] != "41"
        {
            return Err("explicit study configuration required".into());
        }
        let rules =
            hap::serde_json::from_value(document["rules"].clone()).map_err(|e| e.to_string())?;
        Ok(Self {
            admission,
            controller,
            document,
            rules,
            active: None,
        })
    }
    /// Toggle requires known local mode. It never reads or mutates EA-5 variables.
    pub async fn toggle(&mut self, key: u8) -> Result<SceneRun, String> {
        self.start(key, false).await
    }
    /// Explicit activation establishes a local selection even when mode is unknown.
    pub async fn activate(&mut self, key: u8) -> Result<SceneRun, String> {
        self.start(key, true).await
    }
    async fn start(&mut self, key: u8, activate: bool) -> Result<SceneRun, String> {
        let generation = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("generation: {e:?}"))?;
        let expected = self
            .admission
            .study_mode()
            .await
            .map_err(|e| format!("mode: {e:?}"))?;
        let evaluation = homesense_import::study::evaluate(
            &self.rules,
            key,
            true,
            if activate { Some(0) } else { expected },
        )?;
        let Evaluation::Planned { next_mode, actions } = evaluation else {
            return Err("unknown mode or unsupported key; no actions admitted".into());
        };
        let mut intents = Vec::new();
        for action in actions {
            match action {
                Action::SetMode { .. } => {}
                Action::Led { key, on, .. } => intents.push(Intent {
                    device: DeviceId::new(format!("control4:63:key:{key}"))
                        .map_err(|e| e.to_string())?,
                    command: Command::PanelLed(on),
                }),
                Action::Scene {
                    scene_id, activate, ..
                } => {
                    for target in
                        homesense_import::study::targets(&self.document, &scene_id, activate)?
                    {
                        if target.source_delay != 0 || ![0, 1, 1000].contains(&target.source_rate) {
                            return Err("unreviewed timing; nothing admitted".into());
                        }
                        let command =
                            if ["control4:210", "control4:278"].contains(&target.device.as_str()) {
                                if ![0, 100].contains(&target.level) {
                                    return Err("relay target must be 0 or 100".into());
                                }
                                Command::Switch(target.level != 0)
                            } else {
                                Command::SetLevel {
                                    level: Percent::new(target.level).map_err(|e| e.to_string())?,
                                    transition: LevelTransition::Immediate,
                                }
                            };
                        intents.push(Intent {
                            device: DeviceId::new(target.device).map_err(|e| e.to_string())?,
                            command,
                        });
                    }
                }
            }
        }
        // Cancelling old pending work cannot compensate already written output.
        self.cancel().await?;
        let ids = self
            .admission
            .submit_study(
                generation.clone(),
                Batch {
                    controller: self.controller,
                    intents,
                    subscriptions: vec![],
                },
                expected,
                next_mode,
            )
            .await
            .map_err(|e| format!("scene admission: {e:?}"))?;
        let run = SceneRun {
            command_ids: ids,
            mode: next_mode,
            generation,
        };
        self.active = Some(run.clone());
        Ok(run)
    }
    pub async fn cancel(&mut self) -> Result<usize, String> {
        let Some(run) = &self.active else {
            return Ok(0);
        };
        let count = self
            .admission
            .cancel_scene_run(self.controller, run.generation.clone(), run.command_ids[0])
            .await
            .map_err(|e| format!("scene cancellation: {e:?}"))?;
        self.active = None;
        Ok(count)
    }
    pub async fn mode(&self) -> Result<Option<u8>, String> {
        self.admission
            .study_mode()
            .await
            .map_err(|e| format!("mode: {e:?}"))
    }
}
