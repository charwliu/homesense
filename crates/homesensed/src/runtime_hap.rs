//! Native HAP graph with durable Admission and transport-observed snapshots.
//! Construction does not start a network listener or advertise mDNS.
use crate::{admission::Admission, observed_transport::ObservedTransport};
use hap::server::{AccessoryHandle, WriteBatchHandler};
use homesense_config::Configuration;
use homesense_core::ModuleKind;
use homesense_hap::{
    events::ObservedEvents,
    graph::{LightGraph, LightSpec},
    observed::ObservedView,
    requests::{LightBinding, LightRequests},
    storage::IdentityStore,
    write::AsyncLightWriteHandler,
};
use homesense_state::Snapshot;
use std::sync::{Arc, RwLock};

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn logical_command(
    intents:&[(homesense_core::DeviceId,homesense_core::Command)],
    logical_ids:&std::collections::BTreeSet<homesense_core::DeviceId>,
) -> Result<Option<(homesense_core::DeviceId,crate::management_devices::DeviceAction)>,hap::server::WriteStatus> {
    use hap::server::WriteStatus;
    if !intents.iter().any(|(id,_)|logical_ids.contains(id)) { return Ok(None); }
    // The shared-resource owner needs a fresh physical lease. A HAP batch
    // spanning owners must fail before either side can accept a command.
    if intents.len()!=1 { return Err(WriteStatus::InvalidValueInRequest); }
    let (device,command)=&intents[0];
    let action=match command {
        homesense_core::Command::SetLevel{level,..}=>crate::management_devices::DeviceAction::Brightness{level:level.get()},
        homesense_core::Command::Switch(on)=>crate::management_devices::DeviceAction::Power{on:*on},
        _=>return Err(WriteStatus::InvalidValueInRequest),
    };
    Ok(Some((device.clone(),action)))
}

/// Light publication is a view of the shared physical snapshot. Non-light
/// observations remain with the automation owner and are not HAP light inputs.
fn light_snapshot(configuration:&Configuration,physical:&Snapshot,selected:Option<&std::collections::BTreeSet<homesense_core::DeviceId>>,recorded:&std::collections::BTreeMap<homesense_core::DeviceId,homesense_core::Percent>)->Result<Snapshot,String> {
    let mut logical=crate::light_state::project_pending(configuration,physical,recorded).map_err(error)?;
    logical.devices.retain(|id,_|configuration.binding(id).is_some_and(|(_,module)|matches!(module.kind(),ModuleKind::Switch|ModuleKind::Dimmer)));
    if let Some(selected)=selected {logical.devices.retain(|id,_|selected.contains(id));}
    Ok(logical)
}

