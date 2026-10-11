//! Bounded manual commissioning of the three reviewed study scenes only.
use homesense_core::{DeviceId, ExecutionContext};
use homesense_storage::Store;
use crate::{
    admission::{Admission, Options},
    parameter_probe::{Parameter, Probe, Reply, Value},
    study_runtime::{StudyRuntime, TimingPolicy},
    transport_owner::TransportOwner,
};
use std::{io::Read, path::PathBuf, time::Duration};
struct Stop(Admission);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.stop_now();
    }
}
pub async fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || args[3] != "--coexistence-confirmed" {
        return Err("usage: study-scene STATE_DIR KEY(1|3|5) --coexistence-confirmed".into());
    }
    let key: u8 = args[2].parse().map_err(|_| "invalid key")?;
    if ![1, 3, 5].contains(&key) {
        return Err("unsupported study scene".into());
    }
    let document = include_bytes!("../../../config/scenes/study-commission.json");
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    let panel = DeviceId::new("control4:63:key:1").map_err(|e| e.to_string())?;
    let (_, module) = config.binding(&panel).ok_or("panel missing")?;
    let probe = Probe::new(module.address(), module.profile(), Parameter::LedSelfReset)
        .map_err(str::to_owned)?;
    let endpoint = config.gateways()["hopebeauty"].endpoint;
    let stream = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let mut s = std::net::TcpStream::connect_timeout(&endpoint, Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        s.set_nodelay(true).map_err(|e| e.to_string())?;
        let (raw, reply) = probe
            .run(&mut s, Duration::from_millis(250))
            .map_err(|e| e.to_string())?;
        if reply != Reply::Candidate(Value::SelfReset(false)) {
            return Err(format!(
                "manual LED mode unconfirmed: {reply:?} raw={raw:02X?}; no scene writes"
            ));
        }
        Ok(s)
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut drain = stream.try_clone().map_err(|e| e.to_string())?;
    let directory = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let database = directory.join("runtime.sqlite");
    let mut store = Store::open(&database).map_err(|e| e.to_string())?;
    match store.active().map_err(|e| e.to_string())? {
        Some(a) if a.payload != document => {
            return Err("state configuration differs; use dedicated commissioning directory".into());
        }
        Some(_) => (),
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
    let _stop = Stop(admission.clone());
    admission
        .synchronized(1, config.revision())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut scene = StudyRuntime::new(
        admission.clone(),
        principal,
        include_bytes!("../../../config/scenes/study.json"),
        TimingPolicy::ModuleNative,
    )?;
    let run = scene.activate(key).await?;
    let mut owner = TransportOwner::new(
        admission.clone(),
        [(
            "hopebeauty".into(),
            hb_transport::Connection::new(stream, 1),
        )]
        .into(),
    );
    let mut written = Vec::new();
    for id in &run.command_ids {
        let outcome = owner.step().await?.ok_or("missing scene command")?;
        if outcome.command_id != *id {
            return Err("unexpected dispatch; stopped".into());
        }
        outcome
            .result
            .map_err(|e| format!("uncertain scene write; no replay: {e:?}"))?;
        written.push(*id);
    }
    drain
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let until = std::time::Instant::now() + Duration::from_millis(500);
        let mut bytes = 0;
        let mut buffer = [0; 1024];
        while std::time::Instant::now() < until && bytes < 65536 {
            match drain.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    ()
                }
                Err(_) => break,
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    admission
        .shutdown()
        .await
        .map_err(|e| format!("shutdown: {e:?}"))?;
    println!(
        "{}",
        hap::serde_json::json!({"key":key,"mode":run.mode,"written":written,"physical_confirmation":false,"automatic_execution":false,"ea5_modified":false})
    );
    Ok(())
}
