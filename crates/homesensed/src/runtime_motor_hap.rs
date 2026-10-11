//! Calibrated motor action adapter. Construction neither listens nor sends frames.
use crate::admission::Admission;
use hap::{
    characteristic::CharacteristicCallbacks,
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_core::DeviceId;
use homesense_hap::{
    identifiers::MotorOperation,
    motor_graph::{MotorBinding, MotorGraph, MotorSpec},
    motor_requests::MotorRequests,
    requests::RequestError,
    storage::IdentityStore,
};
use std::sync::Arc;
use uuid::Uuid;
type Authorize = Arc<dyn Fn(Uuid, &DeviceId) -> bool + Send + Sync>;
type Reset = (DeviceId, MotorOperation);
struct Handler {
    requests: MotorRequests,
    admission: Admission,
    revision: u64,
    authorize: Authorize,
    resets: tokio::sync::mpsc::Sender<Reset>,
    writes: tokio::sync::Mutex<()>,
}
impl WriteBatchHandler for Handler {
    fn write(&self, _: Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        Err(WriteStatus::ServiceCommunicationFailure)
    }
    fn write_async<'a>(
        &'a self,
        controller: Uuid,
        body: &'a [u8],
    ) -> hap::futures::future::BoxFuture<'a, Result<(), WriteStatus>> {
        Box::pin(async move {
            let batch = self
                .requests
                .normalize(controller, body)
                .map_err(|e| match e {
                    RequestError::UnknownCharacteristic => WriteStatus::ResourceDoesNotExist,
                    _ => WriteStatus::InvalidValueInRequest,
                })?;
            let _guard = self.writes.lock().await;
            for target in &batch.addressed {
                if !(self.authorize)(controller, &target.device) {
                    return Err(WriteStatus::InsufficientPrivileges);
                }
            }
            // Also validate false writes and subscriptions against current owner
            // authorization/configuration, without submitting a physical action.
            let generation = self.admission.task_generation().await?;
            let bundle = self
                .admission
                .automation_bundle(controller, generation)
                .await?;
            if bundle.configuration.revision() != self.revision {
                return Err(WriteStatus::ServiceCommunicationFailure);
            }
            for target in &batch.addressed {
                bundle
                    .configuration
                    .motor_command(&target.device, target.operation.action())
                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
            }
            if let Some(target) = batch.action {
                let permit = self
                    .resets
                    .try_reserve()
                    .map_err(|_| WriteStatus::ResourceBusy)?;
                self.admission
                    .submit_motor_for_revision(
                        self.revision,
                        controller,
                        target.device.clone(),
                        target.operation.action(),
                    )
                    .await?;
                permit.send((target.device, target.operation));
            }
            Ok(())
        })
    }
}
pub struct RuntimeMotorHap {
    pub accessories: Vec<(MotorBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    resets: tokio::sync::mpsc::Receiver<Reset>,
}
impl RuntimeMotorHap {
    pub async fn new(
        config: &homesense_config::Configuration,
        store: &IdentityStore,
        admission: Admission,
        specs: Vec<MotorSpec>,
        authorize: impl Fn(Uuid, &DeviceId) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        for spec in &specs {
            config
                .motor_command(&spec.device, homesense_core::MotorAction::Stop)
                .map_err(|e| e.to_string())?;
        }
        let graph = MotorGraph::build(store, specs)
            .await
            .map_err(|e| e.to_string())?;
        let requests =
            MotorRequests::new(graph.accessories.iter().map(|(b, _)| b.clone()).collect())
                .map_err(|e| e.to_string())?;
        let (send, resets) = tokio::sync::mpsc::channel(32);
        let handler = Handler {
            requests,
            admission,
            revision: config.revision(),
            authorize: Arc::new(authorize),
            resets: send,
            writes: tokio::sync::Mutex::new(()),
        };
        let accessories = graph
            .accessories
            .into_iter()
            .map(|(binding, mut accessory)| {
                accessory
                    .switch
                    .power_state
                    .on_read(Some(|| Ok(Some(false))));
                accessory.switch.power_state.on_update(Some(
                    |_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                        Err(Box::new(std::io::Error::other(
                            "grouped motor write required",
                        )))
                    },
                ));
                let handle: AccessoryHandle =
                    Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
                (binding, handle)
            })
            .collect();
        Ok(Self {
            accessories,
            write_handler: Arc::new(handler),
            resets,
        })
    }
    /// Virtual acceptance acknowledgment, not motor position/completion. Call
    /// outside HTTP locks so projection cannot deadlock command admission.
    pub async fn flush_resets(&mut self) -> Result<usize, String> {
        let mut count = 0;
        while count < 32 {
            let Ok((device, operation)) = self.resets.try_recv() else {
                break;
            };
            let (binding, handle) = self
                .accessories
                .iter()
                .find(|(b, _)| b.device == device && b.operation == operation)
                .ok_or("motor reset binding missing")?;
            let mut accessory = handle.lock().await;
            let characteristic = accessory
                .get_mut_services()
                .into_iter()
                .flat_map(|s| s.get_mut_characteristics())
                .find(|c| c.get_id() == binding.on_iid)
                .ok_or("motor reset characteristic missing")?;
            characteristic
                .set_observed_value(serde_json::json!(false))
                .await
                .map_err(|e| e.to_string())?;
            count += 1;
        }
        Ok(count)
    }
}
