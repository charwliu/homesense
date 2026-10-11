//! A single explicit commissioning action. No button event or scene is emitted.
use crate::{
    admission::{Admission, Options},
    parameter_probe::{Parameter, Probe, Reply, Value},
    transport_owner::TransportOwner,
};
use homesense_core::{Command, DeviceId, ExecutionContext, ProtocolProfile};
use homesense_storage::Store;
use std::{path::Path, time::Duration};

struct StopOnDrop(Admission);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}

/// Requires exactly one enabled, verified TS6 key in explicit manual LED mode.
/// The fresh connection's mode query must contain exactly the documented byte.
/// Written is only a transport result; the operator verifies the physical color.
pub async fn set(
    document: &[u8],
    directory: &Path,
    device: &DeviceId,
    red: bool,
    coexistence_confirmed: bool,
) -> Result<hap::serde_json::Value, String> {
    if !coexistence_confirmed {
        return Err("coexistence confirmation required".into());
    }
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    if config.devices().len() != 1 || config.gateways().len() != 1 {
        return Err("LED commissioning requires a single key and gateway".into());
    }
    let (binding, module) = config.binding(device).ok_or("enabled key required")?;
    if module.profile() != ProtocolProfile::Ts686Rs485SixKey {
        return Err("LED commissioning supports confirmed six-key TS6 only".into());
    }
    hb_protocol::encode_in_context(
        module,
        binding.channel,
        Command::PanelLed(red),
        ExecutionContext::Live,
    )
    .map_err(|e| e.to_string())?;
    let gateway = config
        .gateways()
        .get(&binding.gateway)
        .ok_or("gateway missing")?;
    if gateway.modules().len() != 1 {
        return Err("single-module commissioning required".into());
    }
    let endpoint = gateway.endpoint;
    let probe = Probe::new(module.address(), module.profile(), Parameter::LedSelfReset)
        .map_err(str::to_owned)?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let database = directory.join("runtime.sqlite");
    let mut store = Store::open(&database).map_err(|e| e.to_string())?;
    match store.active().map_err(|e| e.to_string())? {
        Some(active) if active.payload != document => {
            return Err("stored commissioning configuration differs".into());
        }
        Some(_) => {}
        None => {
            store
                .publish_configuration(None, document, ExecutionContext::Live, &[])
                .map_err(|e| e.to_string())?;
        }
    }
    drop(store);
    let principal = uuid::Uuid::new_v4();
    let admission =
        Admission::open(database, Options::default(), move |id| id == principal).await?;
    let _stop = StopOnDrop(admission.clone());
    let result = async {
        let stream = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let mut stream = std::net::TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).map_err(|e| e.to_string())?;
            stream.set_read_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
            stream.set_write_timeout(Some(Duration::from_millis(100))).map_err(|e| e.to_string())?;
            stream.set_nodelay(true).map_err(|e| e.to_string())?;
            let (raw, reply) = probe.run(&mut stream, Duration::from_millis(250)).map_err(|e| e.to_string())?;
            if reply != Reply::Candidate(Value::SelfReset(false)) {
                return Err(format!("manual LED mode not corroborated: {reply:?}; raw={raw:02X?}; no LED write"));
            }
            Ok(stream)
        }).await.map_err(|e| e.to_string())??;
        // This gate applies only to the one LED key validated above. It does not
        // synchronize a general device graph or assert the current LED color.
        admission.synchronized(1, config.revision()).await.map_err(|e| format!("LED readiness: {e:?}"))?;
        let mut owner = TransportOwner::new(admission.clone(), [(binding.gateway.clone(), hb_transport::Connection::new(stream, 1))].into());
        let ids = admission.submit_panel_led(principal, device.clone(), red).await.map_err(|e| format!("LED admission: {e:?}"))?;
        let outcome = owner.step().await?.ok_or("LED command was not dispatched")?;
        if ids.as_slice() != [outcome.command_id] { return Err("unexpected LED dispatch".into()); }
        outcome.result.map_err(|e| format!("LED write uncertain: {e:?}; not replayed"))?;
        Ok(hap::serde_json::json!({"event":"panel_led_written", "device":device.as_str(), "red":red,
            "command_id":outcome.command_id, "boot_id":admission.boot_id(), "mode_query_raw":[2],
            "mode_reply_is_candidate":true,"physical_color_confirmed":false,"button_event_sent":false}))
    }.await;
    let shutdown = admission
        .clone()
        .shutdown()
        .await
        .map_err(|e| format!("LED shutdown: {e:?}"));
    match (result, shutdown) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}
