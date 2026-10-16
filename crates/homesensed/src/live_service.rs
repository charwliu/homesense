//! Explicit, bounded live HAP service. Enabled devices must be verified lights.
use crate::{
    admission::{Admission, Options},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    reconnect::{RetryPolicy, reconnect_tcp},
    runtime_hap::{RuntimeHap, SupervisorOptions},
};
use hap::{
    server::{IpServer, Server},
    storage::Storage,
};
use homesense_core::{Command, ExecutionContext};
use homesense_hap::storage::IdentityStore;
use homesense_storage::Store;
use std::{net::SocketAddr, path::Path, time::Duration};
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
struct StopOnDrop(Admission);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}

pub async fn serve_live(
    document: &[u8],
    directory: &Path,
    address: SocketAddr,
    duration: Duration,
    coexistence_confirmed: bool,
) -> Result<(), String> {
    let allowed = address.ip().is_loopback()
        || match address.ip() {
            std::net::IpAddr::V4(ip) => ip.is_private(),
            std::net::IpAddr::V6(ip) => ip.is_unique_local(),
        };
    if !coexistence_confirmed
        || !allowed
        || address.port() == 0
        || duration.is_zero()
        || duration > Duration::from_secs(86400)
    {
        return Err("live service requires confirmed coexistence, a private/loopback endpoint and duration in (0,24h]".into());
    }
    let config =
        homesense_config::Configuration::parse(document, ExecutionContext::Live).map_err(error)?;
    let mut enabled = 0;
    for id in config.devices().keys() {
        if let Some((device, module)) = config.binding(id) {
            hb_protocol::encode_in_context(
                module,
                device.channel,
                Command::Query,
                ExecutionContext::Live,
            )
            .map_err(error)?;
            enabled += 1;
        }
    }
    if enabled == 0 {
        return Err("live service requires enabled verified lights".into());
    }
    std::fs::create_dir_all(directory).map_err(error)?;
    let database = directory.join("runtime.sqlite");
    let mut store = Store::open(&database).map_err(error)?;
    match store.active().map_err(error)? {
        Some(active) if active.payload != document => {
            return Err("stored configuration differs; explicit publication required".into());
        }
        Some(_) => {}
        None => {
            store
                .publish_configuration(None, document, ExecutionContext::Live, &[])
                .map_err(error)?;
        }
    }
    drop(store);
    let mut identity = IdentityStore::open(directory.join("identity"))
        .await
        .map_err(error)?;
    let authorize = identity.clone();
    let admission = Admission::open(database, Options::default(), move |id| {
        authorize.controller_is_paired(id).unwrap_or(false)
    })
    .await?;
    let _cancel = StopOnDrop(admission.clone());
    let result = async {
        let settings = if identity.has_config().await.map_err(error)? {
            let saved = identity.load_config().await.map_err(error)?;
            if saved.host != address.ip() || saved.port != address.port() { return Err("stored HAP endpoint differs".into()); }
            saved
        } else {
            let settings = hap::Config { host: address.ip(), port: address.port(), name: "HomeSense".into(),
                category: hap::accessory::AccessoryCategory::Bridge, pin: crate::service::random_pin(), ..Default::default() };
            identity.save_config(&settings).await.map_err(error)?;
            settings
        };
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let retry = RetryPolicy::default();
        let endpoints = config.gateways().iter().map(|(id, gateway)| (id.clone(), gateway.endpoint)).collect();
        let (transport, epoch) = reconnect_tcp(admission.clone(), endpoints, 1, retry, coexistence_confirmed, receiver.clone()).await?;
        let feedback = Feedback::new(&config, ExecutionContext::Live,
            uuid::Uuid::parse_str(admission.boot_id()).map_err(error)?.as_u128(), epoch)?;
        let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
        let mut graph = RuntimeHap::new(&config, owner.snapshot(), &identity, admission.clone(), config.refresh_policy().observation_max_age_ms()).await?;
        owner.query_recovery(&config, ExecutionContext::Live, 1000, config.refresh_policy().observation_max_age_ms()).await?;
        log::info!("live_initial_synchronized epoch={epoch} revision={} devices={enabled}", config.revision());
        graph.publish(&owner).await?;
        let server = if address.ip().is_loopback() { IpServer::new_loopback(settings, identity.clone()).await }
            else { IpServer::new(settings, identity.clone()).await }.map_err(error)?;
        graph.install(&server).await?;
        let drive = graph.drive_supervised(&mut owner, &config, SupervisorOptions {
            initial_epoch: epoch, retry, cadence: Duration::from_millis(10), query_timeout_ms: 1000,
            recovery_limit: 16, coexistence_confirmed,
        }, receiver);
        tokio::pin!(drive);
        tokio::select! {
            result = server.run_handle() => { let _ = stop.send(true); drive.await?; result.map_err(error) },
            result = &mut drive => result,
            _ = tokio::time::sleep(duration) => { let _ = stop.send(true); drive.await },
        }
    }.await;
    admission.stop_now();
    let _ = admission.shutdown().await;
    result
}
