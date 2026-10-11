//! Typed motor intents; physical mapping remains the calibrated runtime's job.
use crate::{
    identifiers::MotorOperation,
    motor_graph::MotorBinding,
    requests::{RequestError, Subscription},
    scene_graph::SceneBinding,
    scene_requests::SceneRequests,
};
use homesense_core::DeviceId;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotorTarget {
    pub device: DeviceId,
    pub operation: MotorOperation,
}
pub struct MotorBatch {
    pub controller: Uuid,
    /// Includes false writes and subscription-only targets for authorization.
    pub addressed: Vec<MotorTarget>,
    /// At most one accepted intent. False means reset, never inverse motion.
    pub action: Option<MotorTarget>,
    pub subscriptions: Vec<Subscription>,
}
pub struct MotorRequests {
    parser: SceneRequests,
    targets: BTreeMap<String, MotorTarget>,
}
impl MotorRequests {
    pub fn new(bindings: Vec<MotorBinding>) -> Result<Self, RequestError> {
        let mut targets = BTreeMap::new();
        let mut scenes = vec![];
        let mut unique = BTreeSet::new();
        for binding in bindings {
            if !unique.insert((binding.device.clone(), binding.operation)) {
                return Err(RequestError::InvalidBinding);
            }
            let id = binding.aid.to_string();
            if targets
                .insert(
                    id.clone(),
                    MotorTarget {
                        device: binding.device,
                        operation: binding.operation,
                    },
                )
                .is_some()
            {
                return Err(RequestError::InvalidBinding);
            }
            scenes.push(SceneBinding {
                scene: id,
                aid: binding.aid,
                on_iid: binding.on_iid,
            });
        }
        Ok(Self {
            parser: SceneRequests::new(scenes)?,
            targets,
        })
    }
    pub fn normalize(&self, controller: Uuid, body: &[u8]) -> Result<MotorBatch, RequestError> {
        let batch = self.parser.normalize(controller, body)?;
        // submit_motor is a single logical operation, not a multi-device atomic
        // transaction. Reject multiple actions before either can be submitted.
        if batch.starts.len() > 1 {
            return Err(RequestError::InvalidValue);
        }
        Ok(MotorBatch {
            controller: batch.controller,
            addressed: batch
                .addressed
                .iter()
                .map(|id| self.targets[id].clone())
                .collect(),
            action: batch.starts.first().map(|id| self.targets[id].clone()),
            subscriptions: batch.subscriptions,
        })
    }
}