pub struct RuntimeHap {
    pub bridge: AccessoryHandle,
    pub lights: Vec<(LightBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    admission: Admission,
    snapshot: Arc<RwLock<Snapshot>>,
    projection: ObservedEvents,
    projection_interrupted: bool,
    max_age_ms: u64,
    configuration: Configuration,
    projection_epoch: Option<u64>,
    selected: Option<std::collections::BTreeSet<homesense_core::DeviceId>>,
    physical_snapshot: Arc<RwLock<Snapshot>>,
    restore_levels:
        Arc<RwLock<std::collections::BTreeMap<homesense_core::DeviceId, homesense_core::Percent>>>,
}
// Empty light subsets route no write addresses. Keep the standalone handler
// fail-closed too, should it be invoked without UnifiedHap's router.
struct NoLightWrites;
impl WriteBatchHandler for NoLightWrites {
    fn write(&self,_controller:uuid::Uuid,_body:&[u8])->Result<(),hap::server::WriteStatus> {
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    }
}
struct InstallationGuard {
    admission: Admission,
    installed: bool,
}
impl Drop for InstallationGuard {
    fn drop(&mut self) {
        if !self.installed {
            self.admission.stop_now();
        }
    }
}
struct SceneDriveGuard<'a>(Option<&'a mut crate::scene_scheduler::SceneScheduler>);
impl Drop for SceneDriveGuard<'_> {
    fn drop(&mut self) {
        if let Some(scheduler) = &mut self.0 {
            scheduler.stop_now();
        }
    }
}
impl RuntimeHap {
    pub async fn new(
        configuration: &Configuration,
        initial: Snapshot,
        store: &IdentityStore,
        admission: Admission,
        max_age_ms: u64,
    ) -> Result<Self, String> {
        Self::new_selected(configuration,initial,store,admission,max_age_ms,None).await
    }
    /// Publish an explicit logical light subset (including an empty subset) while retaining the full
    /// physical snapshot for combined-light writes and shared admission.
    pub async fn new_selected(
        configuration:&Configuration,initial:Snapshot,store:&IdentityStore,
        admission:Admission,max_age_ms:u64,
        selected:Option<std::collections::BTreeSet<homesense_core::DeviceId>>,
    )->Result<Self,String> {
        Self::new_selected_with_control(configuration, initial, store, admission, max_age_ms, selected, None).await
    }
    pub async fn new_selected_with_control(
        configuration:&Configuration,initial:Snapshot,store:&IdentityStore,
        admission:Admission,max_age_ms:u64,
        selected:Option<std::collections::BTreeSet<homesense_core::DeviceId>>,
        control:Option<crate::automation_control::Control>,
    )->Result<Self,String> {
        let unspecified = selected.is_none();
        let selected = selected.unwrap_or_else(||crate::hap_bridge_plan::all_light_ids(configuration));
        let supplies:std::collections::BTreeSet<_>=configuration.devices().values().filter(|d|d.enabled).filter_map(|d|d.supply_relay.as_ref()).collect();
        if selected.iter().any(|id|supplies.contains(id)
            || !configuration.binding(id).is_some_and(|(_,module)|matches!(module.kind(),ModuleKind::Switch|ModuleKind::Dimmer))
            || configuration.composed_resource(id).is_some() && configuration.composed_light(id).is_none()) {
            return Err("HAP selection requires enabled logical lights".into());
        }
        let logical_ids:std::collections::BTreeSet<_>=selected.iter()
            .filter(|id|configuration.shared_light(id).is_some() || configuration.composed_light(id).is_some())
            .cloned().collect();
        if !logical_ids.is_empty() && control.is_none() {
            return Err("HAP shared-resource lights require the automation control owner".into());
        }
        // Live standalone graphs can be constructed before their initial sync.
        // Once a ready epoch is captured it is never relabelled after recovery.
        let projection_epoch=admission.generation_updates().borrow().as_ref().map(|generation|generation.epochs.transport);
        let restore_levels = Arc::new(RwLock::new(
            admission
                .remember_levels(configuration.revision(), initial.clone(), max_age_ms)
                .await
                .map_err(|e| format!("restore levels: {e:?}"))?,
        ));
        let physical_snapshot = Arc::new(RwLock::new(initial.clone()));
        let recorded = admission.projection_levels(configuration.revision(), initial.cursor.boot_id).await.map_err(|e|format!("recorded levels: {e:?}"))?;
        let initial = light_snapshot(configuration, &initial, Some(&selected), &recorded)?;
        let supplies: std::collections::BTreeSet<_> = configuration
            .devices()
            .values()
            .filter(|d| d.enabled)
            .filter_map(|d| d.supply_relay.as_ref())
            .collect();
        let mut specs = Vec::new();
        for id in configuration.devices().keys() {
            if supplies.contains(id) || !selected.contains(id) {
                continue;
            }
            if let Some((device, module)) = configuration.binding(id) {
                if let Some((_, composed)) = configuration.composed_light(id) {
                    specs.push(LightSpec {
                        device:id.clone(), name:composed.name.clone(), dimmable:true,
                        default_on_level:Some(homesense_core::Percent::new(composed.default_on_level).map_err(error)?),
                    });
                    continue;
                }
                let dimmable = match module.kind() {
                    ModuleKind::Dimmer => true,
                    ModuleKind::Switch => false,
                    _ => continue,
                };
                specs.push(LightSpec {
                    device: id.clone(),
                    name: device.name.clone(),
                    dimmable,
                    default_on_level: device.default_on_level,
                });
            }
        }
        for spec in &specs {
            if !initial.devices.contains_key(&spec.device) {
                return Err("missing initial observed device".into());
            }
        }
        if initial.devices.len() != specs.len() {
            return Err("snapshot/graph device mismatch".into());
        }
        let snapshot = Arc::new(RwLock::new(initial));
        let clock = admission.clone();
        let view = ObservedView::new(snapshot.clone(), max_age_ms, move || clock.monotonic_ms())
            .map_err(error)?
            .with_restore_levels(restore_levels.clone());
        let bridge_name=hap::storage::Storage::load_config(store).await.map_err(error)?.name;
        let mut graph = LightGraph::build(store, &bridge_name, specs)
            .await
            .map_err(error)?;
        graph.install_observed_reads(view.clone());
        if graph.bindings.is_empty() && unspecified {
            return Err("HAP light publication requires enabled logical lights".into());
        }
        let requests = if graph.bindings.is_empty() {None} else {Some(LightRequests::new(graph.bindings.clone()).map_err(error)?)};
        let revision = configuration.revision();
        let owner = admission.clone();
        let restore = restore_levels.clone();
        let identity = store.clone();
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
        projection.set_restore_levels(restore_levels.clone());
        let transitions = projection.transitions();
        let writes = Arc::new(tokio::sync::Mutex::new(()));
        let write_handler: Arc<dyn WriteBatchHandler> = if let Some(requests)=requests {Arc::new(AsyncLightWriteHandler::new(
            requests,
            move |id: &homesense_core::DeviceId| restore.read().ok()?.get(id).copied(),
            move |batch: homesense_hap::requests::Batch| {
                let owner = owner.clone();
                let transitions = transitions.clone();
                let writes = writes.clone();
                let logical_ids = logical_ids.clone();
                let control = control.clone();
                let identity = identity.clone();
                async move {
                    let _serial = writes.lock().await;
                    let intents: Vec<_> = batch
                        .intents
                        .iter()
                        .map(|i| (i.device.clone(), i.command))
                        .collect();
                    if let Some((device,action))=logical_command(&intents,&logical_ids)? {
                        if !identity.controller_is_paired(batch.controller).unwrap_or(false) {
                            return Err(hap::server::WriteStatus::InsufficientPrivileges);
                        }
                        let control=control.ok_or(hap::server::WriteStatus::ServiceCommunicationFailure)?;
                        let diagnostic_device=device.clone();
                        match control.request(crate::automation_control::Action::DeviceCommand{
                            controller:batch.controller,revision,device,action,
                        }).await {
                            Ok(crate::automation_control::Reply::CommandsAccepted(_))=>{},
                            Err(reason)=>{
                                log::warn!("HAP logical write rejected device={} reason={reason}",diagnostic_device.as_str());
                                return Err(hap::server::WriteStatus::ServiceCommunicationFailure);
                            },
                            _=>return Err(hap::server::WriteStatus::ServiceCommunicationFailure),
                        }
                    } else {
                        owner.submit_for_revision(revision, batch).await?;
                    }
                    for (device, command) in intents {
                        transitions.accepted(device, command, owner.monotonic_ms());
                    }
                    Ok(())
                }
            },
        ))} else {Arc::new(NoLightWrites)};
        let initial = snapshot
            .read()
            .map_err(|_| "snapshot lock poisoned")?
            .clone();
        projection
            .resynchronize(&initial, admission.monotonic_ms())
            .await
            .map_err(error)?;
        Ok(Self {
            bridge: Arc::new(hap::futures::lock::Mutex::new(Box::new(graph.bridge))),
            lights,
            write_handler,
            admission,
            snapshot,
            projection,
            projection_interrupted: false,
            max_age_ms,
            configuration: configuration.clone(),
            projection_epoch,
            selected:Some(selected),
            physical_snapshot,
            restore_levels,
        })
    }
    /// Age the HAP event projection independently of the physical owner and
    /// durable admission. Reads already check this same observation deadline.
    pub(crate) async fn refresh_expiration(&mut self) -> Result<(), String> {
        let snapshot = self.snapshot.read().map_err(|_| "snapshot lock poisoned")?.clone();
        self.projection.refresh_snapshot(&snapshot, self.admission.monotonic_ms()).await.map_err(error)
    }
    /// Coalesced full snapshot path for an independent HAP projection task.
    /// It never borrows the transport owner or blocks its control channel.
    pub async fn publish_snapshot(&mut self, physical:Snapshot)->Result<(),String> {
        {
            let current=self.snapshot.read().map_err(|_|"snapshot lock poisoned")?;
            if physical.cursor.boot_id!=current.cursor.boot_id || physical.cursor.sequence<current.cursor.sequence {return Err("snapshot cursor regressed".into());}
        }
        if physical.devices.values().all(|state|state.quality!=homesense_state::Quality::Confirmed) {
            return self.publish_unavailable(physical).await;
        }
        let (saved,recorded)=match self.admission.hap_projection(self.configuration.revision(),self.projection_epoch,physical.clone(),self.max_age_ms).await.map_err(|e|format!("HAP projection: {e:?}"))? {
            crate::admission::HapProjection::Unavailable=>return self.publish_unavailable(physical).await,
            crate::admission::HapProjection::Ready{restore,targets,transport_epoch}=>{self.projection_epoch=Some(transport_epoch);(restore,targets)},
        };
        self.projection.transitions().retain_owned(&recorded);
        let snapshot=light_snapshot(&self.configuration,&physical,self.selected.as_ref(),&recorded)?;
        {
            let mut current=self.snapshot.write().map_err(|_|"snapshot lock poisoned")?;
            if snapshot.cursor.boot_id!=current.cursor.boot_id || snapshot.cursor.sequence<current.cursor.sequence {return Err("snapshot cursor regressed".into());}
            *current=snapshot.clone();
        }
        *self.physical_snapshot.write().map_err(|_|"snapshot lock poisoned")?=physical;
        *self.restore_levels.write().map_err(|_|"restore lock poisoned")?=saved;
        self.projection.refresh_snapshot(&snapshot,self.admission.monotonic_ms()).await.map_err(error)
    }
    /// Local invalidation only: never turn a closed actor lifecycle gate into
    /// known old-generation feedback, and never touch its restore ledger.
    async fn publish_unavailable(&mut self,mut physical:Snapshot)->Result<(),String>{
        for state in physical.devices.values_mut(){if state.quality==homesense_state::Quality::Confirmed{state.quality=homesense_state::Quality::Stale;}}
        {
            let previous=self.physical_snapshot.read().map_err(|_|"physical snapshot lock poisoned")?;
            if physical.devices.len()!=previous.devices.len() || physical.devices.iter().any(|(id,state)|previous.devices.get(id).is_none_or(|old|old.kind!=state.kind)) {
                return Err("unavailable snapshot mapping changed".into());
            }
        }
        let snapshot=light_snapshot(&self.configuration,&physical,self.selected.as_ref(),&Default::default())?;
        self.projection.transitions().retain_owned(&Default::default());
        *self.snapshot.write().map_err(|_|"snapshot lock poisoned")?=snapshot.clone();
        *self.physical_snapshot.write().map_err(|_|"physical snapshot lock poisoned")?=physical;
        self.projection.refresh_snapshot(&snapshot,self.admission.monotonic_ms()).await.map_err(error)
    }
    /// Register the exact graph handles before running the HTTP server.
    /// A failure requires discarding the server; partial startup is not served.
    pub async fn install(&self, server: &hap::server::IpServer) -> Result<(), String> {
        let mut guard = InstallationGuard {
            admission: self.admission.clone(),
            installed: false,
        };
        let mut handles = vec![self.bridge.clone()];
        handles.extend(self.lights.iter().map(|(_, handle)| handle.clone()));
        let result = server
            .install_startup_graph(handles, self.write_handler.clone())
            .await
            .map_err(error);
        guard.installed = result.is_ok();
        result
    }
    /// Publish the read snapshot before emitting corresponding characteristic
    /// events. Retry after an interrupted publication resynchronizes the graph.
    pub async fn publish<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &ObservedTransport<S>,
    ) -> Result<(), String> {
        if owner.configuration_revision() != self.configuration.revision() {
            for snapshot in [&self.snapshot, &self.physical_snapshot] {
                let mut snapshot = snapshot.write().map_err(|_| "snapshot lock poisoned")?;
                for state in snapshot.devices.values_mut() {
                    state.quality = homesense_state::Quality::Stale;
                }
            }
            return Err("feedback and HAP configuration revisions differ".into());
        }
        let cursor = self
            .snapshot
            .read()
            .map_err(|_| "snapshot lock poisoned")?
            .cursor;
        let physical = owner.snapshot();
        let saved = self
            .admission
            .remember_levels(
                self.configuration.revision(),
                physical.clone(),
                self.max_age_ms,
            )
            .await
            .map_err(|e| format!("restore levels: {e:?}"))?;
        *self
            .restore_levels
            .write()
            .map_err(|_| "restore lock poisoned")? = saved;
        let recorded=self.admission.projection_levels(self.configuration.revision(),physical.cursor.boot_id).await.map_err(|e|format!("recorded levels: {e:?}"))?;
        self.projection.transitions().retain_owned(&recorded);
        let snapshot=light_snapshot(&self.configuration,&physical,self.selected.as_ref(),&recorded)?;
        *self
            .physical_snapshot
            .write()
            .map_err(|_| "physical snapshot lock poisoned")? = physical;
        if snapshot.cursor.boot_id != cursor.boot_id || snapshot.cursor.sequence < cursor.sequence {
            return Err("snapshot boot changed or cursor moved backwards".into());
        }
        let now = self.admission.monotonic_ms();
        *self.snapshot.write().map_err(|_| "snapshot lock poisoned")? = snapshot.clone();
        let resync = self.projection_interrupted;
        self.projection_interrupted = true;
        if resync {
            self.projection.resynchronize(&snapshot, now).await.map_err(error)?;
        } else {
            // Reads and notifications share actual steady feedback and only
            // the actor's bounded local transition targets.
            self.projection.refresh_snapshot(&snapshot, now).await.map_err(error)?;
        }
        self.projection_interrupted = false;
        Ok(())
    }
}

