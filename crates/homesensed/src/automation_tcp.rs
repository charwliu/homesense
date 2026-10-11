//! Explicit bounded automation over TCP. No HAP identity or field configuration is changed.
use crate::{
    admission::{Admission, Options},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    reconnect::{RetryPolicy, reconnect_tcp},
};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

struct Stop(Admission);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}
fn wall_ms() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|e| e.to_string())
}
/// This entry point executes enabled, reviewed rules, including startup rules.
/// The caller explicitly confirms concurrent gateway clients are supported.
/// Loss of transport ends this run; a new invocation never replays old commands.
pub async fn serve(
    devices: &[u8],
    rules: &[u8],
    directory: &Path,
    duration: Duration,
    coexistence_confirmed: bool,
) -> Result<(), String> {
    serve_supervised(
        devices,
        rules,
        directory,
        duration,
        coexistence_confirmed,
        0,
    )
    .await
}
/// Opt-in bounded recovery after an explicitly classified, fully cleaned disconnect.
/// The duration is shared by all connections after initial synchronization.
pub async fn serve_supervised(
    devices: &[u8],
    rules: &[u8],
    directory: &Path,
    duration: Duration,
    coexistence_confirmed: bool,
    recovery_limit: u8,
) -> Result<(), String> {
    if recovery_limit > 16 {
        return Err("recovery limit must be 0..16".into());
    }
    if !coexistence_confirmed || duration.is_zero() || duration > Duration::from_secs(86400) {
        return Err("automation TCP requires confirmed coexistence and duration in (0,24h]".into());
    }
    let config = validate_candidate(devices, rules)?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join("automation-tcp.sqlite");
    let mut store = Store::open(&path).map_err(|e| e.to_string())?;
    if let Some(active) = store.active().map_err(|e| e.to_string())? {
        if active.payload != devices
            || store
                .automation_payload(active.revision)
                .map_err(|e| e.to_string())?
                .as_deref()
                != Some(rules)
        {
            return Err("stored automation differs; explicit publication required".into());
        }
    } else {
        store
            .publish_automation(None, devices, rules, ExecutionContext::Live, &[])
            .map_err(|e| e.to_string())?;
    }
    drop(store);
    let controller = Uuid::new_v4();
    let admission = Admission::open(path, Options::default(), move |id| id == controller).await?;
    let _guard = Stop(admission.clone());
    let result = async {
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let endpoints: std::collections::BTreeMap<_,_> = config
            .gateways()
            .iter()
            .map(|(id, g)| (id.clone(), g.endpoint))
            .collect();
        let (transport, mut epoch) = reconnect_tcp(
            admission.clone(),
            endpoints.clone(),
            1,
            RetryPolicy::default(),
            true,
            receiver.clone(),
        )
        .await?;
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Live,
            Uuid::parse_str(admission.boot_id())
                .map_err(|e| e.to_string())?
                .as_u128(),
            epoch,
        )?;
        let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
        owner
            .query_automation_recovery(&config, ExecutionContext::Live, 1000, 5000)
            .await?;
        let deadline=tokio::time::Instant::now()+duration;
        let mut recoveries=0;
        loop {
            use crate::{automation_drive::Exit,automation_runtime::StartupPolicy};
            let startup=if recoveries==0 {StartupPolicy::Run} else {StartupPolicy::Suppress};
            let mut runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,wall_ms()?,1000,64,Default::default(),startup).await?;
            let outcome={
                let drive=runtime.drive_recoverable(&mut owner,Duration::from_millis(10),5000,receiver.clone(),wall_ms);
                tokio::pin!(drive);
                tokio::select! {
                    result=&mut drive=>result?,
                    _=tokio::time::sleep_until(deadline)=>{let _=stop.send(true);drive.await?},
                }
            };
            drop(runtime);
            match outcome {
                Exit::Stopped => return Ok(()),
                Exit::PublicationPaused => return Err("TCP publication coordinator not installed".into()),
                Exit::Disconnected {gateway} => {
                    if recoveries>=recovery_limit {return Err(format!("gateway disconnected: {gateway}; recovery limit reached"));}
                    recoveries+=1;
                    log::info!("automation recovery={recoveries} gateway={gateway:?} previous_epoch={epoch}");
                    let recovery=async {
                        let (transport,next)=reconnect_tcp(admission.clone(),endpoints.clone(),epoch,RetryPolicy::default(),true,receiver.clone()).await?;
                        epoch=next;
                        owner.recover(transport,epoch)?;
                        owner.query_automation_recovery(&config,ExecutionContext::Live,1000,5000).await
                    };
                    tokio::select! {
                        result=recovery=>result?,
                        _=tokio::time::sleep_until(deadline)=>return Ok(()),
                    }
                }
            }
        }
    }
    .await;
    admission.stop_now();
    let shutdown = admission
        .shutdown()
        .await
        .map_err(|e| format!("automation shutdown: {e:?}"));
    result.and(shutdown)
}

/// Validate both documents and the TCP synchronization policy without opening
/// storage or connections. Publication coordinators must call this before
/// cancelling the old runtime; semantic validity alone does not prove that the
/// new mapping can pass the feedback recovery gate.
pub fn validate_candidate(
    devices: &[u8],
    rules: &[u8],
) -> Result<homesense_config::Configuration, String> {
    let config = homesense_config::Configuration::parse(devices, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    crate::configuration_publish::validate_automation(&config, rules, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    if config.gateways().is_empty()
        || !config
            .devices()
            .keys()
            .any(|id| config.binding(id).is_some())
    {
        return Err("enabled verified devices required".into());
    }
    // Reject unsupported synchronization before entering a publication barrier.
    let mut outputs = 0;
    for id in config.devices().keys() {
        if let Some((device, module)) = config.binding(id) {
            if module.kind() == homesense_core::ModuleKind::Keypad {
                module.validate_context(ExecutionContext::Live).map_err(|e|e.to_string())?;
                module.validate_channel(device.channel).map_err(|e|e.to_string())?;
                continue;
            }
            outputs += 1;
            hb_protocol::encode_in_context(
                module,
                device.channel,
                homesense_core::Command::Query,
                ExecutionContext::Live,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    if outputs == 0 {
        return Err("automation TCP requires at least one queryable output".into());
    }
    Ok(config)
}
