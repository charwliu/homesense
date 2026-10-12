//! Strict versioned configuration input; immutable, validated runtime mappings.
//! Parsing never connects, changes hardware or upgrades simulation evidence.
pub mod rule_document;
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
    gateways: Vec<GatewayInput>,
    devices: Vec<DeviceInput>,
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
    gateway: String,
    address: String,
    channel: u8,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    default_on_level: Option<u8>,
    #[serde(default)]
    motor_calibration: Option<MotorCalibrationInput>,
    #[serde(default)]
    supply_relay: Option<String>,
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
#[derive(Debug, Clone)]
pub struct Device {
    pub room: Option<String>,
    pub name: String,
    pub gateway: String,
    pub address: BusAddress,
    pub channel: Channel,
    pub enabled: bool,
    pub default_on_level: Option<Percent>,
    pub motor_calibration: Option<MotorCalibration>,
    /// Separate physical supply relay for this dimmer; not a second logical light.
    pub supply_relay: Option<DeviceId>,
}
#[derive(Debug, Clone)]
pub struct Configuration {
    revision: u64,
    manual_degraded_enabled: bool,
    gateways: BTreeMap<String, Gateway>,
    devices: BTreeMap<DeviceId, Device>,
    rooms: BTreeMap<String, Room>,
}
impl Configuration {
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
    pub fn devices(&self) -> &BTreeMap<DeviceId, Device> {
        &self.devices
    }
    pub fn binding(&self, id: &DeviceId) -> Option<(&Device, &ModuleSpec)> {
        let device = self.devices.get(id)?;
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
        let input: Document = serde_json::from_slice(bytes).map_err(Error::Json)?;
        if input.schema_version != 1 {
            return Err(invalid("unsupported schema_version"));
        }
        if input.revision == 0 || input.revision > i64::MAX as u64 {
            return Err(invalid("revision must fit a positive SQLite integer"));
        }
        if input.gateways.is_empty() || input.gateways.len() > 64 || input.devices.len() > 4096 {
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
        let mut rooms=BTreeMap::new();
        for room in input.rooms {
            identifier(&room.id)?;
            for label in std::iter::once(&room.name).chain(room.floor.iter()) {
                if label.trim().is_empty() || label.len()>256 || label.chars().any(char::is_control) {return Err(invalid("invalid room or floor label"));}
            }
            if rooms.insert(room.id.clone(),room).is_some() {return Err(invalid("duplicate room id"));}
        }
        let mut devices = BTreeMap::new();
        let mut bindings = BTreeSet::new();
        for d in input.devices {
            identifier(&d.id)?;
            if d.name.trim().is_empty() || d.name.len() > 256 {
                return Err(invalid("device name empty or too long"));
            }
            let id = DeviceId::new(d.id)?;
            let address = BusAddress::from_hex(&d.address)?;
            let gateway = gateways
                .get(&d.gateway)
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
            if !bindings.insert((d.gateway.clone(), address, channel.get())) {
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
            if d.room.as_ref().is_some_and(|id| !rooms.contains_key(id)) {return Err(invalid("unknown room reference"));}
            let device = Device {
                room: d.room,
                name: d.name,
                gateway: d.gateway,
                address,
                channel,
                enabled: d.enabled,
                default_on_level,
                motor_calibration,
                supply_relay: d
                    .supply_relay
                    .map(|id| {
                        identifier(&id)?;
                        DeviceId::new(id).map_err(Error::from)
                    })
                    .transpose()?,
            };
            if devices.insert(id, device).is_some() {
                return Err(invalid("duplicate device ID"));
            }
        }
        let mut supplies = BTreeSet::new();
        for (id, device) in &devices {
            if let Some(relay_id) = &device.supply_relay {
                let relay = devices
                    .get(relay_id)
                    .ok_or_else(|| invalid("unknown supply relay"))?;
                let dimmer = &gateways[&device.gateway].modules[&device.address];
                let switch = &gateways[&relay.gateway].modules[&relay.address];
                if id == relay_id
                    || dimmer.kind() != ModuleKind::Dimmer
                    || switch.kind() != ModuleKind::Switch
                    || relay.supply_relay.is_some()
                    || device.gateway != relay.gateway
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
        Ok(Self {
            revision: input.revision,
            manual_degraded_enabled: input.manual_degraded_enabled,
            rooms,
            gateways,
            devices,
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
