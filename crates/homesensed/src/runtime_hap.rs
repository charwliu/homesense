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
    physical_snapshot: Arc<RwLock<Snapshot>>,
    combined: bool,
    restore_levels:
        Arc<RwLock<std::collections::BTreeMap<homesense_core::DeviceId, homesense_core::Percent>>>,
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
impl RuntimeHap {
    pub async fn new(
        configuration: &Configuration,
        initial: Snapshot,
        store: &IdentityStore,
        admission: Admission,
        max_age_ms: u64,
    ) -> Result<Self, String> {
        let combined = configuration
            .devices()
            .values()
            .any(|d| d.enabled && d.supply_relay.is_some());
        let restore_levels = Arc::new(RwLock::new(
            admission
                .remember_levels(configuration.revision(), initial.clone(), max_age_ms)
                .await
                .map_err(|e| format!("restore levels: {e:?}"))?,
        ));
        let physical_snapshot = Arc::new(RwLock::new(initial.clone()));
        let initial = crate::combined_light::project(configuration, &initial).map_err(error)?;
        let supplies: std::collections::BTreeSet<_> = configuration
            .devices()
            .values()
            .filter(|d| d.enabled)
            .filter_map(|d| d.supply_relay.as_ref())
            .collect();
        let mut specs = Vec::new();
        for id in configuration.devices().keys() {
            if supplies.contains(id) {
                continue;
            }
            if let Some((device, module)) = configuration.binding(id) {
                let dimmable = match module.kind() {
                    ModuleKind::Dimmer => true,
                    ModuleKind::Switch => false,
                    _ => return Err("runtime HAP graph currently supports lights only".into()),
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
        let mut graph = LightGraph::build(store, "HomeSense", specs)
            .await
            .map_err(error)?;
        graph.install_observed_reads(view.clone());
        let requests = LightRequests::new(graph.bindings.clone()).map_err(error)?;
        let revision = configuration.revision();
        let owner = admission.clone();
        let restore = restore_levels.clone();
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
        let write_handler = Arc::new(AsyncLightWriteHandler::new(
            requests,
            move |id: &homesense_core::DeviceId| restore.read().ok()?.get(id).copied(),
            move |batch: homesense_hap::requests::Batch| {
                let owner = owner.clone();
                let transitions = transitions.clone();
                let writes = writes.clone();
                async move {
                    let _serial = writes.lock().await;
                    let intents: Vec<_> = batch
                        .intents
                        .iter()
                        .map(|i| (i.device.clone(), i.command))
                        .collect();
                    owner.submit_for_revision(revision, batch).await?;
                    for (device, command) in intents {
                        transitions.accepted(device, command, owner.monotonic_ms());
                    }
                    Ok(())
                }
            },
        ));
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
            physical_snapshot,
            combined,
            restore_levels,
        })
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
        let snapshot =
            crate::combined_light::project(&self.configuration, &physical).map_err(error)?;
        *self
            .physical_snapshot
            .write()
            .map_err(|_| "physical snapshot lock poisoned")? = physical;
        if snapshot.cursor.boot_id != cursor.boot_id || snapshot.cursor.sequence < cursor.sequence {
            return Err("snapshot boot changed or cursor moved backwards".into());
        }
        let events = owner.events_since(cursor);
        let now = self.admission.monotonic_ms();
        *self
            .snapshot
            .write()
            .map_err(|_| "snapshot lock poisoned")? = snapshot.clone();
        let resync = self.projection_interrupted;
        self.projection_interrupted = true;
        match events {
            _ if self.combined && !resync => self
                .projection
                .refresh_snapshot(&snapshot, now)
                .await
                .map_err(error)?,
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
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        let guard = DriveGuard { graph: self, owner };
        let gateways = guard.owner.gateway_ids();
        let result = async {
            if gateways.is_empty()
                || cadence.is_zero()
                || cadence > std::time::Duration::from_secs(1)
            {
                return Err("drive requires gateways and a cadence in (0, 1s]".into());
            }
            let mut next = 0;
            loop {
                if *stop.borrow() || stop.has_changed().is_err() {
                    return Ok(());
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
            let refresh_devices: Vec<_> = config.devices().keys()
                .filter(|id| config.binding(id).is_some()).cloned().collect();
            if refresh_devices.is_empty() {
                return Err("supervisor requires enabled devices".into());
            }
            let refresh_interval = (guard.graph.max_age_ms / 2 / refresh_devices.len() as u64).max(1);
            let mut refresh_next = 0;
            let mut interactive_until = 0u64;
            let mut refresh_at = guard.graph.admission.monotonic_ms().saturating_add(refresh_interval);
            loop {
                if *stop.borrow() || stop.has_changed().is_err() {
                    return Ok(());
                }
                let cycle = async {
                    let written = guard.graph.drive_cycle(guard.owner, &gateways[next], &stop).await?;
                    if let Some(device) = written {
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
                            result = guard.owner.refresh(&refresh_devices[refresh_next], config, homesense_core::ExecutionContext::Live,
                                options.query_timeout_ms, guard.graph.max_age_ms) => result?,
                            _ = stop.changed() => return Ok(()),
                        }
                        guard.graph.publish(guard.owner).await?;
                        refresh_next = (refresh_next + 1) % refresh_devices.len();
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
                            Ok(()) => { log::info!("gateway_recovery_synchronized epoch={epoch} revision={} devices={}", config.revision(), refresh_devices.len()); guard.graph.publish(guard.owner).await?; interactive_until = 0; refresh_next = 0; refresh_at = guard.graph.admission.monotonic_ms().saturating_add(refresh_interval); break; }
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
