//! Explicit offline configuration publication. Never opens a hardware connection
//! or edits the separate HAP identity database. Back up before invoking.
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use homesense_storage::Store;
use std::error::Error;
use std::path::Path;

pub fn publish(document: &[u8], directory: &Path, expected: u64) -> Result<u64, String> {
    publish_inner(document, None, directory, expected)
}
pub fn publish_automation(
    document: &[u8],
    rules: &[u8],
    directory: &Path,
    expected: u64,
) -> Result<u64, String> {
    publish_inner(document, Some(rules), directory, expected)
}
fn publish_inner(
    document: &[u8],
    rules: Option<&[u8]>,
    directory: &Path,
    expected: u64,
) -> Result<u64, String> {
    let config = homesense_config::Configuration::parse(document, ExecutionContext::Live)
        .map_err(|e| e.to_string())?;
    if config.revision() <= expected {
        return Err("new revision must increase".into());
    }
    if let Some(rules) = rules {
        validate_automation(&config, rules, ExecutionContext::Live).map_err(|e| e.to_string())?;
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
    if let Some(rules) = rules {
        store
            .publish_automation(Some(expected), document, rules, ExecutionContext::Live, &[])
            .map_err(|e| e.to_string())?;
    } else {
        store
            .publish_configuration(Some(expected), document, ExecutionContext::Live, &[])
            .map_err(|e| e.to_string())?;
    }
    Ok(config.revision())
}

/// Shared offline validation for checking and publishing automation documents.
pub fn validate_automation(
    config: &Configuration,
    bytes: &[u8],
    context: ExecutionContext,
) -> Result<homesense_config::rule_document::Document, Box<dyn Error>> {
    let document = homesense_config::rule_document::Document::parse(bytes, config)?;
    let linked = document.linked_rules(
        &document.immediate_scenes,
        &document.schedules.iter().map(|s| s.id.clone()).collect(),
    )?;
    fn commands(
        nodes: &[homesense_core::rule::Node],
        config: &Configuration,
        context: ExecutionContext,
    ) -> Result<(), Box<dyn Error>> {
        for node in nodes {
            match &node.action {
                homesense_core::rule::Action::Command { device, command } => {
                    let (mapping, module) = config
                        .binding(device)
                        .ok_or("enabled rule references disabled output")?;
                    hb_protocol::encode_in_context(module, mapping.channel, *command, context)?;
                }
                homesense_core::rule::Action::If {
                    then_actions,
                    else_actions,
                    ..
                } => {
                    commands(then_actions, config, context)?;
                    commands(else_actions, config, context)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    for rule in &linked {
        if let homesense_config::rule_document::Trigger::Panel { device, event } = &rule.trigger {
            let id = DeviceId::new(device)?;
            if config.binding(&id).is_none() {
                return Err("enabled panel rule references disabled key".into());
            }
            let settings = document
                .gestures
                .get(&id)
                .ok_or("enabled panel rule missing gesture settings")?;
            if matches!(
                event,
                homesense_config::rule_document::PanelEvent::DoubleClick
            ) && !settings.double_click_enabled
            {
                return Err("double-click requires deferred single-click mode".into());
            }
        }
        commands(&rule.actions, config, context)?;
    }
    for scene in document.timed_scenes.values().filter(|s| s.enabled) {
        for step in &scene.steps {
            let (mapping, module) = config
                .binding(&step.device)
                .ok_or("enabled timed scene references disabled output")?;
            hb_protocol::encode_in_context(module, mapping.channel, step.command, context)?;
        }
    }
    Ok(document)
}
