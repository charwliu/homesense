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
pub(crate) struct SimulatedStream {
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
    simulation_connections_at(config, 1)
}
pub(crate) fn simulation_connections_at(
    config: &homesense_config::Configuration,
    epoch: u64,
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
            hb_transport::Connection::for_simulation(stream, epoch),
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
    serve_automation_simulation_with_publication(devices, rules, directory, duration, scene, None).await
}
/// Optional single simulated publication. No live endpoints or HAP identities.
pub struct SimulationPublication<'a> {
    pub devices: &'a [u8],
    pub rules: &'a [u8],
    pub after: Duration,
    pub scene: Option<&'a str>,
}
pub async fn serve_automation_simulation_with_publication(
    devices: &[u8], rules: &[u8], directory: &Path, duration: Duration,
    scene: Option<&str>, publication: Option<SimulationPublication<'_>>,
) -> Result<(), String> {
    serve_automation_simulation_options(devices,rules,directory,duration,scene,publication,None,None,#[cfg(test)] None).await
}
/// Explicit local-only management simulation; credentials are supplied out of band.
pub struct WebSettings { pub address:std::net::SocketAddr, pub secret:String, pub administrator:bool, pub tls:Option<crate::web_tls::Settings> }
pub type WebSimulation = WebSettings;
pub async fn serve_web_simulation(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,web:WebSimulation)->Result<(),String> {
    if let Some(tls)=&web.tls {tls.validate_address(web.address)?;}
    if (web.tls.is_none() && !web.address.ip().is_loopback()) || web.address.port()==0 {return Err("Web simulation requires a loopback address and nonzero port".into());}
    serve_automation_simulation_options(devices,rules,directory,duration,None,None,Some(web),None,#[cfg(test)] None).await
}
pub struct UnifiedSimulation {pub address:std::net::SocketAddr,pub scenes:Vec<String>}
/// Shared HAP/Web simulation. Configuration activation is unavailable until
/// publication can rebuild the HAP graph as well as the automation runtime.
pub async fn serve_combined_simulation(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,hap:UnifiedSimulation,web:WebSettings)->Result<(),String> {
    if !hap.address.ip().is_loopback() || hap.address.port()==0 || hap.scenes.is_empty() {return Err("unified simulation needs loopback endpoint and selected scenes".into());}
    if let Some(tls)=&web.tls {tls.validate_address(web.address)?;}
    else if !web.address.ip().is_loopback() || web.address.port()==0 {return Err("plaintext management requires loopback".into());}
    if hap.address==web.address {return Err("HAP and Web require distinct endpoints".into());}
    serve_automation_simulation_options(devices,rules,directory,duration,None,None,Some(web),Some(hap),#[cfg(test)] None).await
}

pub async fn serve_unified_simulation(devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,hap:UnifiedSimulation)->Result<(),String> {
    if !hap.address.ip().is_loopback() || hap.address.port()==0 || hap.scenes.is_empty() {return Err("unified simulation needs loopback endpoint and selected scenes".into());}
    serve_automation_simulation_options(devices,rules,directory,duration,None,None,None,Some(hap),#[cfg(test)] None).await
}
#[allow(clippy::too_many_arguments)]
async fn serve_automation_simulation_options(
    devices:&[u8],rules:&[u8],directory:&Path,duration:Duration,scene:Option<&str>,publication:Option<SimulationPublication<'_>>,web:Option<WebSimulation>,hap:Option<UnifiedSimulation>,#[cfg(test)] hap_failure_after:Option<Duration>
)->Result<(),String> {
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
    let next_config = if let Some(next) = &publication {
        if next.after.is_zero() || next.after >= duration { return Err("publication time must be inside simulation duration".into()); }
        let next_config = homesense_config::Configuration::parse(next.devices, ExecutionContext::Simulation).map_err(error)?;
        if next_config.revision() <= config.revision() || next_config.gateways().is_empty() { return Err("newer nonempty simulation configuration required".into()); }
        let rules = crate::configuration_publish::validate_automation(&next_config, next.rules, ExecutionContext::Simulation).map_err(error)?;
        if let Some(id) = next.scene && !rules.timed_scenes.get(id).is_some_and(|s| s.enabled) { return Err("enabled replacement scene required".into()); }
        Some(next_config)
    } else {None};
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
    let identity=if hap.is_some(){Some(IdentityStore::open(directory.join("identity")).await.map_err(error)?)}else{None};
    let paired=identity.clone();
    let controller = if web.is_some() { web_controller(directory)? } else { Uuid::new_v4() };
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller || paired.as_ref().is_some_and(|identity|identity.controller_is_paired(id).unwrap_or(false)),
    )
    .await?;
    let daily_backup=crate::daily_backup::Monitor::default();
    let result = async {
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
        // A startup rule can need feedback immediately (for example a fade).
        // Gather simulator observations before opening admission or firing it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            for gateway in owner.gateway_ids() {
                owner.receive(&gateway).await?;
            }
            if owner.snapshot().devices.values().all(|state| {
                state.kind == homesense_core::ModuleKind::Keypad
                    || state.quality == homesense_state::Quality::Confirmed
            }) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("simulation initial feedback timed out".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        admission.synchronized(1, config.revision()).await
            .map_err(|e| format!("{e:?}"))?;
        if let Some(scene) = scene {
            runtime.start_scene(scene).await?;
        }
        if let (Some(hap),Some(mut identity))=(hap,identity) {
            let _settings=if identity.has_config().await.map_err(error)? {
                let settings=identity.load_config().await.map_err(error)?;
                if settings.host!=hap.address.ip() || settings.port!=hap.address.port() {return Err("stored HAP endpoint differs".into());}settings
            } else {
                let settings=hap::Config{host:hap.address.ip(),port:hap.address.port(),name:"HomeSense Unified Simulation".into(),category:hap::accessory::AccessoryCategory::Bridge,pin:random_pin(),..Default::default()};
                identity.save_config(&settings).await.map_err(error)?;settings
            };
            let endpoint=crate::hap_endpoint::Endpoint::start(
                &crate::hap_endpoint::Settings{address:hap.address,scenes:hap.scenes,motors:vec![]},
                &identity,&admission,controller,&runtime,owner.snapshot(),web.is_some(),
            ).await?;
            #[cfg(test)]
            if let (Some(delay),Some(endpoint))=(hap_failure_after,&endpoint) {
                let handle=endpoint.abort_handle();
                tokio::spawn(async move {tokio::time::sleep(delay).await;handle.abort();});
            }
            let snapshots=endpoint.as_ref().map(|endpoint|endpoint.projection.clone()).unwrap_or_else(||tokio::sync::watch::channel(owner.snapshot()).0);
            let mut endpoint=endpoint;
            let mut management=match web {
                Some(settings)=>Some(crate::management_endpoint::Endpoint::start_in_context(settings,directory,&admission,controller,&runtime,None,ExecutionContext::Simulation,daily_backup.clone()).await?),
                None=>None,
            };
            if let Some(management)=&management {management.set_hap_status(if endpoint.is_some(){crate::web_api::HapStatus::Listening}else{crate::web_api::HapStatus::Degraded})?;}
            let (stop,receive)=tokio::sync::watch::channel(false);
            let drive=runtime.drive_hap(&mut owner,snapshots,receive,wall_ms);
            tokio::pin!(drive);
            let deadline=tokio::time::Instant::now()+duration;
            loop {
                tokio::select! {
                    result=async {match management.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>{let _=stop.send(true);drive.await?;return Err(result.err().unwrap_or_else(||"management listener ended".into()));},
                    result=async {match endpoint.as_mut(){Some(endpoint)=>endpoint.finished().await,None=>std::future::pending().await}}=>{
                        log::error!("HAP simulation channel degraded: {}",result.err().unwrap_or_else(||"HAP listener ended".into()));
                        if let Some(endpoint)=endpoint.take(){endpoint.shutdown().await;}
                        if let Some(management)=&management {management.set_hap_status(crate::web_api::HapStatus::Degraded)?;}
                    },
                    result=&mut drive=>{result?;break;},
                    _=tokio::time::sleep_until(deadline)=>{let _=stop.send(true);drive.await?;break;}
                }
            }
            if let Some(endpoint)=endpoint.take(){endpoint.shutdown().await;}
            return Ok(());
        }
        if let Some(web)=web {
            let (publisher,publications)=crate::automation_publication::channel();
            let selected:std::collections::BTreeMap<String,String>=document.timed_scenes.iter().filter(|(_,s)|s.enabled && s.review_evidence.as_ref().is_some_and(|v|!v.trim().is_empty())).map(|(id,_)|(id.clone(),id.clone())).collect();
            let tokens=std::sync::Arc::new(crate::web_auth::Tokens::open_bootstrapped(&directory.join("web-credentials.sqlite"),&web.secret,crate::web_auth::Grant{controller,role:if web.administrator {crate::web_auth::Role::Administrator}else{crate::web_auth::Role::Member},scenes:selected.keys().cloned().collect(),devices:config.devices().iter().filter(|(_,d)|d.enabled).map(|(id,_)|id.as_str().to_owned()).collect()})?);
            let api=std::sync::Arc::new(crate::web_api::Api{daily_backup:daily_backup.clone(),hap_bridges:Default::default(),hap_status:std::sync::RwLock::new(crate::web_api::HapStatus::Disabled),backup_source:Some(directory.join("automation-simulation.sqlite")),publisher:Some(publisher),origin:web.tls.as_ref().map_or_else(||format!("http://{}",web.address),|tls|tls.origin.clone()),tokens,runtime:std::sync::RwLock::new(Some(crate::web_api::Runtime{control:runtime.control(),revision:document.revision,boot:admission.boot_id().to_owned(),scenes:selected})),drafts:Some((ExecutionContext::Simulation,std::sync::Mutex::new(crate::web_drafts::Drafts::open(&directory.join("web-drafts.sqlite"))?))),receipts:std::sync::Mutex::new(crate::web_receipts::Receipts::open(&directory.join("web-receipts.sqlite"))?)});
            let (stop,receive)=tokio::sync::watch::channel(false);
            let drive=crate::web_supervision::run(api.clone(),admission.clone(),controller,runtime,owner,publications,receive.clone(),directory.join("automation-simulation.sqlite"));
            let server=crate::web_server::serve_with_tls(api,web.address,receive,web.tls);
            tokio::pin!(drive,server);
            tokio::select! {
                result=&mut server=>{let _=stop.send(true);drive.await?;result?;return Err("Web listener ended early".into());},
                result=&mut drive=>{let _=stop.send(true);server.await?;result?;return Err("automation ended early".into());},
                _=tokio::time::sleep(duration)=>{let _=stop.send(true);let (a,b)=tokio::join!(drive,server);a?;b?;}
            }
            return Ok(());
        }
        {
            let (stop, receive) = tokio::sync::watch::channel(false);
            let handoff_gate=runtime.handoff_gate();
            let simulation_deadline=tokio::time::Instant::now()+duration;
            let drive = runtime.drive(&mut owner, Duration::from_millis(10), 5000, receive, wall_ms);
            tokio::pin!(drive);
            let first_duration=publication.as_ref().map_or(duration, |p|p.after);
            tokio::select! {
                result=&mut drive=>{result?;return Err("simulation loop ended before deadline".into())},
                _=tokio::time::sleep(first_duration)=>{
                    if let Some(next)=&publication {
                        let quantum=tokio::select! {
                            permit=handoff_gate.clone().lock_owned()=>permit,
                            result=&mut drive=>{result?;return Err("simulation ended before publication barrier".into());},
                            _=tokio::time::sleep_until(simulation_deadline)=>{
                                let _=stop.send(true);drive.await?;return Ok(());
                            },
                        };
                        match crate::automation_tcp::begin_handoff_before_deadline(&admission,next.devices,next.rules,config.revision(),simulation_deadline).await {
                            Ok(())=>{},
                            Err(crate::automation_tcp::HandoffPublicationFailure::Deadline)=>{
                                let _=stop.send(true);drop(quantum);drive.await?;return Ok(());
                            },
                            Err(crate::automation_tcp::HandoffPublicationFailure::Admission(error))=>return Err(format!("{error:?}")),
                        }
                        let _=stop.send(true);
                        drop(quantum);
                    } else {let _=stop.send(true);}
                    drive.await?;
                }
            }
        }
        if let (Some(next),Some(next_config))=(publication,next_config) {
            if !runtime.publication_paused() {return Err("old runtime did not acknowledge publication".into());}
            drop(runtime);
            while owner.drain_publication(&admission).await?.is_some() {}
            admission.commit_publication().await.map_err(|e|format!("{e:?}"))?;
            drop(owner);
            let fresh=TransportOwner::new(admission.clone(),simulation_connections_at(&next_config,2)?);
            let (mut runtime,mut owner)=crate::publication_recovery::rebuild(admission.clone(),controller,fresh,2,crate::publication_recovery::Settings{
                query_timeout_ms:1000,max_age_ms:5000,wall_unix_ms:wall_ms()?,calendar_lateness_ms:1000,scene_capacity:64,
            }).await?;
            if let Some(scene)=next.scene {runtime.start_scene(scene).await?;}
            let (stop,receive)=tokio::sync::watch::channel(false);
            let drive=runtime.drive(&mut owner,Duration::from_millis(10),5000,receive,wall_ms);
            tokio::pin!(drive);
            tokio::select! {
                result=&mut drive=>result?,
                _=tokio::time::sleep(duration-next.after)=>{let _=stop.send(true);drive.await?;}
            }
        }
        Ok(())
    };
    let result=crate::daily_backup::supervise_monitored(directory.join("automation-simulation.sqlite"),ExecutionContext::Simulation,result,daily_backup.clone()).await;
    admission.stop_now();
    let _ = admission.shutdown().await;
    result
}
pub(crate) fn wall_ms() -> Result<i64, String> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?
            .as_millis(),
    )
    .map_err(error)
}

pub(crate) fn web_controller(directory:&Path)->Result<Uuid,String> {
    use std::io::Write;
    let path=directory.join("web-controller-id");
    if !path.exists() && directory.join("web-receipts.sqlite").exists() {return Err("Web identity missing beside existing receipts; refusing identity replacement".into());}
    let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    match options.open(&path) {
        Ok(mut file)=>{let id=Uuid::new_v4();file.write_all(id.to_string().as_bytes()).map_err(error)?;file.sync_all().map_err(error)?;Ok(id)},
        Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{
            let bytes=std::fs::read(&path).map_err(error)?;
            if bytes.len()!=36 {return Err("invalid persistent Web controller".into());}
            Uuid::parse_str(std::str::from_utf8(&bytes).map_err(error)?).map_err(error)
        },
        Err(e)=>Err(error(e))
    }
}

#[cfg(test)]
#[path = "simulation_hap_fault_tests.rs"]
mod hap_fault_tests;
