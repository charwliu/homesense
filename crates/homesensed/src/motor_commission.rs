//! Bounded motor commissioning through durable admission and the transport owner.
use crate::{
    admission::{Admission, Options},
    transport_owner::TransportOwner,
};
use homesense_core::{DeviceId, ExecutionContext, MotorAction, ProtocolProfile};
use homesense_storage::Store;
use std::{
    io::Read,
    net::Shutdown,
    path::Path,
    time::{Duration, Instant},
};

struct StopOnDrop(Admission);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}

/// Writes one motion followed by STOP after three seconds. Physical results require observation.
pub async fn run(
    document: &[u8],
    directory: &Path,
    device: &DeviceId,
    action: MotorAction,
    coexistence_confirmed: bool,
) -> Result<hap::serde_json::Value, String> {
    if !matches!(action, MotorAction::Open | MotorAction::Close) {
        return Err("motion must be open or close".into());
    }
    if !coexistence_confirmed {
        return Err("coexistence confirmation required".into());
    }
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    if config.devices().len() != 1 || config.gateways().len() != 1 {
        return Err("motor commissioning requires a single motor and gateway".into());
    }
    let (binding, module) = config.binding(device).ok_or("enabled motor required")?;
    if module.profile() != ProtocolProfile::C4DinPro {
        return Err("C4 Pro required".into());
    }
    let gateway = config
        .gateways()
        .get(&binding.gateway)
        .ok_or("gateway missing")?;
    if gateway.modules().len() != 1 {
        return Err("single-module commissioning required".into());
    }
    let endpoint = gateway.endpoint;
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
            let stream = std::net::TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).map_err(|e| e.to_string())?;
            stream.set_read_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
            stream.set_write_timeout(Some(Duration::from_millis(100))).map_err(|e| e.to_string())?;
            stream.set_nodelay(true).map_err(|e| e.to_string())?;
            Ok(stream)
        }).await.map_err(|e| e.to_string())??;
        let mut closing_stream = stream.try_clone().map_err(|e| e.to_string())?;
        // Explicit commissioning only: readiness does not claim position feedback.
        admission.synchronized(1, config.revision()).await.map_err(|e| format!("motor readiness: {e:?}"))?;
        let mut owner = TransportOwner::new(admission.clone(), [(binding.gateway.clone(), hb_transport::Connection::new(stream, 1))].into());
        // Do not write stdout while the motor is moving: a blocked pipe or
        // broken stdout must not delay or panic before the scheduled STOP.
        let mut reports = Vec::new();
        for next in [action, MotorAction::Stop] {
            let ids = admission.submit_motor(principal, device.clone(), next).await.map_err(|e| format!("motor admission: {e:?}; use original controller if moving"))?;
            let outcome = owner.step().await?.ok_or("motor command not dispatched")?;
            if ids.as_slice() != [outcome.command_id] { return Err("unexpected motor dispatch".into()); }
            outcome.result.map_err(|e| format!("motor write uncertain: {e:?}; use original controller to stop; not replayed"))?;
            let report = hap::serde_json::json!({"action":format!("{next:?}"),"command_id":outcome.command_id,"physical_result_confirmed":false});
            reports.push(report);
            if next != MotorAction::Stop { tokio::time::sleep(Duration::from_secs(3)).await; }
        }
        // Half-close only after STOP; drain unsolicited feedback so unread bytes
        // do not cause an immediate local RST. This is not an actuator ACK.
        let drained = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            closing_stream.shutdown(Shutdown::Write).map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_millis(500);
            let mut total = 0;
            let mut buffer = [0; 1024];
            while Instant::now() < deadline && total < 65536 {
                match closing_stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(total)
        }).await.map_err(|e| e.to_string())??;
        Ok(hap::serde_json::json!({"boot_id":admission.boot_id(),"commands":reports,"teardown_drained_bytes":drained,"physical_result_confirmed":false}))
    }.await;
    let shutdown = admission
        .clone()
        .shutdown()
        .await
        .map_err(|e| format!("motor shutdown: {e:?}"));
    match (result, shutdown) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}