/// Stops admission and invalidates reads even if the driving future is dropped.
struct DriveGuard<'a, S: std::io::Read + std::io::Write + Send + 'static> {
    graph: &'a mut RuntimeHap,
    owner: &'a mut ObservedTransport<S>,
}
impl<S: std::io::Read + std::io::Write + Send + 'static> Drop for DriveGuard<'_, S> {
    fn drop(&mut self) {
        self.graph.admission.stop_now();
        self.owner.invalidate();
        if let Ok(mut snapshot) = self.graph.physical_snapshot.write() {
            for state in snapshot.devices.values_mut() {
                state.quality = homesense_state::Quality::Stale;
            }
        }
        if let Ok(mut snapshot) = self.graph.snapshot.write() {
            for state in snapshot.devices.values_mut() {
                if state.quality == homesense_state::Quality::Confirmed {
                    state.quality = homesense_state::Quality::Stale;
                }
            }
        }
    }
}
impl RuntimeHap {
    async fn drive_cycle<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut ObservedTransport<S>,
        gateway: &str,
        stop: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<Option<homesense_core::DeviceId>, String> {
        let mut written = None;
        if let Some(outcome) = owner.step().await? {
            outcome
                .result
                .map_err(|e| format!("gateway write: {e:?}"))?;
            written = Some(outcome.device_id);
        }
        if *stop.borrow() || stop.has_changed().is_err() {
            return Ok(written);
        }
        let read = owner.receive(gateway).await?;
        if !matches!(read.result, hb_transport::Receive::Disconnected { .. }) {
            owner.expire(self.admission.monotonic_ms(), self.max_age_ms)?;
        }
        self.publish(owner).await?;
        if matches!(read.result, hb_transport::Receive::Disconnected { .. }) {
            return Err(format!("gateway {} disconnected", read.gateway_id));
        }
        Ok(written)
    }

