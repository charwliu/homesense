//! HomeSense device types. Construction validates wire-level ranges.
pub mod variable_migration;
pub mod admission;
pub mod arbiter;
pub mod calendar;
pub mod execution;
pub mod deferred_plan;
pub mod gesture;
pub mod fade;
pub mod queue;
pub mod rule;
pub mod rule_chain;
pub mod scene;
pub mod scene_expand;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    BroadcastAddress,
    InvalidHexAddress,
    ChannelOutOfRange,
    PercentOutOfRange,
    UnverifiedModule,
    SimulationOnly,
    InvalidVerification,
    LedModeNotManual,
    UnsupportedCapability,
    EmptyDeviceId,
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BusAddress(u8);
impl BusAddress {
    pub fn new(value: u8) -> Result<Self, ValidationError> {
        if value == 0 {
            Err(ValidationError::BroadcastAddress)
        } else {
            Ok(Self(value))
        }
    }
    /// Control4 HopeBeauty IDs are exactly two hexadecimal digits, never decimal IDs.
    pub fn from_hex(value: &str) -> Result<Self, ValidationError> {
        if value.len() != 2 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(ValidationError::InvalidHexAddress);
        }
        Self::new(u8::from_str_radix(value, 16).map_err(|_| ValidationError::InvalidHexAddress)?)
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}
impl fmt::Display for BusAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02X}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Channel(u8);
impl Channel {
    pub fn new(value: u8) -> Result<Self, ValidationError> {
        if (1..=16).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ValidationError::ChannelOutOfRange)
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Percent(u8);
impl Percent {
    pub fn new(value: u8) -> Result<Self, ValidationError> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(ValidationError::PercentOutOfRange)
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);
impl DeviceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(ValidationError::EmptyDeviceId)
        } else {
            Ok(Self(value))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Switch,
    Dimmer,
    Motor,
    Keypad,
}

/// Versioned manufacturer profiles. Unknown layouts never inherit a known protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolProfile {
    Ad8Din,
    R8i8DinPro,
    C4DinPro,
    GtC8Rl,
    GtE4Rl,
    GtD8Rl,
    GtL8Rl,
    GtM4Rl,
    Bt86cRs485,
    Ts686Rs485SixKey,
    UnverifiedKeypad,
}
impl ProtocolProfile {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ad8Din => "ad8_din_rs485_v2_2_1",
            Self::R8i8DinPro => "r8i8_din_pro_v2_7_7",
            Self::C4DinPro => "c4_din_pro_v2_7_7",
            Self::GtC8Rl => "gt_c8_rl_v2_8_2_2",
            Self::GtE4Rl => "gt_e4_rl_v3_2_2_2",
            Self::GtD8Rl => "gt_d8_rl_v2_2_6_2",
            Self::GtL8Rl => "gt_l8_rl_v2_0_7_2",
            Self::GtM4Rl => "gt_m4_rl_v2_1_9_2",
            Self::Bt86cRs485 => "bt86c_rs485_v2_9_6_2",
            Self::Ts686Rs485SixKey => "ts6_86_rs485_v2_6_5_6key",
            Self::UnverifiedKeypad => "unverified_keypad",
        }
    }
    pub const fn kind(self) -> ModuleKind {
        match self {
            Self::GtC8Rl | Self::R8i8DinPro => ModuleKind::Switch,
            Self::GtE4Rl | Self::GtD8Rl | Self::GtL8Rl | Self::Ad8Din => ModuleKind::Dimmer,
            Self::GtM4Rl | Self::C4DinPro => ModuleKind::Motor,
            Self::Bt86cRs485 | Self::Ts686Rs485SixKey | Self::UnverifiedKeypad => {
                ModuleKind::Keypad
            }
        }
    }
    pub const fn channels(self) -> u8 {
        match self {
            Self::GtC8Rl | Self::GtD8Rl | Self::GtL8Rl | Self::R8i8DinPro | Self::Ad8Din => 8,
            Self::GtE4Rl | Self::GtM4Rl | Self::C4DinPro => 4,
            Self::Bt86cRs485 | Self::Ts686Rs485SixKey => 6,
            Self::UnverifiedKeypad => 0,
        }
    }
}

