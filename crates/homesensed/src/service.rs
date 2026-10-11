//! Bounded-duration HAP simulation; gateway endpoints are never connected.
use crate::{
    admission::{Admission, Options},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    runtime_hap::RuntimeHap,
    transport_owner::TransportOwner,
};
use hap::{
    server::{IpServer, Server},
    storage::Storage,
};
use homesense_core::ExecutionContext;
use homesense_hap::storage::IdentityStore;
use homesense_simulator::{Faults, Gateway};
use homesense_storage::Store;
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
struct SimulatedStream {
    gateway: Gateway,
    clock: Instant,
    pending: VecDeque<u8>,
    queries: Vec<[u8; 5]>,
    next_query_ms: u64,
}
impl SimulatedStream {
    fn now(&self) -> u64 {
        self.clock
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}
impl Read for SimulatedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let now = self.now();
        if now >= self.next_query_ms {
            for query in &self.queries {
                for result in self.gateway.receive(query, now).map_err(io::Error::other)? {
                    result.map_err(io::Error::other)?;
                }
            }
            self.next_query_ms = now.saturating_add(1000);
        }
        for chunk in self.gateway.drain_due(now).map_err(io::Error::other)? {
            self.pending.extend(chunk);
        }
        if self.pending.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = bytes.len().min(self.pending.len());
        for byte in &mut bytes[..count] {
            *byte = self.pending.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl Write for SimulatedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let now = self.now();
        for result in self.gateway.receive(bytes, now).map_err(io::Error::other)? {
            result.map_err(io::Error::other)?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Explicit loopback prototype, preserving identity and configuration between runs.
/// Configured gateway endpoints are never connected. Configuration replacement
/// requires a separate publication workflow, not an implicit startup overwrite.
pub async fn serve_simulation(
    document: &[u8],
    directory: &Path,
    address: SocketAddr,
    duration: Duration,
) -> Result<(), String> {
    serve_simulation_inner(document, directory, address, duration, false).await
}

/// Explicit LAN simulation for real controller interoperability. Uses the same
/// in-memory bus and durable admission as loopback; never connects configured
/// hardware endpoints. The caller supplies a concrete private interface address.
pub async fn serve_simulation_lan(
    document: &[u8],
    directory: &Path,
    address: SocketAddr,
    duration: Duration,
) -> Result<(), String> {
    let private = match address.ip() {
        std::net::IpAddr::V4(ip) => ip.is_private(),
        std::net::IpAddr::V6(ip) => ip.is_unique_local(),
    };
    if !private {
        return Err("LAN simulation requires a concrete private interface address".into());
    }
    serve_simulation_inner(document, directory, address, duration, true).await
}

pub(crate) fn random_pin() -> hap::Pin {
    loop {
        let bytes = *Uuid::new_v4().as_bytes();
        let mut digits = [0; 8];
        // UUID random bytes outside its version/variant positions, rejection
        // sampling to avoid bias when converting random octets to decimal.
        let samples: Vec<_> = bytes
            .iter()
            .enumerate()
            .filter(|(i, b)| *i != 6 && *i != 8 && **b < 250)
            .map(|(_, b)| b % 10)
            .collect();
        if samples.len() < 8 {
            continue;
        }
        digits.copy_from_slice(&samples[..8]);
        if let Ok(pin) = hap::Pin::new(digits) {
            return pin;
        }
    }
}
async fn serve_simulation_inner(
    document: &[u8],
    directory: &Path,
    address: SocketAddr,
    duration: Duration,
    advertise: bool,
) -> Result<(), String> {
    if (!advertise && !address.ip().is_loopback())
        || address.port() == 0
        || duration.is_zero()
        || duration > Duration::from_secs(86400)
    {
        return Err(
            "simulation service needs an allowed address, nonzero port and duration in (0, 24h]"
                .into(),
        );
    }
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Simulation)
        .map_err(error)?;
    if config.gateways().is_empty() {
        return Err("at least one gateway required".into());
    }
    std::fs::create_dir_all(directory).map_err(error)?;
    let database = directory.join("runtime.sqlite");
    let mut store = Store::open(&database).map_err(error)?;
    match store.active().map_err(error)? {
        Some(active) if active.payload != document => {
            return Err(
                "stored configuration differs; explicit configuration publication required".into(),
            );
        }
        Some(_) => {}
        None => {
            store
                .publish_configuration(None, document, ExecutionContext::Simulation, &[])
                .map_err(error)?;
        }
    }
    drop(store);
    let mut identity = IdentityStore::open(directory.join("identity"))
        .await
        .map_err(error)?;
    let authorize = identity.clone();
    let admission = Admission::open(
        database,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| authorize.controller_is_paired(id).unwrap_or(false),
    )
    .await?;
    let result = async {
        let settings = if identity.has_config().await.map_err(error)? {
            let saved = identity.load_config().await.map_err(error)?;
            if saved.host != address.ip() || saved.port != address.port() {
                return Err("stored HAP endpoint differs".into());
            }
            saved
        } else {
            let settings = hap::Config {
                host: address.ip(),
                port: address.port(),
                name: "HomeSense Simulation".into(),
                category: hap::accessory::AccessoryCategory::Bridge,
                pin: random_pin(),
                ..Default::default()
            };
            identity.save_config(&settings).await.map_err(error)?;
            settings
        };
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id())
                .map_err(error)?
                .as_u128(),
            1,
        )?;
        let mut graph = RuntimeHap::new(
            &config,
            feedback.snapshot(),
            &identity,
            admission.clone(),
            5000,
        )
        .await?;
        let connections = simulation_connections(&config)?;
        let mut owner = ObservedTransport::new(
            TransportOwner::new(admission.clone(), connections),
            feedback,
        );
        let server = if advertise {
            IpServer::new(settings, identity.clone()).await
        } else {
            IpServer::new_loopback(settings, identity.clone()).await
        }
        .map_err(error)?;
        graph.install(&server).await?;
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let drive = graph.drive(&mut owner, Duration::from_millis(10), receiver);
        tokio::pin!(drive);
        tokio::select! {
            result = server.run_handle() => {
                let _ = stop.send(true);
                drive.await?;
                result.map_err(error)
            },
            result = &mut drive => result,
            _ = tokio::time::sleep(duration) => {let _ = stop.send(true); drive.await}
        }
    }
    .await;
    admission.stop_now();
    let _ = admission.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simulation_queries_initial_and_periodic_observations_without_control() {
        use homesense_core::{BusAddress, ModuleSpec, ProtocolProfile, Verification};
        let module = ModuleSpec::new(
            BusAddress::new(0x15).unwrap(),
            ProtocolProfile::GtD8Rl,
            Verification::Simulated,
        )
        .unwrap();
        let mut stream = SimulatedStream {
            gateway: Gateway::new(vec![module], Faults::default(), 128).unwrap(),
            clock: Instant::now(),
            pending: VecDeque::new(),
            queries: vec![[0xAE, 0x15, 0xA1, 0xF2, 0xEE]],
            next_query_ms: 0,
        };
        let mut bytes = [0; 512];
        let count = stream.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], &[0xAE, 0x15, 0xD1, 0, 0xEE]);
        assert_eq!(
            stream.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        stream.clock = Instant::now() - Duration::from_secs(2);
        let count = stream.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], &[0xAE, 0x15, 0xD1, 0, 0xEE]);
    }
}

