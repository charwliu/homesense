//! Bounded administrator inventory of physical slots. Never performs a query.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
};
use homesense_core::{DeviceId, ModuleKind, Observation, Verification};
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub name: String,
    pub room: Option<String>,
    pub enabled: bool,
    pub supply_for: Vec<String>,
    pub motor_calibrated: bool,
    pub actual: Option<serde_json::Value>,
    pub quality: &'static str,
    pub observed_ms: Option<String>,
    pub age_ms: Option<String>,
    pub actions: Vec<&'static str>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Slot {
    pub channel: u8,
    pub binding: Option<Binding>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Module {
    pub module_id: Option<String>,
    pub gateway: String,
    pub address: String,
    pub profile: &'static str,
    pub kind: String,
    pub verification: &'static str,
    pub enabled: bool,
    pub channels: u8,
    pub response: &'static str,
    pub queryable_bindings: usize,
    pub fresh_responses: usize,
    pub slots: Vec<Slot>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CommandRecord {
    pub command: String,
    pub device: String,
    pub status: String,
    pub requested: serde_json::Value,
    pub transport_epoch: String,
    pub control_epoch: String,
    pub updated_ms: String,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Page {
    pub boot: String,
    pub revision: String,
    pub transport_epoch: String,
    pub control_epoch: String,
    pub observed_transport_epoch: String,
    pub truth_source: &'static str,
    pub captured_ms: String,
    pub ready: bool,
    pub modules: Vec<Module>,
    pub commands: Vec<CommandRecord>,
    pub next_offset: Option<usize>,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn project(
    config: &homesense_config::Configuration,
    raw: &homesense_state::Snapshot,
    now: u64,
    max_age: u64,
    ready: bool,
    offset: usize,
    limit: usize,
) -> Result<Vec<Module>, String> {
    if !(1..=64).contains(&limit) || offset > 16320 || max_age == 0 || max_age > 5000 {
        return Err("invalid module page".into());
    }
    let mut bindings = BTreeMap::new();
    let mut supply_users: BTreeMap<&DeviceId, Vec<String>> = BTreeMap::new();
    for (id, device) in config.devices() {
        bindings.insert(
            (
                device.gateway.as_str(),
                device.address,
                device.channel.get(),
            ),
            (id, device),
        );
        if let Some(relay) = &device.supply_relay {
            supply_users
                .entry(relay)
                .or_default()
                .push(id.as_str().to_owned());
        }
    }
    let mut modules = vec![];
    for (gateway, module) in config
        .gateways()
        .iter()
        .flat_map(|(id, g)| g.modules().values().map(move |m| (id, m)))
        .skip(offset)
        .take(limit)
    {
        let mut slots = vec![];
        let mut enabled = false;
        let mut queryable = 0;
        let mut fresh_count = 0;
        let supported = matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer);
        for channel in 1..=module.channels() {
            let binding = bindings
                .get(&(gateway.as_str(), module.address(), channel))
                .map(|(id, device)| {
                    enabled |= device.enabled;
                    if device.enabled && supported {
                        queryable += 1;
                    }
                    let state = raw.devices.get(id);
                    let at = state.and_then(|s| s.observed_at_ms);
                    let age = at.and_then(|at| now.checked_sub(at));
                    let fresh = ready
                        && device.enabled
                        && supported
                        && state.is_some_and(|s| s.quality == homesense_state::Quality::Confirmed)
                        && age.is_some_and(|age| age < max_age);
                    if fresh {
                        fresh_count += 1;
                    }
                    let actual = state.and_then(|s| s.observed).and_then(|value| {
                        match (module.kind(), value) {
                            (ModuleKind::Switch, Observation::Switch(on)) => {
                                Some(serde_json::json!({"on":on}))
                            }
                            (ModuleKind::Dimmer, Observation::Level(level)) => {
                                Some(serde_json::json!({"level":level.get(),"on":level.get()>0}))
                            }
                            _ => None,
                        }
                    });
                    let supply_for = supply_users.get(id).cloned().unwrap_or_default();
                    let motor_calibrated = config
                        .motor_command(id, homesense_core::MotorAction::Stop)
                        .is_ok();
                    let actions = if device.enabled
                        && supply_for.is_empty()
                        && device.module_id.is_some()
                    {
                        match module.kind() {
                            ModuleKind::Switch => vec!["power"],
                            ModuleKind::Dimmer => vec!["power", "brightness"],
                            ModuleKind::Motor if motor_calibrated => vec!["open", "close", "stop"],
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };
                    Binding {
                        id: id.as_str().to_owned(),
                        name: device.name.clone(),
                        room: device.room.clone(),
                        enabled: device.enabled,
                        supply_for,
                        motor_calibrated,
                        actual,
                        quality: if !device.enabled {
                            "disabled"
                        } else if !supported {
                            "unsupported_feedback"
                        } else if !ready {
                            "unavailable"
                        } else if fresh {
                            "confirmed"
                        } else if at.is_some() {
                            "stale"
                        } else {
                            "unknown"
                        },
                        observed_ms: at.map(|at| at.to_string()),
                        age_ms: age.map(|age| age.to_string()),
                        actions,
                    }
                });
            slots.push(Slot { channel, binding });
        }
        let response = if !supported {
            "unsupported_feedback"
        } else if queryable == 0 {
            "no_queryable_bindings"
        } else if !ready {
            "unavailable"
        } else if fresh_count == queryable {
            "fresh"
        } else if fresh_count > 0 {
            "partial"
        } else {
            "unknown_or_stale"
        };
        modules.push(Module {
            module_id: config
                .module_id(gateway, module.address())
                .map(str::to_owned),
            gateway: gateway.clone(),
            address: module.address().to_string(),
            profile: module.profile().id(),
            kind: format!("{:?}", module.kind()),
            verification: match module.verification() {
                Verification::Verified(_) => "verified",
                Verification::Simulated => "simulated",
                Verification::Pending => "pending",
            },
            enabled,
            channels: module.channels(),
            response,
            queryable_bindings: queryable,
            fresh_responses: fresh_count,
            slots,
        });
    }
    Ok(modules)
}
#[allow(clippy::too_many_arguments)]
pub async fn snapshot(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    feedback: &Feedback,
    max_age: u64,
    offset: usize,
    limit: usize,
) -> Result<Page, String> {
    let bundle = admission
        .automation_bundle(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let diagnostics = admission
        .diagnostics(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut raw = feedback.snapshot();
    let mapping_current = feedback.configuration_revision() == bundle.configuration.revision()
        && feedback.transport_epoch() == generation.epochs.transport;
    if !mapping_current {
        raw.devices.clear();
    }
    let ready = mapping_current
        && diagnostics.ready()
        && feedback.is_active()
        && feedback.transport_epoch() == generation.epochs.transport
        && raw.cursor.boot_id
            == Uuid::parse_str(admission.boot_id())
                .map_err(|e| e.to_string())?
                .as_u128();
    let now = admission.monotonic_ms();
    let modules = project(
        &bundle.configuration,
        &raw,
        now,
        max_age,
        ready,
        offset,
        limit,
    )?;
    let ids = modules
        .iter()
        .flat_map(|module| module.slots.iter())
        .filter_map(|slot| slot.binding.as_ref())
        .map(|binding| DeviceId::new(&binding.id).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut commands = vec![];
    for chunk in ids.chunks(128) {
        commands.extend(
            admission
                .recent_device_commands(controller, generation.clone(), chunk.to_vec(), 64)
                .await
                .map_err(|e| format!("{e:?}"))?
                .into_iter()
                .map(|r| CommandRecord {
                    command: r.command.to_string(),
                    device: r.device,
                    status: r.status,
                    requested: requested_command(&r.intent),
                    transport_epoch: r.transport_epoch.to_string(),
                    control_epoch: r.control_epoch.to_string(),
                    updated_ms: r.updated_ms.to_string(),
                }),
        );
    }
    commands.sort_by(|a, b| {
        b.updated_ms
            .parse::<u64>()
            .unwrap()
            .cmp(&a.updated_ms.parse::<u64>().unwrap())
            .then_with(|| {
                b.command
                    .parse::<u64>()
                    .unwrap()
                    .cmp(&a.command.parse::<u64>().unwrap())
            })
    });
    commands.truncate(64);
    let total = bundle
        .configuration
        .gateways()
        .values()
        .map(|g| g.modules().len())
        .sum::<usize>();
    let next_offset = (offset + modules.len() < total).then_some(offset + modules.len());
    Ok(Page {
        boot: admission.boot_id().to_owned(),
        revision: bundle.configuration.revision().to_string(),
        transport_epoch: generation.epochs.transport.to_string(),
        control_epoch: generation.epochs.control.to_string(),
        observed_transport_epoch: feedback.transport_epoch().to_string(),
        truth_source: "single_feedback_owner_physical_bindings",
        captured_ms: now.to_string(),
        ready,
        modules,
        commands,
        next_offset,
    })
}

/// A logical DeviceId is only the binding adapter for this physical channel.
/// Hidden supply outputs are observed here but controlled via the logical light.
pub(crate) fn control_device(
    config: &homesense_config::Configuration,
    module_id: &str,
    channel: u8,
) -> Result<DeviceId, String> {
    let (_, module) = config
        .module(module_id)
        .ok_or("persistent module ID missing")?;
    module
        .validate_channel(homesense_core::Channel::new(channel).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (id, binding) = config
        .devices()
        .iter()
        .find(|(_, binding)| {
            binding.module_id.as_deref() == Some(module_id) && binding.channel.get() == channel
        })
        .ok_or("unbound module channel")?;
    if !binding.enabled {
        return Err("disabled channel".into());
    }
    if config
        .devices()
        .values()
        .any(|binding| binding.supply_relay.as_ref() == Some(id))
    {
        return Err("supply relay uses logical-light control".into());
    }
    if !matches!(
        module.kind(),
        ModuleKind::Switch | ModuleKind::Dimmer | ModuleKind::Motor
    ) {
        return Err("module channel has no enabled typed control".into());
    }
    Ok(id.clone())
}

fn requested_command(intent: &[u8]) -> serde_json::Value {
    match intent {
        [1, 0, 0, 0] => serde_json::json!({"kind":"query"}),
        [1, 1, value, 0] if *value <= 1 => serde_json::json!({"kind":"power","on":*value==1}),
        [1, 2 | 3, level, 0] if *level <= 100 => serde_json::json!({"kind":"level","level":level}),
        [1, 4, 0, 0] => serde_json::json!({"kind":"pause_ramp"}),
        [1, 5, 0, 0] => serde_json::json!({"kind":"motor","motion":"up"}),
        [1, 5, 1, 0] => serde_json::json!({"kind":"motor","motion":"down"}),
        [1, 5, 2, 0] => serde_json::json!({"kind":"motor","motion":"stop"}),
        [1, 6, value, 0] if *value <= 1 => serde_json::json!({"kind":"panel_led","on":*value==1}),
        [1, 7, 0, 0] => serde_json::json!({"kind":"native_toggle"}),
        _ => serde_json::json!({"kind":"unknown"}),
    }
}

#[cfg(test)]
#[path = "module_object_tests.rs"]
mod tests;
