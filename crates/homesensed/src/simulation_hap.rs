//! Offline composition of HAP characteristics and the configured simulator.
//! This does not advertise or listen; its write handler has memory-only acceptance.
use crate::simulation::SimulationRuntime;
use hap::{
    accessory::bridge::BridgeAccessory,
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_config::Configuration;
use homesense_core::{ExecutionContext, ModuleKind};
use homesense_hap::{
    events::ObservedEvents,
    graph::{LightGraph, LightSpec},
    observed::ObservedView,
    requests::{LightBinding, LightRequests},
    storage::IdentityStore,
    write::LightWriteHandler,
};
use homesense_state::Snapshot;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, Ordering},
};
use uuid::Uuid;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub struct SimulationHap {
    pub bridge: BridgeAccessory,
    pub lights: Vec<(LightBinding, AccessoryHandle)>,
    pub write_handler: Box<dyn WriteBatchHandler>,
    runtime: Arc<Mutex<SimulationRuntime>>,
    snapshot: Arc<RwLock<Snapshot>>,
    clock: Arc<AtomicU64>,
    projection: ObservedEvents,
    projection_interrupted: bool,
}
impl SimulationHap {
    pub async fn new(
        document: &[u8],
        store: &IdentityStore,
        controller: Uuid,
        max_age_ms: u64,
    ) -> Result<Self, String> {
        let configuration =
            Configuration::parse(document, ExecutionContext::Simulation).map_err(error)?;
        let mut specs = Vec::new();
        for id in configuration.devices().keys() {
            if let Some((device, module)) = configuration.binding(id) {
                let dimmable = match module.kind() {
                    ModuleKind::Dimmer => true,
                    ModuleKind::Switch => false,
                    _ => return Err("simulation HAP graph currently supports lights only".into()),
                };
                specs.push(LightSpec {
                    device: id.clone(),
                    name: device.name.clone(),
                    dimmable,
                    default_on_level: None,
                });
            }
        }
        let runtime = SimulationRuntime::new(document, [controller], 128)?;
        let snapshot = Arc::new(RwLock::new(runtime.snapshot()));
        let clock = Arc::new(AtomicU64::new(0));
        let now = clock.clone();
        let view = ObservedView::new(snapshot.clone(), max_age_ms, move || {
            now.load(Ordering::Acquire)
        })
        .map_err(error)?;
        let mut graph = LightGraph::build(store, "HomeSense Simulation", specs)
            .await
            .map_err(error)?;
        graph.install_observed_reads(view.clone());
        let requests = LightRequests::new(graph.bindings.clone()).map_err(error)?;
        let runtime = Arc::new(Mutex::new(runtime));
        let owner = runtime.clone();
        let now = clock.clone();
        let write_handler = Box::new(LightWriteHandler::new(
            requests,
            move |id: &homesense_core::DeviceId| view.brightness(id).ok().filter(|v| v.get() > 0),
            move |batch| {
                owner
                    .try_lock()
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
                    .submit(batch, now.load(Ordering::Acquire))
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)
            },
        ));
        let mut lights = Vec::new();
        for binding in graph.bindings {
            let light = graph
                .lights
                .remove(&binding.device)
                .ok_or("graph binding missing")?;
            let handle: AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(light)));
            lights.push((binding, handle));
        }
        let mut projection = ObservedEvents::new(lights.clone(), max_age_ms)
            .await
            .map_err(error)?;
        let initial = snapshot
            .read()
            .map_err(|_| "snapshot lock poisoned")?
            .clone();
        projection.resynchronize(&initial, 0).await.map_err(error)?;
        Ok(Self {
            bridge: graph.bridge,
            lights,
            write_handler,
            runtime,
            snapshot,
            clock,
            projection,
            projection_interrupted: false,
        })
    }
    /// Publish the read snapshot before emitting corresponding characteristic
    /// events. Retry after an interrupted publication resynchronizes the graph.
    pub async fn advance(&mut self, now: u64) -> Result<(), String> {
        let (snapshot, events) = {
            let mut runtime = self.runtime.try_lock().map_err(|_| "runtime busy")?;
            let cursor = self
                .snapshot
                .read()
                .map_err(|_| "snapshot lock poisoned")?
                .cursor;
            runtime.advance(now)?;
            (runtime.snapshot(), runtime.events_since(cursor))
        };
        self.clock.store(now, Ordering::Release);
        *self
            .snapshot
            .write()
            .map_err(|_| "snapshot lock poisoned")? = snapshot.clone();
        let resync = self.projection_interrupted;
        self.projection_interrupted = true;
        match events {
            Ok(events) if !resync => {
                for event in events {
                    self.projection.apply(&event, now).await.map_err(error)?;
                }
            }
            _ => self
                .projection
                .resynchronize(&snapshot, now)
                .await
                .map_err(error)?,
        }
        self.projection_interrupted = false;
        Ok(())
    }
}
