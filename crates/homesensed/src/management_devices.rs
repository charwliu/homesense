//! Typed device management, sharing HAP normalization and calibrated Admission.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
};
use homesense_core::{DeviceId, ModuleKind};
use homesense_hap::requests::{Batch, LightBinding, LightRequests};
use uuid::Uuid;
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceAction {
    Power { on: bool },
    Brightness { level: u8 },
    Open,
    Close,
    Stop,
}
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceView {
    pub room: Option<homesense_config::Room>,
    pub id: DeviceId,
    pub name: String,
    pub motor_calibrated: bool,
    pub state: homesense_state::DeviceState,
}
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub boot: String,
    pub revision: u64,
    pub sequence: u64,
    pub devices: Vec<DeviceView>,
}
pub async fn snapshot(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
) -> Result<DeviceSnapshot, String> {
    if ids.len() > 256 {
        return Err("device query limit exceeded".into());
    }
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let snapshot = crate::combined_light::project(&bundle.configuration, &feedback.snapshot())
        .map_err(|e| e.to_string())?;
    let supplies: std::collections::BTreeSet<_> = bundle
        .configuration
        .devices()
        .values()
        .filter(|device| device.enabled)
        .filter_map(|device| device.supply_relay.as_ref())
        .collect();
    let mut devices = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            return Err("duplicate device".into());
        }
        let (device, _) = bundle
            .configuration
            .binding(&id)
            .ok_or("unknown or disabled device")?;
        // Supply relays belong to their logical light, even when an administrator
        // scope includes all enabled physical bindings. Never expose them twice.
        if supplies.contains(&id) {
            continue;
        }
        let state = snapshot
            .devices
            .get(&id)
            .ok_or("missing device state")?
            .clone();
        let motor_calibrated = bundle
            .configuration
            .motor_command(&id, homesense_core::MotorAction::Stop)
            .is_ok();
        devices.push(DeviceView {
            room: device
                .room
                .as_ref()
                .and_then(|id| bundle.configuration.rooms().get(id))
                .cloned(),
            id,
            name: device.name.clone(),
            motor_calibrated,
            state,
        });
    }
    Ok(DeviceSnapshot {
        boot: admission.boot_id().to_owned(),
        revision: bundle.configuration.revision(),
        sequence: snapshot.cursor.sequence,
        devices,
    })
}
pub async fn prepare(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    id: DeviceId,
    action: DeviceAction,
    feedback: &Feedback,
    max_age_ms: u64,
) -> Result<Batch, String> {
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let (device, module) = bundle
        .configuration
        .binding(&id)
        .ok_or("unknown or disabled device")?;
    if bundle
        .configuration
        .devices()
        .values()
        .any(|binding| binding.enabled && binding.supply_relay.as_ref() == Some(&id))
    {
        return Err("supply relay is controlled through its logical light".into());
    }
    let dimmable = match module.kind() {
        ModuleKind::Dimmer => true,
        ModuleKind::Switch => false,
        _ => return Err("not a light device".into()),
    };
    if matches!(action, DeviceAction::Brightness { .. }) && !dimmable {
        return Err("brightness unsupported".into());
    }
    let (iid, value) = match action {
        DeviceAction::Power { on } => (1, serde_json::json!(on)),
        DeviceAction::Brightness { level } if level <= 100 => (2, serde_json::json!(level)),
        _ => return Err("invalid brightness".into()),
    };
    let levels = admission
        .remember_levels(
            bundle.configuration.revision(),
            feedback.snapshot(),
            max_age_ms,
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let requests = LightRequests::new(vec![LightBinding {
        device: id,
        aid: 2,
        on_iid: 1,
        brightness_iid: dimmable.then_some(2),
        default_on_level: device.default_on_level,
    }])
    .map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(
        &serde_json::json!({"characteristics":[{"aid":2,"iid":iid,"value":value}]}),
    )
    .map_err(|e| e.to_string())?;
    requests
        .normalize(controller, &body, |id| levels.get(id).copied())
        .map_err(|e| e.to_string())
}