    /// Drive already-established bounded streams. Does not connect, advertise,
    /// synchronize a Live gateway, reconnect or replay commands.
    /// Each turn dispatches at most one command and reads one gateway, round-robin.
    /// Graceful stop waits for the current bounded I/O operation; aborting this
    /// future poisons the transport and invalidates HAP reads synchronously.
    pub async fn drive<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut ObservedTransport<S>,
        cadence: std::time::Duration,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        self.drive_with_scenes(owner, cadence, stop, None).await
    }
    /// Explicit scene-enabled drive; the scheduler must belong to this admission
    /// owner. Existing service entry points do not opt in automatically.
    pub async fn drive_with_scenes<S: std::io::Read + std::io::Write + Send + 'static>(
        &mut self,
        owner: &mut ObservedTransport<S>,
        cadence: std::time::Duration,
        mut stop: tokio::sync::watch::Receiver<bool>,
        scenes: Option<&mut crate::scene_scheduler::SceneScheduler>,
    ) -> Result<(), String> {
        let mut scenes = SceneDriveGuard(scenes);
        let guard = DriveGuard { graph: self, owner };
        let gateways = guard.owner.gateway_ids();
        let result = async {
            if gateways.is_empty()
                || cadence.is_zero()
                || cadence > std::time::Duration::from_secs(1)
            {
                return Err("drive requires gateways and a cadence in (0, 1s]".into());
            }
            if scenes
                .0
                .as_ref()
                .is_some_and(|s| !s.matches_admission(&guard.graph.admission))
            {
                return Err("scene scheduler belongs to another admission owner".into());
            }
            let mut next = 0;
            loop {
                if *stop.borrow() || stop.has_changed().is_err() {
                    return Ok(());
                }
                if let Some(scheduler) = &mut scenes.0 {
                    for result in scheduler.tick_with_feedback(guard.graph.admission.monotonic_ms(),Some((guard.owner.feedback(),guard.graph.max_age_ms))).await? {
                        if let Err(error) = result.result {
                            log::warn!(
                                "scene run={} status={:?}: {error}",
                                result.run,
                                result.status
                            );
                        }
                    }
                }
                guard
                    .graph
                    .drive_cycle(guard.owner, &gateways[next], &stop)
                    .await?;
                next = (next + 1) % gateways.len();
                tokio::select! {
                    _ = tokio::time::sleep(cadence) => {},
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() { return Ok(()); }
                    }
                }
            }
        }
        .await;
        if let Some(scheduler) = &mut scenes.0 {
            for (id, result) in scheduler.stop().await {
                if let Err(error) = result {
                    log::warn!("scene shutdown run={id}: {error}");
                }
            }
        }
        guard.graph.admission.stop_now();
        guard.owner.invalidate();
        let publication = guard.graph.publish(guard.owner).await;
        let shutdown = guard
            .graph
            .admission
            .clone()
            .shutdown()
            .await
            .map_err(|e| format!("shutdown: {e:?}"));
        result.and(publication).and(shutdown)
    }
}

