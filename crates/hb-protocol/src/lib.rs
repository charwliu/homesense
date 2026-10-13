//! HopeBeauty five-byte binary protocol. No transport or hardware side effects.
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, LevelTransition, ModuleKind, ModuleSpec,
    MotorMotion, Observation, PanelLedMode, Percent, ValidationError,
};
use std::{collections::BTreeMap, fmt};

pub const FRAME_LEN: usize = 5;
pub const START: u8 = 0xAE;
pub const END: u8 = 0xEE;

/// Preserve unknown payloads for diagnostics; this type alone authorizes no action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawFrame {
    pub address: u8,
    pub command: u8,
    pub value: u8,
}
impl RawFrame {
    pub const fn bytes(self) -> [u8; FRAME_LEN] {
        [START, self.address, self.command, self.value, END]
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() != FRAME_LEN || bytes[0] != START || bytes[4] != END {
            return Err(ProtocolError::MalformedFrame);
        }
        Ok(Self {
            address: bytes[1],
            command: bytes[2],
            value: bytes[3],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    Validation(ValidationError),
    UnsupportedCommand,
    MalformedFrame,
    DuplicateAddress,
}
impl From<ValidationError> for ProtocolError {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProtocolError {}

/// Only profiles whose same-model manuals explicitly document A0 F2.
/// R8 PRO's ambiguous A8 entry and GT-C8's individual-only table are excluded.
pub fn supports_module_query(profile: homesense_core::ProtocolProfile) -> bool {
    matches!(
        profile,
        homesense_core::ProtocolProfile::Ad8Din
            | homesense_core::ProtocolProfile::GtE4Rl
            | homesense_core::ProtocolProfile::GtD8Rl
            | homesense_core::ProtocolProfile::GtL8Rl
    )
}

/// Addressed read only; BusAddress excludes broadcast. No channel-zero escape
/// through the ordinary command encoder, and no relaxation of verification.
pub fn encode_module_query(
    module: &ModuleSpec,
    context: ExecutionContext,
) -> Result<RawFrame, ProtocolError> {
    module.validate_context(context)?;
    if !supports_module_query(module.profile()) {
        return Err(ProtocolError::UnsupportedCommand);
    }
    Ok(RawFrame {
        address: module.address().get(),
        command: 0xA0,
        value: 0xF2,
    })
}

pub fn encode(
    module: &ModuleSpec,
    channel: Channel,
    command: Command,
) -> Result<RawFrame, ProtocolError> {
    encode_in_context(module, channel, command, ExecutionContext::Live)
}

/// Simulation must be explicit; runtime transports must use the Live encoder.
pub fn encode_in_context(
    module: &ModuleSpec,
    channel: Channel,
    command: Command,
    context: ExecutionContext,
) -> Result<RawFrame, ProtocolError> {
    module.validate_context(context)?;
    module.validate_channel(channel)?;
    if matches!(command, Command::PanelLed(_))
        && module.kind() == ModuleKind::Keypad
        && module.led_mode() != PanelLedMode::Manual
    {
        return Err(ValidationError::LedModeNotManual.into());
    }
    // AD8 manual 7.3 documents only immediate off/full-on; arbitrary
    // percentages are documented under the Bx ramp command in section 7.4.
    if module.profile() == homesense_core::ProtocolProfile::Ad8Din
        && matches!(command, Command::SetLevel { level, transition: LevelTransition::Immediate } if !matches!(level.get(), 0 | 100))
    {
        return Err(ProtocolError::UnsupportedCommand);
    }
    let c = channel.get();
    let (command, value) = match (module.kind(), command) {
        (ModuleKind::Switch, Command::Switch(on)) | (ModuleKind::Keypad, Command::PanelLed(on)) => {
            (c, if on { 0xF1 } else { 0xF2 })
        }
        (ModuleKind::Dimmer, Command::SetLevel { level, transition }) => (
            match transition {
                LevelTransition::Immediate => c,
                LevelTransition::Ramp => 0xB0 + c,
            },
            level.get(),
        ),
        (ModuleKind::Switch | ModuleKind::Dimmer, Command::Query) => (0xA0 + c, 0xF2),
        (ModuleKind::Switch | ModuleKind::Dimmer, Command::Toggle)
            if matches!(
                module.profile(),
                homesense_core::ProtocolProfile::R8i8DinPro
                    | homesense_core::ProtocolProfile::Ad8Din
                    | homesense_core::ProtocolProfile::GtC8Rl
                    | homesense_core::ProtocolProfile::GtE4Rl
                    | homesense_core::ProtocolProfile::GtD8Rl
                    | homesense_core::ProtocolProfile::GtL8Rl
            ) =>
        {
            (0xA0 + c, 0xF1)
        }
        (ModuleKind::Dimmer, Command::PauseRamp) => (0xA0 + c, 0x05),
        (ModuleKind::Motor, Command::Motor(motion)) => (
            c,
            match motion {
                MotorMotion::Up => 0xF1,
                MotorMotion::Down => 0xF2,
                MotorMotion::Stop => 0xF0,
            },
        ),
        _ => return Err(ProtocolError::UnsupportedCommand),
    };
    Ok(RawFrame {
        address: module.address().get(),
        command,
        value,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    UnregisteredAddress,
    UnsupportedFeedback,
    InvalidChannel,
    InvalidValue,
    UnverifiedModule,
    SimulationOnly,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Feedback {
        address: BusAddress,
        channel: Channel,
        observation: Observation,
    },
    Unknown {
        frame: RawFrame,
        reason: UnknownReason,
    },
}

#[derive(Debug, Default)]
pub struct Registry {
    modules: BTreeMap<BusAddress, ModuleSpec>,
    context: ExecutionContext,
}
impl Registry {
    pub fn for_simulation() -> Self {
        Self {
            context: ExecutionContext::Simulation,
            ..Self::default()
        }
    }
    pub fn module(&self, address: BusAddress) -> Option<&ModuleSpec> {
        self.modules.get(&address)
    }
    pub fn register(&mut self, module: ModuleSpec) -> Result<(), ProtocolError> {
        if self.modules.contains_key(&module.address()) {
            return Err(ProtocolError::DuplicateAddress);
        }
        self.modules.insert(module.address(), module);
        Ok(())
    }
    pub fn decode(&self, frame: RawFrame) -> Decoded {
        let unknown = |reason| Decoded::Unknown { frame, reason };
        let Ok(address) = BusAddress::new(frame.address) else {
            return unknown(UnknownReason::UnregisteredAddress);
        };
        let Some(module) = self.modules.get(&address) else {
            return unknown(UnknownReason::UnregisteredAddress);
        };
        match module.validate_context(self.context) {
            Err(ValidationError::SimulationOnly) => return unknown(UnknownReason::SimulationOnly),
            Err(_) => return unknown(UnknownReason::UnverifiedModule),
            Ok(()) => {}
        }
        if module.kind() == ModuleKind::Motor || !(0xD1..=0xE0).contains(&frame.command) {
            return unknown(UnknownReason::UnsupportedFeedback);
        }
        let channel = Channel::new(frame.command - 0xD0).expect("validated feedback channel");
        if module.validate_channel(channel).is_err() {
            return unknown(UnknownReason::InvalidChannel);
        }
        let observation = match module.kind() {
            ModuleKind::Dimmer => match Percent::new(frame.value) {
                Ok(level) => Observation::Level(level),
                Err(_) => return unknown(UnknownReason::InvalidValue),
            },
            ModuleKind::Switch | ModuleKind::Keypad => {
                let active = match frame.value {
                    0xF1 => true,
                    0xF2 => false,
                    _ => return unknown(UnknownReason::InvalidValue),
                };
                if module.kind() == ModuleKind::Switch {
                    Observation::Switch(active)
                } else {
                    Observation::Button { pressed: active }
                }
            }
            ModuleKind::Motor => return unknown(UnknownReason::UnsupportedFeedback),
        };
        Decoded::Feedback {
            address,
            channel,
            observation,
        }
    }
}

/// Incremental fixed-length framing. At most four bytes are retained between calls.
/// Delimiter bytes inside ID/CMD/VALUE are payload, not separators.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    buffer: [u8; FRAME_LEN],
    len: usize,
    discarded: u64,
    malformed: u64,
}
impl StreamDecoder {
    pub const fn buffered_len(&self) -> usize {
        self.len
    }
    pub const fn discarded_bytes(&self) -> u64 {
        self.discarded
    }
    pub const fn malformed_frames(&self) -> u64 {
        self.malformed
    }
    /// Report an incomplete frame on disconnect; callers must not carry it into a new connection.
    pub fn reset(&mut self) -> usize {
        let pending = self.len;
        self.len = 0;
        pending
    }
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<RawFrame> {
        let mut output = Vec::new();
        for &byte in chunk {
            if self.len == 0 && byte != START {
                self.discarded = self.discarded.saturating_add(1);
                continue;
            }
            self.buffer[self.len] = byte;
            self.len += 1;
            if self.len < FRAME_LEN {
                continue;
            }
            if self.buffer[4] == END {
                output.push(RawFrame {
                    address: self.buffer[1],
                    command: self.buffer[2],
                    value: self.buffer[3],
                });
                self.len = 0;
            } else {
                self.malformed = self.malformed.saturating_add(1);
                let keep = self.buffer[1..]
                    .iter()
                    .position(|b| *b == START)
                    .map(|i| i + 1)
                    .unwrap_or(FRAME_LEN);
                self.discarded = self.discarded.saturating_add(keep as u64);
                self.buffer.copy_within(keep..FRAME_LEN, 0);
                self.len = FRAME_LEN - keep;
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{ProtocolProfile, Verification, VerificationRecord};
    fn spec(kind: ModuleKind, n: u8) -> ModuleSpec {
        let profile = match (kind, n) {
            (ModuleKind::Switch, 8) => ProtocolProfile::GtC8Rl,
            (ModuleKind::Dimmer, 4) => ProtocolProfile::GtE4Rl,
            (ModuleKind::Dimmer, 8) => ProtocolProfile::GtD8Rl,
            (ModuleKind::Motor, 4) => ProtocolProfile::GtM4Rl,
            (ModuleKind::Keypad, 6) => ProtocolProfile::Bt86cRs485,
            _ => panic!("no documented profile for fixture"),
        };
        let module = ModuleSpec::new(
            BusAddress::from_hex("15").unwrap(),
            profile,
            Verification::Verified(
                VerificationRecord::new("unit-test fixture, not field evidence", 1).unwrap(),
            ),
        )
        .unwrap();
        if kind == ModuleKind::Keypad {
            module.with_panel_led_mode(PanelLedMode::Manual).unwrap()
        } else {
            module
        }
    }
    fn ch(n: u8) -> Channel {
        Channel::new(n).unwrap()
    }
    #[test]
    fn native_toggle_vectors_are_distinct_from_query_for_all_reviewed_channels() {
        for profile in [
            ProtocolProfile::R8i8DinPro,
            ProtocolProfile::Ad8Din,
            ProtocolProfile::GtC8Rl,
            ProtocolProfile::GtE4Rl,
            ProtocolProfile::GtD8Rl,
            ProtocolProfile::GtL8Rl,
        ] {
            let module = ModuleSpec::new(
                BusAddress::new(0x15).unwrap(),
                profile,
                Verification::Verified(
                    VerificationRecord::new("manufacturer vector fixture", 1).unwrap(),
                ),
            )
            .unwrap();
            for channel in 1..=module.channels() {
                assert_eq!(
                    encode(&module, ch(channel), Command::Toggle)
                        .unwrap()
                        .bytes(),
                    [0xae, 0x15, 0xa0 + channel, 0xf1, 0xee]
                );
                assert_eq!(
                    encode(&module, ch(channel), Command::Query)
                        .unwrap()
                        .bytes(),
                    [0xae, 0x15, 0xa0 + channel, 0xf2, 0xee]
                );
            }
            assert!(encode(&module, ch(module.channels() + 1), Command::Toggle).is_err());
        }
    }
    #[test]
    fn native_toggle_keeps_verification_context_and_capability_gates() {
        for profile in [ProtocolProfile::R8i8DinPro, ProtocolProfile::Ad8Din] {
            let pending =
                ModuleSpec::new(BusAddress::new(1).unwrap(), profile, Verification::Pending)
                    .unwrap();
            assert_eq!(
                encode(&pending, ch(1), Command::Toggle),
                Err(ProtocolError::Validation(ValidationError::UnverifiedModule))
            );
            let simulated = ModuleSpec::new(
                BusAddress::new(1).unwrap(),
                profile,
                Verification::Simulated,
            )
            .unwrap();
            assert_eq!(
                encode(&simulated, ch(1), Command::Toggle),
                Err(ProtocolError::Validation(ValidationError::SimulationOnly))
            );
            assert!(
                encode_in_context(
                    &simulated,
                    ch(1),
                    Command::Toggle,
                    ExecutionContext::Simulation
                )
                .is_ok()
            );
        }
        for (kind, count) in [(ModuleKind::Motor, 4), (ModuleKind::Keypad, 6)] {
            assert_eq!(
                encode(&spec(kind, count), ch(1), Command::Toggle),
                Err(ProtocolError::UnsupportedCommand)
            );
        }
    }
    #[test]
    fn manufacturer_command_vectors() {
        let cases = [
            (
                ModuleKind::Switch,
                8,
                Command::Switch(true),
                [0xAE, 0x15, 1, 0xF1, 0xEE],
            ),
            (
                ModuleKind::Switch,
                8,
                Command::Switch(false),
                [0xAE, 0x15, 1, 0xF2, 0xEE],
            ),
            (
                ModuleKind::Dimmer,
                8,
                Command::SetLevel {
                    level: Percent::new(30).unwrap(),
                    transition: LevelTransition::Immediate,
                },
                [0xAE, 0x15, 1, 0x1E, 0xEE],
            ),
            (
                ModuleKind::Dimmer,
                4,
                Command::SetLevel {
                    level: Percent::new(100).unwrap(),
                    transition: LevelTransition::Ramp,
                },
                [0xAE, 0x15, 0xB1, 0x64, 0xEE],
            ),
            (
                ModuleKind::Switch,
                8,
                Command::Query,
                [0xAE, 0x15, 0xA1, 0xF2, 0xEE],
            ),
            (
                ModuleKind::Dimmer,
                8,
                Command::Query,
                [0xAE, 0x15, 0xA1, 0xF2, 0xEE],
            ),
            (
                ModuleKind::Dimmer,
                8,
                Command::PauseRamp,
                [0xAE, 0x15, 0xA1, 5, 0xEE],
            ),
            (
                ModuleKind::Motor,
                4,
                Command::Motor(MotorMotion::Up),
                [0xAE, 0x15, 1, 0xF1, 0xEE],
            ),
            (
                ModuleKind::Motor,
                4,
                Command::Motor(MotorMotion::Down),
                [0xAE, 0x15, 1, 0xF2, 0xEE],
            ),
            (
                ModuleKind::Motor,
                4,
                Command::Motor(MotorMotion::Stop),
                [0xAE, 0x15, 1, 0xF0, 0xEE],
            ),
            (
                ModuleKind::Keypad,
                6,
                Command::PanelLed(false),
                [0xAE, 0x15, 1, 0xF2, 0xEE],
            ),
        ];
        for (kind, n, command, expected) in cases {
            assert_eq!(
                encode(&spec(kind, n), ch(1), command).unwrap().bytes(),
                expected
            );
        }
        assert_eq!(
            encode(&spec(ModuleKind::Dimmer, 8), ch(8), Command::Query)
                .unwrap()
                .command,
            0xA8
        );
        assert_eq!(
            encode(
                &spec(ModuleKind::Motor, 4),
                ch(4),
                Command::Motor(MotorMotion::Stop)
            )
            .unwrap()
            .command,
            4
        );
    }
    #[test]
    fn unsupported_commands_and_channels_fail() {
        assert_eq!(
            encode(&spec(ModuleKind::Motor, 4), ch(1), Command::Query),
            Err(ProtocolError::UnsupportedCommand)
        );
        assert!(encode(&spec(ModuleKind::Dimmer, 4), ch(5), Command::Query).is_err());
        assert!(
            encode(
                &spec(ModuleKind::Switch, 8),
                ch(1),
                Command::Motor(MotorMotion::Stop)
            )
            .is_err()
        );
        assert!(encode(&spec(ModuleKind::Keypad, 6), ch(1), Command::Query).is_err());
    }
    #[test]
    fn identical_feedback_is_classified_by_registered_module() {
        let frame = RawFrame {
            address: 0x15,
            command: 0xD1,
            value: 0xF1,
        };
        for (kind, n, expected) in [
            (ModuleKind::Switch, 8, Observation::Switch(true)),
            (ModuleKind::Keypad, 6, Observation::Button { pressed: true }),
        ] {
            let mut registry = Registry::default();
            registry.register(spec(kind, n)).unwrap();
            assert_eq!(
                registry.decode(frame),
                Decoded::Feedback {
                    address: BusAddress::new(0x15).unwrap(),
                    channel: ch(1),
                    observation: expected
                }
            );
            assert_eq!(
                registry.register(spec(ModuleKind::Dimmer, 8)),
                Err(ProtocolError::DuplicateAddress)
            );
            assert!(
                matches!(registry.decode(frame), Decoded::Feedback { observation, .. } if observation == expected)
            );
        }
    }
    #[test]
    fn unknown_frames_are_preserved_without_events() {
        let mut registry = Registry::default();
        registry.register(spec(ModuleKind::Dimmer, 4)).unwrap();
        for (address, command, value, reason) in [
            (0, 0xD1, 30, UnknownReason::UnregisteredAddress),
            (0x16, 0xD1, 30, UnknownReason::UnregisteredAddress),
            (0x15, 0xCC, 30, UnknownReason::UnsupportedFeedback),
            (0x15, 0xD5, 30, UnknownReason::InvalidChannel),
            (0x15, 0xD1, 101, UnknownReason::InvalidValue),
        ] {
            let frame = RawFrame {
                address,
                command,
                value,
            };
            assert_eq!(registry.decode(frame), Decoded::Unknown { frame, reason });
        }
    }
    #[test]
    fn all_fragment_boundaries_and_coalesced_frames() {
        let expected = vec![
            RawFrame {
                address: 0x15,
                command: 0xD1,
                value: 30,
            },
            RawFrame {
                address: 0x05,
                command: 0xD8,
                value: 0xF2,
            },
        ];
        let bytes: Vec<_> = expected.iter().flat_map(|f| f.bytes()).collect();
        for split in 0..=bytes.len() {
            let mut decoder = StreamDecoder::default();
            let mut actual = decoder.feed(&bytes[..split]);
            actual.extend(decoder.feed(&bytes[split..]));
            assert_eq!(actual, expected);
            assert_eq!(decoder.buffered_len(), 0);
        }
        let mut decoder = StreamDecoder::default();
        assert_eq!(
            bytes
                .iter()
                .flat_map(|b| decoder.feed(&[*b]))
                .collect::<Vec<_>>(),
            expected
        );
    }
    #[test]
    fn delimiters_are_legal_payload_bytes() {
        let frames = [
            RawFrame {
                address: START,
                command: END,
                value: END,
            },
            RawFrame {
                address: END,
                command: START,
                value: START,
            },
        ];
        let bytes: Vec<_> = frames.iter().flat_map(|f| f.bytes()).collect();
        assert_eq!(StreamDecoder::default().feed(&bytes), frames);
        assert_eq!(RawFrame::parse(&frames[0].bytes()).unwrap(), frames[0]);
        assert!(RawFrame::parse(&[]).is_err());
        assert!(RawFrame::parse(&[START, 1, 2, 3, 4]).is_err());
    }
    #[test]
    fn corrupt_frames_resynchronize_and_disconnect_drops_partial() {
        let frame = RawFrame {
            address: 0x15,
            command: 0xD1,
            value: 30,
        };
        let mut decoder = StreamDecoder::default();
        let mut bytes = vec![1, 2, START, 1, 2];
        bytes.extend(frame.bytes());
        assert_eq!(decoder.feed(&bytes), vec![frame]);
        assert_eq!(decoder.malformed_frames(), 1);
        assert_eq!(decoder.discarded_bytes(), 5);
        decoder.feed(&frame.bytes()[..3]);
        assert_eq!(decoder.reset(), 3);
        assert!(decoder.feed(&frame.bytes()[3..]).is_empty());
        assert_eq!(decoder.feed(&frame.bytes()), vec![frame]);
    }
    #[test]
    fn malformed_input_retains_bounded_memory() {
        let mut decoder = StreamDecoder::default();
        assert!(decoder.feed(&vec![START; 100_000]).is_empty());
        assert!(decoder.buffered_len() < FRAME_LEN);
        assert!(decoder.malformed_frames() > 0);
    }
}
