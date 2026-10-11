//! Explicit offline configuration publication. Never opens a hardware connection
//! or edits the separate HAP identity database. Back up before invoking.
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::path::Path;

pub fn publish(document: &[u8], directory: &Path, expected: u64) -> Result<u64, String> {
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    if config.revision() <= expected {
        return Err("new revision must increase".into());
    }
    let path = directory
        .join("runtime.sqlite")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("runtime.lock"))
        .map_err(|e| e.to_string())?;
    lease
        .try_lock()
        .map_err(|e| format!("runtime must be stopped: {e}"))?;
    let mut store = Store::open(&path).map_err(|e| e.to_string())?;
    let old = store
        .active()
        .map_err(|e| e.to_string())?
        .ok_or("active configuration required")?;
    if old.revision != expected {
        return Err("active revision changed".into());
    }
    // The stopped process has no authoritative in-memory queue. Preserve all
    // incomplete old commands as uncertain; none will be replayed on restart.
    store
        .recover_commands(&format!("publication-{}", uuid::Uuid::new_v4()))
        .map_err(|e| e.to_string())?;
    store
        .publish_configuration(Some(expected), document, ExecutionContext::Live, &[])
        .map_err(|e| e.to_string())?;
    Ok(config.revision())
}
