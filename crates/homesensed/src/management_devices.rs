//! Typed device management, sharing HAP normalization and calibrated Admission.
#[cfg(test)]
#[path = "management_device_freshness_tests.rs"]
mod freshness_tests;
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
};
use homesense_core::{DeviceId, ModuleKind};
use homesense_hap::requests::{Batch, LightBinding, LightRequests};
use uuid::Uuid;
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Deserialize,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum MotorDirection {Open,Close}
impl MotorDirection {
    pub(crate) fn requested(self)->homesense_core::MotorAction {match self {Self::Open=>homesense_core::MotorAction::Open,Self::Close=>homesense_core::MotorAction::Close}}
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceAction {
    Power { on: bool },
    Brightness { level: u8 },
    PanelLed { on: bool },
    PanelLedOff,
    Open,
    Close,
    Stop,
    MotorPress { direction:MotorDirection },
    /// Configured native card key, resolved atomically by Admission. Never
    /// accepted as a pre-normalized physical motor direction.
    KeypadPress { keypad: String, key: u8 },
}
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceView {
    pub availability:Option<crate::gateway_runtime::View>,
    pub catalog_visible:bool,
    pub room: Option<homesense_config::Room>,
    pub id: DeviceId,
    pub name: String,
    pub motor_calibrated: bool,
    pub motor_estimate: Option<crate::curtain_owner::View>,
    pub state: homesense_state::DeviceState,
    pub reported: Option<homesense_core::Observation>,
    pub recorded: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub boot: String,
    pub revision: u64,
    pub sequence: u64,
    pub devices: Vec<DeviceView>,
    pub floors: Vec<String>,
    pub(crate) configured_rooms: Vec<homesense_config::Room>,
    pub(crate) scoped_catalog_rooms: Vec<homesense_config::Room>,
    pub keypads: Vec<serde_json::Value>,
    pub keypad_endpoints: std::collections::BTreeSet<DeviceId>,
    pub(crate) keypad_execute_scopes: std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,
    pub(crate) keypad_away_scope: Option<crate::away_off::CatalogScope>,
}
/// Metadata only; this projection never adds devices, endpoints or control rights.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Directory {
    pub rooms: Vec<homesense_config::Room>,
    pub floors: Vec<String>,
}
impl DeviceSnapshot {
    pub(crate) fn directory(&self, configuration_manager:bool)->Directory {
        if configuration_manager {
            return Directory{rooms:self.configured_rooms.clone(),floors:self.floors.clone()};
        }
        let rooms=self.scoped_catalog_rooms.clone();
        let floors=rooms.iter().filter_map(|room|room.floor.clone())
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        Directory{rooms,floors}
    }
}
pub async fn snapshot(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
) -> Result<DeviceSnapshot, String> {
    snapshot_bound(admission, controller, generation, ids, feedback, None).await
}
pub(crate) async fn snapshot_with_max_age(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
    max_age_ms: u64,
 ) -> Result<DeviceSnapshot, String> {
    snapshot_bound(admission,controller,generation,ids,feedback,Some(max_age_ms)).await
}
async fn snapshot_bound(admission:&Admission,controller:Uuid,generation:TaskGeneration,ids:Vec<DeviceId>,feedback:&Feedback,max_age_ms:Option<u64>)->Result<DeviceSnapshot,String> {
    if max_age_ms.is_some_and(|age|age==0 || age>homesense_config::RefreshPolicy::MAX_OBSERVATION_AGE_MS) {
        return Err("invalid device freshness bound".into());
    }
    if ids.len() > crate::web_auth::MAX_DEVICE_SCOPE {
        return Err("device query limit exceeded".into());
    }
    let bundle = admission
        .automation_bundle(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let configured=bundle.configuration.refresh_policy().observation_max_age_ms();
    let requested=max_age_ms.unwrap_or(configured);
    if requested>configured {return Err("device freshness exceeds configured allowance".into());}
    let max_age_ms=feedback.effective_max_age_ms(requested);
    let motor_estimates=admission.curtain_views(controller,generation.clone(),ids.clone()).await.map_err(|e|format!("curtain inference: {e:?}"))?;
    let gateway_views=admission.gateway_availability().await.map_err(|e|format!("gateway view: {e:?}"))?;
    let recorded=admission.projection_levels(generation.epochs.configuration,feedback.snapshot().cursor.boot_id).await.map_err(|e|format!("recorded levels: {e:?}"))?;
    // Evaluate freshness after awaited reads. The periodic expiration owner may
    // be delayed; a cached Confirmed flag alone cannot establish a current view.
    // Expire physical components before combining, so a fresh dimmer cannot
    // conceal its old supply feedback. Retain original observations and cursors.
    let mut raw = feedback.snapshot();
    let now = admission.monotonic_ms();
    for state in raw.devices.values_mut() {
        if matches!(state.kind, ModuleKind::Switch | ModuleKind::Dimmer)
            && state.quality == homesense_state::Quality::Confirmed
            && state.observed_at_ms.and_then(|at| now.checked_sub(at))
                .is_none_or(|age| age >= max_age_ms)
        {
            state.quality = homesense_state::Quality::Stale;
        }
    }
    let snapshot = crate::combined_light::project(&bundle.configuration, &raw)
        .map_err(|e| e.to_string())?;
    let published=crate::light_state::project_pending(&bundle.configuration,&raw,&recorded).map_err(str::to_owned)?;
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
        let mut state = snapshot
            .devices
            .get(&id)
            .ok_or("missing device state")?
            .clone();
        let motor_calibrated = bundle
            .configuration
            .motor_command(&id, homesense_core::MotorAction::Stop)
            .is_ok();
        let availability=gateway_views.iter().find(|view|view.gateway==device.gateway).cloned();
        if availability.as_ref().is_some_and(|view|view.state!="ready") {state.quality=homesense_state::Quality::Unknown;}
        devices.push(DeviceView {
            availability,
            catalog_visible:device.catalog_visible,
            room: device
                .room
                .as_ref()
                .and_then(|id| bundle.configuration.rooms().get(id))
                .cloned(),
            id: id.clone(),
            name: device.name.clone(),
            motor_calibrated,
            motor_estimate: motor_estimates.get(&id).cloned(),
            reported: published.devices.get(&id).and_then(|s|s.observed),
            // Only dimmers actually consume the target overlay. Relay Off also
            // has an internal restore-policy entry, but relay state is physical.
            recorded: state.kind == ModuleKind::Dimmer
                && state.observed.is_some()
                && recorded.contains_key(&id),
            state,
        });
    }
    let (keypads, keypad_endpoints) = crate::management_keypads::snapshot(
        admission, controller, generation, &bundle.configuration, &raw, &devices, feedback.transport_epoch(), now,
    ).await?;
    // Retain the full scoped directory before event deltas trim device rows.
    let mut scoped_catalog_rooms=devices.iter().filter(|d|d.catalog_visible).filter_map(|d|d.room.as_ref())
        .map(|room|(room.id.clone(),room.clone())).collect::<std::collections::BTreeMap<_,_>>();
    for panel in &keypads {
        if let Some(id)=panel["room"]["id"].as_str() && let Some(room)=bundle.configuration.rooms().get(id) {
            scoped_catalog_rooms.insert(id.to_owned(),room.clone());
        }
    }
    let mut keypad_execute_scopes=std::collections::BTreeMap::new();
    if let Some(document)=&bundle.automation {for panel in bundle.configuration.keypads().values(){for key in panel.keys.values(){if let Some(homesense_config::keypad::KeyAction::SceneExecute{scene,..})=&key.action {let ids=homesense_config::keypad::scene_execute_devices(document,&bundle.configuration,scene).map_err(|e|e.to_string())?;keypad_execute_scopes.insert(scene.clone(),ids.into_iter().map(|id|id.as_str().to_owned()).collect());}}}}
    Ok(DeviceSnapshot {
        boot: admission.boot_id().to_owned(),
        revision: bundle.configuration.revision(),
        sequence: snapshot.cursor.sequence,
        devices,
        keypads,
        keypad_endpoints,
        keypad_execute_scopes,
        keypad_away_scope:bundle.automation.as_ref().and_then(|document|crate::away_off::catalog_scope(&bundle.configuration,document,&gateway_views)),
        floors: bundle.configuration.floors().iter().cloned().collect(),
        configured_rooms: bundle.configuration.rooms().values().cloned().collect(),
        scoped_catalog_rooms:scoped_catalog_rooms.into_values().collect(),
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
    if matches!(action,DeviceAction::PanelLed{..}|DeviceAction::PanelLedOff) {
        if module.profile() != homesense_core::ProtocolProfile::Ts686Rs485SixKey
            || module.led_mode() != homesense_core::PanelLedMode::Manual {
            return Err("panel LED requires reviewed TS6 manual mode".into());
        }
        return Ok(Batch { controller, subscriptions: vec![], intents: vec![
            homesense_hap::requests::Intent { device: id, command: match action {DeviceAction::PanelLed{on}=>homesense_core::Command::PanelLed(on),_=>homesense_core::Command::PanelLedOff} }
        ] });
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
