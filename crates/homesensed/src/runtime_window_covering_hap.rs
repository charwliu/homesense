//! Native window coverings backed by the admission owner's written motor
//! trajectory. No construction path sends a physical motor command.
use crate::{
    admission::Admission,
    window_covering_state::{CoveringState, TRAVEL_MS, View},
};
use hap::{
    accessory::HapAccessory,
    characteristic::CharacteristicCallbacks,
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_core::{DeviceId, MotorAction, curtain_state::Timing};
use homesense_hap::{
    motor_graph::MotorSpec,
    requests::RequestError,
    storage::IdentityStore,
    window_covering_graph::{WindowCoveringBinding, WindowCoveringGraph},
    window_covering_requests::{WindowCommand, WindowCoveringRequests},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};
use uuid::Uuid;

type Authorize = Arc<dyn Fn(Uuid, &DeviceId) -> bool + Send + Sync>;

struct Handler {
    requests: WindowCoveringRequests,
    admission: Admission,
    revision: u64,
    authorize: Authorize,
    states: Arc<RwLock<BTreeMap<DeviceId, CoveringState>>>,
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
            for device in &batch.addressed {
                if !(self.authorize)(controller, device) {
                    return Err(WriteStatus::InsufficientPrivileges);
                }
            }
            let generation = self.admission.task_generation().await?;
            let bundle = self
                .admission
                .automation_bundle(controller, generation)
                .await?;
            if bundle.configuration.revision() != self.revision {
                return Err(WriteStatus::ServiceCommunicationFailure);
            }
            for device in &batch.addressed {
                bundle
                    .configuration
                    .motor_command(device, MotorAction::Stop)
                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
            }
            let Some(command) = batch.command else {
                return Ok(());
            };
            let (device, action, target) = match command {
                WindowCommand::Hold { device } => (device, Some(MotorAction::Stop), None),
                WindowCommand::Target { device, percent } => {
                    let current = self
                        .states
                        .read()
                        .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                        .get(&device)
                        .ok_or(WriteStatus::ResourceDoesNotExist)?
                        .view(self.admission.monotonic_ms());
                    let action = if percent == 100 || percent > current.current && percent != 0 {
                        Some(MotorAction::Open)
                    } else if percent == 0 || percent < current.current {
                        Some(MotorAction::Close)
                    } else if current.state != 2 {
                        Some(MotorAction::Stop)
                    } else {
                        None
                    };
                    (device, action, Some(percent))
                }
            };
            let Some(action) = action else { return Ok(()) };
            let ids = self
                .admission
                .submit_motor_for_revision(self.revision, controller, device.clone(), action)
                .await?;
            if let Some(target) = target {
                let id = *ids
                    .first()
                    .ok_or(WriteStatus::ServiceCommunicationFailure)?;
                self.states
                    .write()
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                    .get_mut(&device)
                    .ok_or(WriteStatus::ResourceDoesNotExist)?
                    .accepted_target(controller, id, target);
            }
            Ok(())
        })
    }
}