fn simulation_connections(
    config: &homesense_config::Configuration,
) -> Result<BTreeMap<String, hb_transport::Connection<SimulatedStream>>, String> {
    let mut connections = BTreeMap::new();
    for (id, gateway) in config.gateways() {
        let mut queries = Vec::new();
        for device_id in config.devices().keys() {
            if let Some((device, module)) = config.binding(device_id)
                && device.gateway == *id
            {
                queries.push(
                    hb_protocol::encode_in_context(
                        module,
                        device.channel,
                        homesense_core::Command::Query,
                        ExecutionContext::Simulation,
                    )
                    .map_err(error)?
                    .bytes(),
                );
            }
        }
        let stream = SimulatedStream {
            gateway: Gateway::new(
                gateway.modules().values().cloned().collect(),
                Faults::default(),
                128,
            )
            .map_err(error)?,
            clock: Instant::now(),
            pending: VecDeque::new(),
            queries,
            next_query_ms: 0,
        };
        connections.insert(
            id.clone(),
            hb_transport::Connection::for_simulation(stream, 1),
        );
    }
    Ok(connections)
}

/// Isolated automation service. Uses a separate database and in-memory gateways;
/// neither runtime.sqlite nor the HAP identity directory is opened.
pub async fn serve_automation_simulation(
    devices: &[u8],
    rules: &[u8],
    directory: &Path,
    duration: Duration,
    scene: Option<&str>,
) -> Result<(), String> {
    if duration.is_zero() || duration > Duration::from_secs(86400) {
        return Err("duration must be in (0,24h]".into());
    }
    let config = homesense_config::Configuration::parse(devices, ExecutionContext::Simulation)
        .map_err(error)?;
    if config.gateways().is_empty() {
        return Err("at least one simulated gateway required".into());
    }
    let document = crate::configuration_publish::validate_automation(
        &config,
        rules,
        ExecutionContext::Simulation,
    )
    .map_err(error)?;
    if let Some(id) = scene
        && !document.timed_scenes.get(id).is_some_and(|s| s.enabled) {
        return Err("enabled timed scene required".into());
    }
    std::fs::create_dir_all(directory).map_err(error)?;
    let path = directory.join("automation-simulation.sqlite");
    let mut store = Store::open(&path).map_err(error)?;
    if let Some(existing) = store.active().map_err(error)? {
        if existing.payload != devices {
            return Err("simulation configuration differs; use a new simulation directory".into());
        }
        // Stored rule bytes are immutable. Compare through the published bundle
        // source rather than accepting a caller-supplied replacement on restart.
        if store
            .automation_payload(existing.revision)
            .map_err(error)?
            .as_deref()
            != Some(rules)
        {
            return Err("simulation automation differs; use a new simulation directory".into());
        }
    } else {
        store
            .publish_automation(None, devices, rules, ExecutionContext::Simulation, &[])
            .map_err(error)?;
    }
    drop(store);
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await?;
    let result = async {
        admission
            .synchronized(1, config.revision())
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut runtime = crate::automation_runtime::AutomationRuntime::load(
            admission.clone(),
            controller,
            wall_ms()?,
            1000,
            64,
            Default::default(),
        )
        .await?;
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id())
                .map_err(error)?
                .as_u128(),
            1,
        )?;
        let mut owner = ObservedTransport::new(
            TransportOwner::new(admission.clone(), simulation_connections(&config)?),
            feedback,
        );
        if let Some(scene) = scene {
            runtime.start_scene(scene).await?;
        }
        let (stop, receive) = tokio::sync::watch::channel(false);
        let drive = runtime.drive(
            &mut owner,
            Duration::from_millis(10),
            5000,
            receive,
            wall_ms,
        );
        tokio::pin!(drive);
        tokio::select! {
            result=&mut drive=>result,
            _=tokio::time::sleep(duration)=>{let _=stop.send(true);drive.await}
        }
    }
    .await;
    admission.stop_now();
    let _ = admission.shutdown().await;
    result
}
fn wall_ms() -> Result<i64, String> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?
            .as_millis(),
    )
    .map_err(error)
}