/// Bounds for a live TCP supervision run. No defaults enable hardware access.
pub struct SupervisorOptions {
    pub initial_epoch: u64,
    pub retry: crate::reconnect::RetryPolicy,
    pub cadence: std::time::Duration,
    pub query_timeout_ms: u64,
    pub recovery_limit: u8,
    pub coexistence_confirmed: bool,
}
impl RuntimeHap {
    /// Drive existing TCP streams and recover faults using fresh connections,
    /// read-only queries and audited re-admission. User stop/cancellation remains
    /// terminal. The caller still owns HAP serving and discovery.
    pub async fn drive_supervised(
        &mut self,
        owner: &mut ObservedTransport<std::net::TcpStream>,
        config: &Configuration,
        options: SupervisorOptions,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        let guard = DriveGuard { graph: self, owner };
        let result = async {
            if !options.coexistence_confirmed
                || options.initial_epoch == 0
                || options.cadence.is_zero()
                || options.cadence > std::time::Duration::from_secs(1)
                || options.recovery_limit == 0
                || options.recovery_limit > 16
                || options.query_timeout_ms == 0
                || options.query_timeout_ms > 5000
            {
                return Err("invalid supervisor scope or bounds".into());
            }
            let endpoints: std::collections::BTreeMap<_, _> = config
                .gateways()
                .iter()
                .map(|(id, gateway)| (id.clone(), gateway.endpoint))
                .collect();
            let gateways = guard.owner.gateway_ids();
            if gateways.is_empty() || gateways != endpoints.keys().cloned().collect::<Vec<_>>() {
                return Err("supervisor gateway mapping mismatch".into());
            }
            let mut epoch = options.initial_epoch;
            let mut recoveries = 0;
            let mut next = 0;
            let refresh_targets=crate::physical_queries::plan(config,homesense_core::ExecutionContext::Live)?;
            let refresh_interval=crate::physical_queries::policy_interval(&refresh_targets,guard.owner.write_interval_ms(),guard.graph.max_age_ms,guard.graph.configuration.refresh_policy())?;
            let mut refresh_next = 0;
            let mut interactive_until = 0u64;
            let mut refresh_at = guard.graph.admission.monotonic_ms().saturating_add(refresh_interval);
            loop {
                if *stop.borrow() || stop.has_changed().is_err() {
                    return Ok(());
                }
                let cycle = async {
                    let written = guard.graph.drive_cycle(guard.owner, &gateways[next], &stop).await?;
                    if let Some(device) = written.filter(|id|config.binding(id).is_some()) {
                        let now = guard.graph.admission.monotonic_ms();
                        // AD8 supports hardware ramps up to 15 seconds. Observe
                        // the transition at a bounded higher rate, then back off.
                        interactive_until = now.saturating_add(16_000);
                        refresh_at = refresh_at.min(now.saturating_add(refresh_interval.min(250)));
                        tokio::select! {
                            result = guard.owner.refresh(&device, config, homesense_core::ExecutionContext::Live,
                                options.query_timeout_ms, guard.graph.max_age_ms) => result?,
                            _ = stop.changed() => return Ok(()),
                        }
                        guard.graph.publish(guard.owner).await?;
                    }

                    if guard.graph.admission.monotonic_ms() >= refresh_at {
                        tokio::select! {
                            result = guard.owner.refresh_physical(config, &refresh_targets[refresh_next], homesense_core::ExecutionContext::Live,
                                options.query_timeout_ms, guard.graph.max_age_ms) => result?,
                            _ = stop.changed() => return Ok(()),
                        }
                        guard.graph.publish(guard.owner).await?;
                        refresh_next = (refresh_next + 1) % refresh_targets.len();
                        let now = guard.graph.admission.monotonic_ms();
                        let interval = if now < interactive_until { refresh_interval.min(250) } else { refresh_interval };
                        refresh_at = now.saturating_add(interval);
                    }
                    Ok::<(), String>(())
                }.await;
                if let Err(error) = cycle {
                    log::warn!("gateway_cycle_failed epoch={epoch} error={error:?}");
                    let mut last_error = error;
                    loop {
                        guard.owner.invalidate();
                        guard.graph.admission.transport_failed().await
                            .map_err(|e| format!("fault audit: {e:?}"))?;
                        guard.graph.publish(guard.owner).await?;
                        if *stop.borrow() || stop.has_changed().is_err() { return Ok(()); }
                        if recoveries >= options.recovery_limit {
                            return Err(format!("recovery limit: {last_error}"));
                        }
                        recoveries += 1;
                        log::info!("gateway_recovery_start previous_epoch={epoch} recovery={recoveries}");
                        let (transport, new_epoch) = crate::reconnect::reconnect_tcp(
                            guard.graph.admission.clone(), endpoints.clone(), epoch, options.retry,
                            options.coexistence_confirmed, stop.clone()).await?;
                        epoch = new_epoch;
                        guard.owner.recover(transport, epoch)?;
                        let synchronization = tokio::select! {
                            result = guard.owner.query_recovery(config, homesense_core::ExecutionContext::Live,
                                options.query_timeout_ms, guard.graph.max_age_ms) => result,
                            _ = stop.changed() => return Ok(()),
                        };
                        match synchronization {
                            Ok(()) => { log::info!("gateway_recovery_synchronized epoch={epoch} revision={} query_frames={}", config.revision(), refresh_targets.len()); guard.graph.publish(guard.owner).await?; interactive_until = 0; refresh_next = 0; refresh_at = guard.graph.admission.monotonic_ms().saturating_add(refresh_interval); break; }
                            Err(error) => { log::warn!("gateway_recovery_query_failed epoch={epoch} error={error:?}"); last_error = error; }
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(options.retry.initial_delay) => {},
                            _ = stop.changed() => return Ok(()),
                        }
                    }
                }
                next = (next + 1) % gateways.len();
                tokio::select! {
                    _ = tokio::time::sleep(options.cadence) => {},
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() { return Ok(()); }
                    }
                }
            }
        }
        .await;
        guard.graph.admission.stop_now();
        guard.owner.invalidate();
        let publication = guard.graph.publish(guard.owner).await;
        let shutdown = guard
            .graph
            .admission
            .clone()
            .shutdown()
            .await
            .map_err(|e| format!("shutdown: {e:?}"));
        result.and(publication).and(shutdown)
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[tokio::test]
    async fn selected_bridges_share_physical_feedback_without_exposing_each_other() {
        let dir=std::env::temp_dir().join(format!("homesense-selected-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut raw:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        let mut second=raw["devices"][0].clone();second["id"]="second".into();second["channel"]=2.into();raw["devices"].as_array_mut().unwrap().push(second);
        let mut store=homesense_storage::Store::open(dir.join("runtime.sqlite")).unwrap();
        let config=store.publish_configuration(None,&serde_json::to_vec(&raw).unwrap(),homesense_core::ExecutionContext::Simulation,&[]).unwrap();drop(store);
        let admission=Admission::open(dir.join("runtime.sqlite"),crate::admission::Options{context:homesense_core::ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
        let boot=uuid::Uuid::parse_str(admission.boot_id()).unwrap().as_u128();
        let mut physical=crate::feedback::Feedback::new(&config,homesense_core::ExecutionContext::Simulation,boot,1).unwrap().snapshot();
        for state in physical.devices.values_mut() {state.quality=homesense_state::Quality::Confirmed;state.observed=Some(homesense_core::Observation::Level(homesense_core::Percent::new(30).unwrap()));state.observed_at_ms=Some(0);}
        let a=homesense_core::DeviceId::new("simulation:light").unwrap();let b=homesense_core::DeviceId::new("second").unwrap();
        let mut graphs=Vec::new();
        for id in [&a,&b] {
            let mut identity=IdentityStore::open(dir.join(if id==&a {"a"}else{"b"})).await.unwrap();
            hap::storage::Storage::save_config(&mut identity,&hap::Config::default()).await.unwrap();
            for invalid in [[homesense_core::DeviceId::new("missing").unwrap()].into()] {
                assert!(RuntimeHap::new_selected(&config,physical.clone(),&identity,admission.clone(),5000,Some(invalid)).await.is_err());
            }
            let graph=RuntimeHap::new_selected(&config,physical.clone(),&identity,admission.clone(),5000,Some([id.clone()].into())).await.unwrap();
            assert_eq!(graph.lights.len(),1);assert_eq!(&graph.lights[0].0.device,id);
            assert_eq!(graph.snapshot.read().unwrap().devices.len(),1);
            assert_eq!(graph.physical_snapshot.read().unwrap().devices.len(),2);
            graphs.push(graph);
        }
        physical.cursor.sequence+=1;physical.state_version=physical.cursor.sequence;
        physical.devices.get_mut(&b).unwrap().observed=Some(homesense_core::Observation::Level(homesense_core::Percent::new(70).unwrap()));
        for graph in &mut graphs {graph.publish_snapshot(physical.clone()).await.unwrap();}
        assert_eq!(graphs[0].snapshot.read().unwrap().devices[&a].observed,Some(homesense_core::Observation::Level(homesense_core::Percent::new(30).unwrap())));
        assert!(!graphs[0].snapshot.read().unwrap().devices.contains_key(&b));
        assert_eq!(graphs[1].snapshot.read().unwrap().devices[&b].observed,Some(homesense_core::Observation::Level(homesense_core::Percent::new(70).unwrap())));
        assert!(!graphs[1].snapshot.read().unwrap().devices.contains_key(&a));
        drop(graphs);admission.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn light_view_excludes_motor_and_keypad_without_mutating_physical_snapshot() {
        let mut value:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        for (id,address,profile) in [("motor","22","c4_din_pro_v2_7_7"),("key","41","ts6_86_rs485_v2_6_5_6key")] {
            let mut module=serde_json::json!({"address":address,"profile":profile,"verification":{"status":"simulated"}});
            if id=="key" {module["led_mode"]="manual".into();}
            value["gateways"][0]["modules"].as_array_mut().unwrap().push(module);
            value["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":id,"name":id,"gateway":"simulation","address":address,"channel":1,"enabled":true}));
        }
        let config=Configuration::parse(&serde_json::to_vec(&value).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let physical=crate::feedback::Feedback::new(&config,homesense_core::ExecutionContext::Simulation,1,1).unwrap().snapshot();
        let prior=physical.clone();
        let lights=light_snapshot(&config,&physical,None,&Default::default()).unwrap();
        assert_eq!(physical,prior);assert_eq!(physical.devices.len(),3);
        assert_eq!(lights.devices.len(),1);assert!(lights.devices.contains_key(&homesense_core::DeviceId::new("simulation:light").unwrap()));
        assert_eq!(lights.cursor,physical.cursor);
    }

    #[tokio::test]
    async fn changed_feedback_revision_invalidates_old_hap_projection() {
        let dir =
            std::env::temp_dir().join(format!("homesense-graph-revision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let document = include_bytes!("../../../config/simulation.json");
        let mut store = homesense_storage::Store::open(dir.join("runtime.sqlite")).unwrap();
        let config = store
            .publish_configuration(
                None,
                document,
                homesense_core::ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        drop(store);
        let admission = Admission::open(
            dir.join("runtime.sqlite"),
            crate::admission::Options {
                context: homesense_core::ExecutionContext::Simulation,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .unwrap();
        let boot = uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128();
        let feedback = crate::feedback::Feedback::new(
            &config,
            homesense_core::ExecutionContext::Simulation,
            boot,
            1,
        )
        .unwrap();
        let mut snapshot = feedback.snapshot();
        for state in snapshot.devices.values_mut() {
            state.quality = homesense_state::Quality::Confirmed;
            state.observed = Some(homesense_core::Observation::Level(
                homesense_core::Percent::new(30).unwrap(),
            ));
            state.observed_at_ms = Some(0);
        }
        let mut identity = homesense_hap::storage::IdentityStore::open(dir.join("identity"))
            .await
            .unwrap();
        hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
            .await
            .unwrap();
        let mut graph = RuntimeHap::new(&config, snapshot, &identity, admission.clone(), 5000)
            .await
            .unwrap();
        let mut next: hap::serde_json::Value = hap::serde_json::from_slice(document).unwrap();
        next["revision"] = 2.into();
        next["devices"][0]["channel"] = 2.into();
        let next_document = hap::serde_json::to_vec(&next).unwrap();
        admission
            .begin_publication(next_document.clone(), 1, std::time::Duration::from_secs(3))
            .await
            .unwrap();
        admission.commit_publication().await.unwrap();
        admission.synchronized(1, 2).await.unwrap();
        let binding = &graph.lights[0].0;
        let body = format!(
            "{{\"characteristics\":[{{\"aid\":{},\"iid\":{},\"value\":40}}]}}",
            binding.aid,
            binding.brightness_iid.unwrap()
        );
        assert!(
            graph
                .write_handler
                .write_async(uuid::Uuid::new_v4(), body.as_bytes())
                .await
                .is_err()
        );
        assert!(admission.dispatch().await.unwrap().is_none());
        let next = homesense_config::Configuration::parse(
            &next_document,
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        let feedback = crate::feedback::Feedback::new(
            &next,
            homesense_core::ExecutionContext::Simulation,
            boot,
            1,
        )
        .unwrap();
        let transport = crate::transport_owner::TransportOwner::<std::io::Cursor<Vec<u8>>>::new(
            admission.clone(),
            Default::default(),
        );
        let owner = ObservedTransport::new(transport, feedback);
        let new_graph =
            RuntimeHap::new(&next, owner.snapshot(), &identity, admission.clone(), 5000)
                .await
                .unwrap();
        assert_eq!(new_graph.lights[0].0.aid, graph.lights[0].0.aid);
        new_graph
            .write_handler
            .write_async(uuid::Uuid::new_v4(), body.as_bytes())
            .await
            .unwrap();
        let ticket = admission.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.epochs.configuration, 2);
        admission
            .finish_write(ticket.command.id, true)
            .await
            .unwrap();
        assert!(
            graph
                .publish(&owner)
                .await
                .unwrap_err()
                .contains("revisions differ")
        );
        assert!(
            graph
                .snapshot
                .read()
                .unwrap()
                .devices
                .values()
                .all(|s| s.quality == homesense_state::Quality::Stale)
        );
        assert!(
            graph
                .physical_snapshot
                .read()
                .unwrap()
                .devices
                .values()
                .all(|s| s.quality == homesense_state::Quality::Stale)
        );
        admission.shutdown().await.unwrap();
        drop(graph);
        drop(identity);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
#[path="runtime_hap_publication_tests.rs"]
mod unavailable_publication_tests;

#[cfg(test)]
mod logical_write_tests {
    use super::*;
    #[test]
    fn shared_resource_hap_write_rejects_mixed_batch_before_submission() {
        let logical=homesense_core::DeviceId::new("composed").unwrap();
        let ordinary=homesense_core::DeviceId::new("ordinary").unwrap();
        let level=homesense_core::Percent::new(45).unwrap();
        let command=homesense_core::Command::SetLevel{level,transition:homesense_core::LevelTransition::Immediate};
        let protected=std::collections::BTreeSet::from([logical.clone()]);
        assert!(matches!(logical_command(&[(logical.clone(),command),(ordinary.clone(),command)],&protected),Err(hap::server::WriteStatus::InvalidValueInRequest)));
        assert!(matches!(logical_command(&[(logical,command)],&protected).unwrap().unwrap().1,crate::management_devices::DeviceAction::Brightness{level:45}));
        assert!(logical_command(&[(ordinary,command)],&protected).unwrap().is_none());
    }
}