pub struct RuntimeWindowCoveringHap {
    pub accessories: Vec<(WindowCoveringBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    admission: Admission,
    controller: Uuid,
    revision: u64,
    states: Arc<RwLock<BTreeMap<DeviceId, CoveringState>>>,
    published: BTreeMap<DeviceId, View>,
}

async fn observed(
    accessory: &mut dyn HapAccessory,
    iid: u64,
    value: serde_json::Value,
) -> Result<(), String> {
    let characteristic = accessory
        .get_mut_services()
        .into_iter()
        .flat_map(|service| service.get_mut_characteristics())
        .find(|characteristic| characteristic.get_id() == iid)
        .ok_or("covering characteristic missing")?;
    characteristic
        .set_observed_value(value)
        .await
        .map_err(|e| e.to_string())
}

impl RuntimeWindowCoveringHap {
    pub async fn new(
        config: &homesense_config::Configuration,
        store: &IdentityStore,
        admission: Admission,
        controller: Uuid,
        specs: Vec<MotorSpec>,
        authorize: impl Fn(Uuid, &DeviceId) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("covering generation: {e:?}"))?;
        for spec in &specs {
            config
                .motor_command(&spec.device, MotorAction::Stop)
                .map_err(|e| e.to_string())?;
            // The admission owner, not an independent HAP timer, observes every
            // successful motor write from HAP, Web and physical keypad routes.
            admission
                .curtain_candidate(
                    controller,
                    generation.clone(),
                    spec.device.clone(),
                    Timing {
                        open_travel_ms: Some(TRAVEL_MS),
                        close_travel_ms: Some(TRAVEL_MS),
                        stop_led_idle_ms: None,
                    },
                )
                .await
                .map_err(|e| format!("covering inference: {e:?}"))?;
        }
        let graph = WindowCoveringGraph::build(store, specs)
            .await
            .map_err(|e| e.to_string())?;
        let requests = WindowCoveringRequests::new(
            graph
                .accessories
                .iter()
                .map(|(binding, _)| binding.clone())
                .collect(),
        )
        .map_err(|e| e.to_string())?;
        let states: Arc<RwLock<BTreeMap<_, _>>> = Arc::new(RwLock::new(
            graph
                .accessories
                .iter()
                .map(|(binding, _)| (binding.device.clone(), CoveringState::default()))
                .collect(),
        ));
        let handler = Handler {
            requests,
            admission: admission.clone(),
            revision: config.revision(),
            authorize: Arc::new(authorize),
            states: states.clone(),
            writes: tokio::sync::Mutex::new(()),
        };
        let mut accessories = Vec::with_capacity(graph.accessories.len());
        for (binding, mut accessory) in graph.accessories {
            observed(&mut accessory, binding.current_iid, serde_json::json!(50)).await?;
            observed(&mut accessory, binding.target_iid, serde_json::json!(50)).await?;
            observed(&mut accessory, binding.state_iid, serde_json::json!(2)).await?;
            accessory.window_covering.target_position.on_update(Some(
                |_: &u8, _: &u8| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    Err(Box::new(std::io::Error::other(
                        "grouped covering write required",
                    )))
                },
            ));
            if let Some(hold) = &mut accessory.window_covering.hold_position {
                hold.on_update(Some(
                    |_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                        Err(Box::new(std::io::Error::other(
                            "grouped covering write required",
                        )))
                    },
                ));
            }
            let handle: AccessoryHandle =
                Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
            accessories.push((binding, handle));
        }
        let mut runtime = Self {
            accessories,
            write_handler: Arc::new(handler),
            admission,
            controller,
            revision: config.revision(),
            states,
            published: BTreeMap::new(),
        };
        runtime.refresh().await?;
        Ok(runtime)
    }

    pub async fn refresh(&mut self) -> Result<(), String> {
        let ids = self
            .accessories
            .iter()
            .map(|(binding, _)| binding.device.clone())
            .collect();
        let snapshots = match self.admission.task_generation().await {
            Ok(generation) => match self
                .admission
                .curtain_snapshots(self.controller, generation, ids)
                .await
            {
                Ok(snapshots) => snapshots,
                Err(error) => {
                    log::warn!("covering inference unavailable: {error:?}");
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        let now = self.admission.monotonic_ms();
        let mut due = Vec::new();
        let mut views = BTreeMap::new();
        {
            let mut states = self
                .states
                .write()
                .map_err(|_| "covering state lock poisoned")?;
            for (binding, _) in &self.accessories {
                let state = states
                    .get_mut(&binding.device)
                    .ok_or("covering state missing")?;
                let snapshot = snapshots.get(&binding.device);
                views.insert(binding.device.clone(), state.apply(snapshot, now));
                if let Some((controller, command_id)) = state.due_stop(snapshot, now) {
                    due.push((binding.device.clone(), controller, command_id));
                }
            }
        }
        for (device, controller, command_id) in due {
            if let Err(error) = self
                .admission
                .submit_motor_for_revision(
                    self.revision,
                    controller,
                    device.clone(),
                    MotorAction::Stop,
                )
                .await
            {
                log::error!("covering automatic STOP rejected for {device:?}: {error:?}");
                self.states
                    .write()
                    .map_err(|_| "covering state lock poisoned")?
                    .get_mut(&device)
                    .ok_or("covering state missing")?
                    .stop_failed(command_id);
            }
        }
        for (binding, handle) in &self.accessories {
            let view = views[&binding.device];
            if self.published.get(&binding.device) == Some(&view) {
                continue;
            }
            let mut accessory = handle.lock().await;
            let accessory = accessory.as_mut();
            observed(
                accessory,
                binding.current_iid,
                serde_json::json!(view.current),
            )
            .await?;
            observed(
                accessory,
                binding.target_iid,
                serde_json::json!(view.target),
            )
            .await?;
            observed(accessory, binding.state_iid, serde_json::json!(view.state)).await?;
            self.published.insert(binding.device.clone(), view);
        }
        Ok(())
    }
}
