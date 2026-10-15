//! Strict versioned configuration input; immutable, validated runtime mappings.
//! Parsing never connects, changes hardware or upgrades simulation evidence.
pub mod rule_document;
pub mod scene_tracking;
pub mod keypad;
use homesense_core::{
    BusAddress, Channel, DeviceId, ExecutionContext, ModuleKind, ModuleSpec, PanelLedMode, Percent,
    ProtocolProfile, ValidationError, Verification, VerificationRecord,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Validation(ValidationError),
    Invalid(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<ValidationError> for Error {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    revision: u64,
    #[serde(default)]
    manual_degraded_enabled: bool,
    #[serde(default)]
    rooms: Vec<Room>,
    #[serde(default)]
    floors: Vec<String>,
    gateways: Vec<GatewayInput>,
    devices: Vec<DeviceInput>,
    #[serde(default)]
    keypads: Vec<keypad::KeypadInput>,
}
/// Optional display metadata; room membership never grants control permission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Room {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub floor: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayInput {
    id: String,
    endpoint: SocketAddr,
    modules: Vec<ModuleInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleInput {
    #[serde(default)]
    id: Option<String>,
    address: String,
    profile: String,
    verification: EvidenceInput,
    #[serde(default)]
    led_mode: LedMode,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceInput {
    Pending {},
    Simulated {},
    Verified {
        evidence: String,
        verified_at_unix_ms: u64,
    },
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum LedMode {
    #[default]
    Unknown,
    SelfReset,
    Manual,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceInput {
    #[serde(default)]
    room: Option<String>,
    id: String,
    name: String,
    #[serde(default)]
    module_id: Option<String>,
    #[serde(default)]
    gateway: Option<String>,
    #[serde(default)]
    address: Option<String>,
    channel: u8,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    default_on_level: Option<u8>,
    #[serde(default)]
    motor_calibration: Option<MotorCalibrationInput>,
    #[serde(default)]
    curtain_state: Option<CurtainStateInput>,
    #[serde(default)]
    supply_relay: Option<String>,
    #[serde(default)]
    supply: Option<ModuleChannel>,
}
/// Explicit local review for command/time inference and its fixed LED effects.
/// Absence keeps the existing motor path and publishes no estimate or LEDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurtainState {
    pub timing: homesense_core::curtain_state::Timing,
    pub leds: CurtainLeds,
    pub evidence: VerificationRecord,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CurtainLeds {
    pub open: Vec<DeviceId>,
    pub close: Vec<DeviceId>,
    pub stop: Vec<DeviceId>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CurtainLedInput {
    #[serde(default)]
    open: Vec<String>,
    #[serde(default)]
    close: Vec<String>,
    #[serde(default)]
    stop: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurtainStateInput {
    #[serde(default)]
    open_travel_ms: Option<u64>,
    #[serde(default)]
    close_travel_ms: Option<u64>,
    #[serde(default)]
    stop_led_idle_ms: Option<u64>,
    #[serde(default)]
    leds: CurtainLedInput,
    evidence: String,
    reviewed_at_unix_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MotorCalibrationInput {
    open_direction: OpenDirection,
    stop_verified: bool,
    evidence: String,
    verified_at_unix_ms: u64,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpenDirection {
    Up,
    Down,
}
#[derive(Debug, Clone)]
pub struct MotorCalibration {
    open_direction: OpenDirection,
    evidence: VerificationRecord,
}
impl MotorCalibration {
    pub fn evidence(&self) -> &VerificationRecord {
        &self.evidence
    }
    pub fn motion(&self, action: homesense_core::MotorAction) -> homesense_core::MotorMotion {
        use homesense_core::{MotorAction as A, MotorMotion as M};
        match (action, self.open_direction) {
            (A::Stop, _) => M::Stop,
            (A::Open, OpenDirection::Up) | (A::Close, OpenDirection::Down) => M::Up,
            (A::Close, OpenDirection::Up) | (A::Open, OpenDirection::Down) => M::Down,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Gateway {
    pub endpoint: SocketAddr,
    modules: BTreeMap<BusAddress, ModuleSpec>,
}
impl Gateway {
    pub fn modules(&self) -> &BTreeMap<BusAddress, ModuleSpec> {
        &self.modules
    }
}
#[derive(Debug,Clone,PartialEq,Eq,serde::Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleChannel {pub module_id:String,pub channel:u8}
#[derive(Debug, Clone)]
pub struct LogicalDevice {
    /// Stable module reference in schema 2; legacy schema 1 has no fabricated ID.
    pub module_id: Option<String>,
    pub supply_channel: Option<ModuleChannel>,
    pub room: Option<String>,
    pub name: String,
    pub channel: Channel,
    pub enabled: bool,
    pub default_on_level: Option<Percent>,
    pub motor_calibration: Option<MotorCalibration>,
    pub curtain_state: Option<CurtainState>,

}
/// Current-revision transport adapter. Logical Device has no physical locator.
#[derive(Debug,Clone)]
pub struct ResolvedBinding {pub gateway:String,pub address:BusAddress,pub supply_relay:Option<DeviceId>,logical:LogicalDevice}
/// Source-compatible name for the immutable transport adapter. New runtime
/// domain objects use LogicalDevice and stable ModuleChannel references.
pub type Device=ResolvedBinding;
impl std::ops::Deref for ResolvedBinding {type Target=LogicalDevice;fn deref(&self)->&LogicalDevice{&self.logical}}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ModuleLocation {pub gateway:String,pub address:BusAddress}
#[derive(Debug, Clone)]
pub struct Configuration {
    schema_version:u32,
    module_registry:BTreeMap<String,ModuleLocation>,
    revision: u64,
    manual_degraded_enabled: bool,
    gateways: BTreeMap<String, Gateway>,
    devices: BTreeMap<DeviceId, LogicalDevice>,
    resolved_bindings:BTreeMap<DeviceId,ResolvedBinding>,
    rooms: BTreeMap<String, Room>,
    floors: BTreeSet<String>,
    keypads: BTreeMap<DeviceId, keypad::Keypad>,
}
impl Configuration {
    pub fn keypads(&self) -> &BTreeMap<DeviceId, keypad::Keypad> { &self.keypads }
    pub fn schema_version(&self)->u32 {self.schema_version}
    pub fn module_registry(&self)->&BTreeMap<String,ModuleLocation>{&self.module_registry}
    pub fn module(&self,id:&str)->Option<(&ModuleLocation,&ModuleSpec)>{let location=self.module_registry.get(id)?;Some((location,self.gateways.get(&location.gateway)?.modules.get(&location.address)?))}
    pub fn module_id(&self,gateway:&str,address:BusAddress)->Option<&str>{self.module_registry.iter().find(|(_,location)|location.gateway==gateway && location.address==address).map(|(id,_)|id.as_str())}

    /// Does not send or authorize a command. The caller must still pass the
    /// command through normal admission, verification and transport ownership.
    pub fn motor_command(
        &self,
        id: &DeviceId,
        action: homesense_core::MotorAction,
    ) -> Result<homesense_core::Command, Error> {
        let (device, module) = self
            .binding(id)
            .ok_or_else(|| invalid("motor disabled or missing"))?;
        if module.kind() != ModuleKind::Motor {
            return Err(invalid("motor action requires a motor binding"));
        }
        let calibration = device
            .motor_calibration
            .as_ref()
            .ok_or_else(|| invalid("motor direction and STOP calibration required"))?;
        Ok(homesense_core::Command::Motor(calibration.motion(action)))
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn manual_degraded_enabled(&self) -> bool {
        self.manual_degraded_enabled
    }
    pub fn gateways(&self) -> &BTreeMap<String, Gateway> {
        &self.gateways
    }
    pub fn rooms(&self) -> &BTreeMap<String, Room> { &self.rooms }
    pub fn floors(&self) -> &BTreeSet<String> { &self.floors }
    /// Logical domain objects contain stable module references, never addresses.
    pub fn logical_devices(&self)->&BTreeMap<DeviceId,LogicalDevice>{&self.devices}
    /// Compatibility enumeration for transport adapters in the current immutable
    /// revision. New domain code should use logical_devices/module/channel.
    pub fn devices(&self) -> &BTreeMap<DeviceId, ResolvedBinding> {
        &self.resolved_bindings
    }
    pub fn binding(&self, id: &DeviceId) -> Option<(&ResolvedBinding, &ModuleSpec)> {
        let device = self.resolved_bindings.get(id)?;
        if !device.enabled {
            return None;
        }
        Some((
            device,
            self.gateways
                .get(&device.gateway)?
                .modules
                .get(&device.address)?,
        ))
    }
    pub fn parse(bytes: &[u8], context: ExecutionContext) -> Result<Self, Error> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(invalid("configuration exceeds 4 MiB"));
        }
        let mut input: Document = serde_json::from_slice(bytes).map_err(Error::Json)?;
        if !matches!(input.schema_version,1|2) {
            return Err(invalid("unsupported schema_version"));
        }
        if input.revision == 0 || input.revision > i64::MAX as u64 {
            return Err(invalid("revision must fit a positive SQLite integer"));
        }
        if input.gateways.is_empty() || input.gateways.len() > homesense_core::gateway_scope::MAX_GATEWAYS || input.devices.len() > 4096 {
            return Err(invalid("gateway/device count exceeds limits"));
        }
        let profiles = [
            ProtocolProfile::Ad8Din,
            ProtocolProfile::R8i8DinPro,
            ProtocolProfile::C4DinPro,
            ProtocolProfile::GtC8Rl,
            ProtocolProfile::GtE4Rl,
            ProtocolProfile::GtD8Rl,
            ProtocolProfile::GtL8Rl,
            ProtocolProfile::GtM4Rl,
            ProtocolProfile::Bt86cRs485,
            ProtocolProfile::Ts686Rs485SixKey,
            ProtocolProfile::UnverifiedKeypad,
        ];
        let mut gateways = BTreeMap::new();
        let mut module_registry=BTreeMap::new();
        let mut endpoints = BTreeSet::new();
        for g in input.gateways {
            identifier(&g.id)?;
            if gateways.contains_key(&g.id) {
                return Err(invalid(format!("duplicate gateway {}", g.id)));
            }
            if g.endpoint.port() == 0
                || g.endpoint.ip().is_unspecified()
                || g.endpoint.ip().is_multicast()
            {
                return Err(invalid("invalid unicast gateway endpoint"));
            }
            if !endpoints.insert(g.endpoint) {
                return Err(invalid("multiple gateway IDs target the same endpoint"));
            }
            if g.modules.len() > 255 {
                return Err(invalid("too many modules"));
            }
            let mut modules = BTreeMap::new();
            for m in g.modules {
                let address = BusAddress::from_hex(&m.address)?;
                match (input.schema_version,m.id) {
                    (2,Some(id))=>{identifier(&id)?;if module_registry.insert(id,ModuleLocation{gateway:g.id.clone(),address}).is_some(){return Err(invalid("duplicate stable module ID"));}},
                    (1,None)=>{},
                    _=>return Err(invalid("schema 2 requires persistent module IDs; schema 1 cannot partially introduce them")),
                }

                let profile = profiles
                    .iter()
                    .find(|p| p.id() == m.profile)
                    .copied()
                    .ok_or_else(|| invalid(format!("unknown profile {}", m.profile)))?;
                let evidence = match m.verification {
                    EvidenceInput::Pending {} => Verification::Pending,
                    EvidenceInput::Simulated {} => Verification::Simulated,
                    EvidenceInput::Verified {
                        evidence,
                        verified_at_unix_ms,
                    } => {
                        if evidence.len() > 4096 {
                            return Err(invalid("verification evidence exceeds limit"));
                        }
                        Verification::Verified(VerificationRecord::new(
                            evidence,
                            verified_at_unix_ms,
                        )?)
                    }
                };
                let mut module = ModuleSpec::new(address, profile, evidence)?;
                match m.led_mode {
                    LedMode::Unknown => (),
                    LedMode::Manual => module = module.with_panel_led_mode(PanelLedMode::Manual)?,
                    LedMode::SelfReset => {
                        module = module.with_panel_led_mode(PanelLedMode::SelfReset)?
                    }
                }
                if modules.insert(address, module).is_some() {
                    return Err(invalid("duplicate module address"));
                }
            }
            gateways.insert(
                g.id,
                Gateway {
                    endpoint: g.endpoint,
                    modules,
                },
            );
        }
        if input.rooms.len()>256 {return Err(invalid("room count exceeds limit"));}
        if input.floors.len() > 256 {return Err(invalid("floor count exceeds limit"));}
        let mut floors=BTreeSet::new();
        for floor in input.floors {
            if floor.trim().is_empty() || floor.len()>256 || floor.chars().any(char::is_control) {return Err(invalid("invalid floor label"));}
            if !floors.insert(floor) {return Err(invalid("duplicate floor label"));}
        }
        let mut rooms=BTreeMap::new();
        for room in input.rooms {
            identifier(&room.id)?;
            for label in std::iter::once(&room.name).chain(room.floor.iter()) {
                if label.trim().is_empty() || label.len()>256 || label.chars().any(char::is_control) {return Err(invalid("invalid room or floor label"));}
            }
            if let Some(floor)=&room.floor {floors.insert(floor.clone());}
            if floors.len()>256 {return Err(invalid("floor count exceeds limit"));}
            if rooms.insert(room.id.clone(),room).is_some() {return Err(invalid("duplicate room id"));}
        }
        let keypads = keypad::expand(input.schema_version, input.keypads, &mut input.devices, &module_registry, &gateways, &rooms)?;
        let mut devices = BTreeMap::new();
        let mut pending_supplies=Vec::new();
        let mut resolved_bindings=BTreeMap::new();
        let mut bindings = BTreeSet::new();
        for d in input.devices {
            identifier(&d.id)?;
            if d.name.trim().is_empty() || d.name.len() > 256 {
                return Err(invalid("device name empty or too long"));
            }
            let id = DeviceId::new(d.id)?;
            let (gateway_id,address,module_id)=match (input.schema_version,d.module_id,d.gateway,d.address) {
                (1,None,Some(gateway),Some(address))=>(gateway,BusAddress::from_hex(&address)?,None),
                (2,Some(id),None,None)=>{identifier(&id)?;let location=module_registry.get(&id).ok_or_else(||invalid("unknown stable module reference"))?;(location.gateway.clone(),location.address,Some(id))},
                _=>return Err(invalid("logical bindings require module_id + channel in schema 2; physical address fields are legacy only")),
            };
            let gateway = gateways
                .get(&gateway_id)
                .ok_or_else(|| invalid("unknown gateway reference"))?;
            let module = gateway
                .modules
                .get(&address)
                .ok_or_else(|| invalid("unknown module reference"))?;
            let channel = Channel::new(d.channel)?;
            // Unknown keypad layouts can be retained disabled without assuming six keys.
            if module.profile() != ProtocolProfile::UnverifiedKeypad {
                module.validate_channel(channel)?;
            }
            if d.enabled {
                module.validate_context(context)?;
            }
            if !bindings.insert((gateway_id.clone(), address, channel.get())) {
                return Err(invalid("duplicate physical device channel"));
            }
            let default_on_level = d.default_on_level.map(Percent::new).transpose()?;
            if default_on_level
                .is_some_and(|level| level.get() == 0 || module.kind() != ModuleKind::Dimmer)
            {
                return Err(invalid(
                    "default_on_level requires a dimmer and a value in 1..100",
                ));
            }
            let motor_calibration = match d.motor_calibration {
                Some(calibration) => {
                    if module.kind() != ModuleKind::Motor
                        || !calibration.stop_verified
                        || calibration.evidence.len() > 4096
                    {
                        return Err(invalid(
                            "motor calibration requires a motor, verified STOP and bounded evidence",
                        ));
                    }
                    Some(MotorCalibration {
                        open_direction: calibration.open_direction,
                        evidence: VerificationRecord::new(
                            calibration.evidence,
                            calibration.verified_at_unix_ms,
                        )?,
                    })
                }
                None => None,
            };
            let curtain_state = d.curtain_state.map(|state| -> Result<_, Error> {
                if module.kind() != ModuleKind::Motor || motor_calibration.is_none()
                    || !d.enabled || state.evidence.len() > 4096 {
                    return Err(invalid("curtain state requires enabled calibrated motor and bounded review evidence"));
                }
                let timing = homesense_core::curtain_state::Timing {
                    open_travel_ms: state.open_travel_ms,
                    close_travel_ms: state.close_travel_ms,
                    stop_led_idle_ms: state.stop_led_idle_ms,
                }.validate().map_err(|_|invalid("invalid curtain inference timing"))?;
                let ids = |values: Vec<String>| -> Result<Vec<DeviceId>, Error> {
                    if values.len() > 16 { return Err(invalid("curtain LED binding limit")); }
                    values.into_iter().map(|id| { identifier(&id)?; DeviceId::new(id).map_err(Error::from) }).collect()
                };
                Ok(CurtainState {
                    timing,
                    leds: CurtainLeds { open: ids(state.leds.open)?, close: ids(state.leds.close)?, stop: ids(state.leds.stop)? },
                    evidence: VerificationRecord::new(state.evidence, state.reviewed_at_unix_ms)?,
                })
            }).transpose()?;
            if d.room.as_ref().is_some_and(|id| !rooms.contains_key(id)) {return Err(invalid("unknown room reference"));}
            if (input.schema_version==1 && d.supply.is_some()) || (input.schema_version==2 && d.supply_relay.is_some()){return Err(invalid("schema 2 supply references require module_id + channel"));}
            if let Some(port)=&d.supply {identifier(&port.module_id)?;let location=module_registry.get(&port.module_id).ok_or_else(||invalid("supply module missing"))?;gateways[&location.gateway].modules[&location.address].validate_channel(Channel::new(port.channel)?)?;pending_supplies.push((id.clone(),port.clone()));}
            let device = LogicalDevice {
                module_id,
                supply_channel:d.supply,
                room: d.room,
                name: d.name,
                channel,
                enabled: d.enabled,
                default_on_level,
                motor_calibration,
                curtain_state,

            };
            let legacy_supply=d.supply_relay.map(|id|{identifier(&id)?;DeviceId::new(id).map_err(Error::from)}).transpose()?;
            resolved_bindings.insert(id.clone(),ResolvedBinding{gateway:gateway_id,address,supply_relay:legacy_supply,logical:device.clone()});
            if devices.insert(id, device).is_some() {
                return Err(invalid("duplicate device ID"));
            }
        }
        for (id,port) in pending_supplies {
            let relay_id=devices.iter().find(|(_,binding)|binding.module_id.as_deref()==Some(port.module_id.as_str()) && binding.channel.get()==port.channel).map(|(id,_)|id.clone()).ok_or_else(||invalid("supply module channel requires a registered physical binding"))?;
            resolved_bindings.get_mut(&id).unwrap().supply_relay=Some(relay_id);
        }
        let mut supplies = BTreeSet::new();
        for (id, device) in &resolved_bindings {
            if let Some(relay_id) = &device.supply_relay {
                let relay = resolved_bindings
                    .get(relay_id)
                    .ok_or_else(|| invalid("unknown supply relay"))?;
                let physical=&resolved_bindings[id];let relay_physical=&resolved_bindings[relay_id];
                let dimmer = &gateways[&physical.gateway].modules[&physical.address];
                let switch = &gateways[&relay_physical.gateway].modules[&relay_physical.address];
                if id == relay_id
                    || dimmer.kind() != ModuleKind::Dimmer
                    || switch.kind() != ModuleKind::Switch
                    || relay.supply_relay.is_some()
                    || physical.gateway != relay_physical.gateway
                    || device.enabled != relay.enabled
                    || !supplies.insert(relay_id.clone())
                {
                    return Err(invalid(
                        "supply relay requires an exclusive same-gateway switch with matching enabled state",
                    ));
                }
                if device.default_on_level.is_none() {
                    return Err(invalid(
                        "combined light requires an explicit nonzero default_on_level",
                    ));
                }
            }
        }
        let mut curtain_leds = BTreeSet::new();
        for device in devices.values() {
            if let Some(state) = &device.curtain_state {
                for id in state.leds.open.iter().chain(&state.leds.close).chain(&state.leds.stop) {
                    let led = resolved_bindings.get(id).ok_or_else(||invalid("curtain LED binding missing"))?;
                    let module = &gateways[&led.gateway].modules[&led.address];
                    if !led.enabled || module.kind() != ModuleKind::Keypad
                        || module.led_mode() != PanelLedMode::Manual || !curtain_leds.insert(id.clone()) {
                        return Err(invalid("curtain LEDs require enabled exclusive manual-mode keypad bindings"));
                    }
                    module.validate_context(context)?;
                }
            }
        }
        for panel in keypads.values() {for key in panel.keys.values() {
            if let Some(keypad::KeyAction::Motor{device,..})=&key.action {
                let id=DeviceId::new(device)?;
                if resolved_bindings.get(&id).is_none_or(|d|!d.enabled || d.motor_calibration.is_none()) {return Err(invalid("keypad motor action requires enabled calibrated target"));}
            }
        }}
        Ok(Self {
            schema_version:input.schema_version,
            module_registry,
            revision: input.revision,
            manual_degraded_enabled: input.manual_degraded_enabled,
            rooms,
            floors,
            gateways,
            devices,
            resolved_bindings,
            keypads,
        })
    }
}
fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(invalid("invalid stable identifier"))
    } else {
        Ok(())
    }
}

pub mod scene_switches;

pub mod indicator_links;

pub mod scene_observation;

/// Read-only discriminator for the storage CAS downgrade barrier.
pub fn document_schema_version(payload:&[u8])->Option<u32>{serde_json::from_slice::<serde_json::Value>(payload).ok()?.get("schema_version")?.as_u64().and_then(|n|u32::try_from(n).ok())}
