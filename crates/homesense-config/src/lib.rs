//! Strict versioned configuration input; immutable, validated runtime mappings.
//! Parsing never connects, changes hardware or upgrades simulation evidence.
use homesense_core::{
    BusAddress, Channel, DeviceId, ExecutionContext, ModuleSpec, PanelLedMode, ProtocolProfile,
    ValidationError, Verification, VerificationRecord,
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
    gateways: Vec<GatewayInput>,
    devices: Vec<DeviceInput>,
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
    id: String,
    name: String,
    gateway: String,
    address: String,
    channel: u8,
    #[serde(default)]
    enabled: bool,
}
#[derive(Debug)]
pub struct Gateway {
    pub endpoint: SocketAddr,
    modules: BTreeMap<BusAddress, ModuleSpec>,
}
impl Gateway {
    pub fn modules(&self) -> &BTreeMap<BusAddress, ModuleSpec> {
        &self.modules
    }
}
#[derive(Debug)]
pub struct Device {
    pub name: String,
    pub gateway: String,
    pub address: BusAddress,
    pub channel: Channel,
    pub enabled: bool,
}
#[derive(Debug)]
pub struct Configuration {
    revision: u64,
    manual_degraded_enabled: bool,
    gateways: BTreeMap<String, Gateway>,
    devices: BTreeMap<DeviceId, Device>,
}
impl Configuration {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn manual_degraded_enabled(&self) -> bool {
        self.manual_degraded_enabled
    }
    pub fn gateways(&self) -> &BTreeMap<String, Gateway> {
        &self.gateways
    }
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
            ProtocolProfile::GtC8Rl,
            ProtocolProfile::GtE4Rl,
            ProtocolProfile::GtD8Rl,
            ProtocolProfile::GtL8Rl,
            ProtocolProfile::GtM4Rl,
            ProtocolProfile::Bt86cRs485,
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
            let device = Device {
                name: d.name,
                gateway: d.gateway,
                address,
                channel,
                enabled: d.enabled,
            };
            if devices.insert(id, device).is_some() {
                return Err(invalid("duplicate device ID"));
            }
        }
        Ok(Self {
            revision: input.revision,
            manual_degraded_enabled: input.manual_degraded_enabled,
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