/// Evidence is supplied by an operator, never inferred from an incoming frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRecord {
    evidence: String,
    verified_at_unix_ms: u64,
}
impl VerificationRecord {
    pub fn new(
        evidence: impl Into<String>,
        verified_at_unix_ms: u64,
    ) -> Result<Self, ValidationError> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() || verified_at_unix_ms == 0 {
            return Err(ValidationError::InvalidVerification);
        }
        Ok(Self {
            evidence,
            verified_at_unix_ms,
        })
    }
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
    pub const fn verified_at_unix_ms(&self) -> u64 {
        self.verified_at_unix_ms
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    Pending,
    Simulated,
    Verified(VerificationRecord),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionContext {
    #[default]
    Live,
    Simulation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelLedMode {
    #[default]
    Unknown,
    SelfReset,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSpec {
    address: BusAddress,
    profile: ProtocolProfile,
    verification: Verification,
    led_mode: PanelLedMode,
}
impl ModuleSpec {
    pub fn new(
        address: BusAddress,
        profile: ProtocolProfile,
        verification: Verification,
    ) -> Result<Self, ValidationError> {
        if profile == ProtocolProfile::UnverifiedKeypad && verification != Verification::Pending {
            return Err(ValidationError::UnverifiedModule);
        }
        Ok(Self {
            address,
            profile,
            verification,
            led_mode: PanelLedMode::Unknown,
        })
    }
    /// Consuming configuration edit; callers must publish a new runtime revision.
    pub fn with_panel_led_mode(mut self, mode: PanelLedMode) -> Result<Self, ValidationError> {
        if !matches!(
            self.profile,
            ProtocolProfile::Bt86cRs485 | ProtocolProfile::Ts686Rs485SixKey
        ) {
            return Err(ValidationError::UnsupportedCapability);
        }
        self.led_mode = mode;
        Ok(self)
    }
    pub const fn address(&self) -> BusAddress {
        self.address
    }
    pub const fn profile(&self) -> ProtocolProfile {
        self.profile
    }
    pub fn verification(&self) -> &Verification {
        &self.verification
    }
    pub const fn kind(&self) -> ModuleKind {
        self.profile.kind()
    }
    pub const fn channels(&self) -> u8 {
        self.profile.channels()
    }
    pub const fn led_mode(&self) -> PanelLedMode {
        self.led_mode
    }
    pub fn validate_context(&self, context: ExecutionContext) -> Result<(), ValidationError> {
        match (&self.verification, context) {
            (Verification::Pending, _) => Err(ValidationError::UnverifiedModule),
            (Verification::Simulated, ExecutionContext::Live) => {
                Err(ValidationError::SimulationOnly)
            }
            _ => Ok(()),
        }
    }
    pub fn validate_channel(&self, channel: Channel) -> Result<(), ValidationError> {
        if channel.get() <= self.channels() {
            Ok(())
        } else {
            Err(ValidationError::ChannelOutOfRange)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelTransition {
    Immediate,
    Ramp,
}
/// Electrical directions only. Open/close needs a separately verified mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotorMotion {
    Up,
    Down,
    Stop,
}
/// User intent; resolve with the channel's verified calibration before encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotorAction {
    Open,
    Close,
    Stop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Switch(bool),
    SetLevel {
        level: Percent,
        transition: LevelTransition,
    },
    Query,
    PauseRamp,
    Motor(MotorMotion),
    PanelLed(bool),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Switch(bool),
    Level(Percent),
    Button { pressed: bool },
}

impl Command {
    /// Whether feedback reports this command's absolute target state.
    /// This is not an acknowledgement: the bus has no transaction identifier,
    /// so another controller may have produced the same state. The caller must
    /// also validate the endpoint, connection epoch and feedback timing.
    pub fn target_matches(self, observation: Observation) -> bool {
        match (self, observation) {
            (Self::Switch(target), Observation::Switch(actual)) => target == actual,
            (Self::SetLevel { level, .. }, Observation::Level(actual)) => level == actual,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_must_match_absolute_target_and_never_acknowledges_motor_or_led() {
        let target = Percent::new(73).unwrap();
        for transition in [LevelTransition::Immediate, LevelTransition::Ramp] {
            let command = Command::SetLevel {
                level: target,
                transition,
            };
            assert!(command.target_matches(Observation::Level(target)));
            assert!(!command.target_matches(Observation::Level(Percent::new(30).unwrap())));
            assert!(!command.target_matches(Observation::Switch(true)));
        }
        assert!(Command::Switch(true).target_matches(Observation::Switch(true)));
        assert!(!Command::Switch(false).target_matches(Observation::Switch(true)));
        for command in [
            Command::Motor(MotorMotion::Stop),
            Command::PanelLed(true),
            Command::Query,
            Command::PauseRamp,
        ] {
            assert!(!command.target_matches(Observation::Button { pressed: true }));
            assert!(!command.target_matches(Observation::Switch(true)));
        }
    }
    #[test]
    fn address_is_hex_and_broadcast_is_rejected() {
        assert_eq!(BusAddress::from_hex("15").unwrap().get(), 21);
        assert_eq!(BusAddress::from_hex("ae").unwrap().to_string(), "AE");
        assert_eq!(
            BusAddress::from_hex("00"),
            Err(ValidationError::BroadcastAddress)
        );
        for bad in ["1", "0x15", " 15", "GG", "123", "é"] {
            assert!(BusAddress::from_hex(bad).is_err());
        }
    }
    #[test]
    fn ranges_and_module_capabilities_are_checked() {
        assert!(Channel::new(0).is_err());
        assert!(Channel::new(17).is_err());
        assert!(Percent::new(101).is_err());
        assert!(Percent::new(100).is_ok());
        let id = BusAddress::new(1).unwrap();
        let spec = ModuleSpec::new(id, ProtocolProfile::GtE4Rl, Verification::Simulated).unwrap();
        assert!(spec.validate_channel(Channel::new(5).unwrap()).is_err());
        assert!(DeviceId::new("  ").is_err());
        assert_eq!(DeviceId::new("c4:362").unwrap().as_str(), "c4:362");
    }
}
