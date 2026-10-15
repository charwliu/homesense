//! In-memory, deterministic GJ-compatible gateway. No physical I/O.
//! Ramps reach their target immediately here: this does not model real timing.
use hb_protocol::{RawFrame, StreamDecoder};
use homesense_core::{
    BusAddress, Channel, ExecutionContext, ModuleKind, ModuleSpec, MotorMotion, PanelLedMode,
};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct Faults {
    pub reply_delay_ms: u64,
    pub drop_replies: bool,
    pub duplicate_replies: bool,
    pub output_chunk_size: usize,
}
impl Default for Faults {
    fn default() -> Self {
        Self {
            reply_delay_ms: 0,
            drop_replies: false,
            duplicate_replies: false,
            output_chunk_size: 5,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    InvalidConfig,
    DuplicateAddress,
    UnknownModule,
    UnsupportedCommand,
    InvalidValue,
    ClockWentBackwards,
    TimeOverflow,
}
impl std::fmt::Display for SimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SimError {}

pub struct Gateway {
    modules: BTreeMap<BusAddress, ModuleSpec>,
    values: BTreeMap<(u8, u8), u8>,
    // Synthetic toggle policy: restore last nonzero dimmer value, default 100.
    // Manufacturer manuals do not establish this as real hardware behavior.
    toggle_restore: BTreeMap<(u8, u8), u8>,
    motors: BTreeMap<(u8, u8), MotorMotion>,
    decoder: StreamDecoder,
    pending: VecDeque<(u64, RawFrame)>,
    faults: Faults,
    reply_capacity: usize,
    now_ms: u64,
    dropped: u64,
}
impl Gateway {
    pub fn new(
        modules: Vec<ModuleSpec>,
        faults: Faults,
        reply_capacity: usize,
    ) -> Result<Self, SimError> {
        if faults.output_chunk_size == 0 || reply_capacity == 0 {
            return Err(SimError::InvalidConfig);
        }
        let mut registered = BTreeMap::new();
        for module in modules {
            module
                .validate_context(ExecutionContext::Simulation)
                .map_err(|_| SimError::InvalidConfig)?;
            if registered.insert(module.address(), module).is_some() {
                return Err(SimError::DuplicateAddress);
            }
        }
        Ok(Self {
            modules: registered,
            values: BTreeMap::new(),
            toggle_restore: BTreeMap::new(),
            motors: BTreeMap::new(),
            decoder: StreamDecoder::default(),
            pending: VecDeque::new(),
            faults,
            reply_capacity,
            now_ms: 0,
            dropped: 0,
        })
    }
    pub fn dropped_replies(&self) -> u64 {
        self.dropped
    }
    pub fn pending_replies(&self) -> usize {
        self.pending.len()
    }
    pub fn last_motor_command(&self, address: BusAddress, channel: Channel) -> Option<MotorMotion> {
        self.motors.get(&(address.get(), channel.get())).copied()
    }
    fn advance(&mut self, now_ms: u64) -> Result<(), SimError> {
        if now_ms < self.now_ms {
            return Err(SimError::ClockWentBackwards);
        }
        self.now_ms = now_ms;
        Ok(())
    }
    fn schedule(&mut self, reply: RawFrame, now_ms: u64) -> Result<(), SimError> {
        let due = now_ms
            .checked_add(self.faults.reply_delay_ms)
            .ok_or(SimError::TimeOverflow)?;
        for _ in 0..if self.faults.duplicate_replies { 2 } else { 1 } {
            if self.faults.drop_replies || self.pending.len() == self.reply_capacity {
                self.dropped = self.dropped.saturating_add(1);
            } else {
                self.pending.push_back((due, reply));
            }
        }
        Ok(())
    }
    /// Each recognized frame returns its own result so one invalid command cannot
    /// hide successful effects of earlier frames in the same TCP-style chunk.
    pub fn receive(
        &mut self,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<Vec<Result<(), SimError>>, SimError> {
        self.advance(now_ms)?;
        Ok(self
            .decoder
            .feed(bytes)
            .into_iter()
            .map(|frame| self.apply(frame, now_ms))
            .collect())
    }
    fn apply(&mut self, frame: RawFrame, now_ms: u64) -> Result<(), SimError> {
        let address = BusAddress::new(frame.address).map_err(|_| SimError::UnknownModule)?;
        let module = self.modules.get(&address).ok_or(SimError::UnknownModule)?;
        if module.profile()==homesense_core::ProtocolProfile::Ts686Rs485SixKey {
            let supported=match (frame.command,frame.value) {
                (0xC1|0xC5|0xC7,0)=>true,
                (0xC2,1|2)|(0xC6,1..=3)=>true,
                (0xC8,2)=>module.led_mode()==homesense_core::PanelLedMode::Manual,
                (0xC8,1)=>module.led_mode()==homesense_core::PanelLedMode::SelfReset,
                _=>false,
            };
            if supported { return Ok(()); } // No undocumented parameter readback.
        }
        if frame.command == 0 {
            let expected = hb_protocol::encode_module_all_off(module, ExecutionContext::Simulation)
                .map_err(|_| SimError::UnsupportedCommand)?;
            if frame.bytes() != expected.bytes() {
                return Err(SimError::UnsupportedCommand);
            }
            // One received module frame changes all eight simulated outputs.
            // Reuse the per-output model for feedback faults and restore memory.
            let channels = module.channels();
            for channel in 1..=channels {
                self.apply(RawFrame { command: channel, ..frame }, now_ms)?;
            }
            return Ok(());
        }
        if frame.command == 0xA0 && frame.value == 0xF2 {
            if !hb_protocol::supports_module_query(module.profile()) {
                return Err(SimError::UnsupportedCommand);
            }
            let channels = module.channels();
            for channel in 1..=channels {
                self.apply(RawFrame { command: 0xA0 + channel, ..frame }, now_ms)?;
            }
            return Ok(());
        }
        let (channel, query, ramp, pause, toggle) = match frame.command {
            c @ 1..=8 => (c, false, false, false, false),
            c @ 0xA1..=0xA8 if frame.value == 0xF2 => (c - 0xA0, true, false, false, false),
            c @ 0xA1..=0xA8 if frame.value == 0xF1 => (c - 0xA0, false, false, false, true),
            c @ 0xA1..=0xA8 if frame.value == 5 => (c - 0xA0, false, false, true, false),
            c @ 0xB1..=0xB8 => (c - 0xB0, false, true, false, false),
            _ => return Err(SimError::UnsupportedCommand),
        };
        if channel > module.channels() {
            return Err(SimError::UnsupportedCommand);
        }
        let key = (frame.address, channel);
        match module.kind() {
            ModuleKind::Switch | ModuleKind::Dimmer => {
                if (ramp || pause) && module.kind() != ModuleKind::Dimmer {
                    return Err(SimError::UnsupportedCommand);
                }
                if pause {
                    return Ok(());
                }
                let is_switch = module.kind() == ModuleKind::Switch;
                let value = if query {
                    *self.values.get(&key).unwrap_or(&0)
                } else if toggle {
                    if self.values.get(&key).copied().unwrap_or(0) != 0 {
                        0
                    } else if is_switch {
                        1
                    } else {
                        self.toggle_restore.get(&key).copied().unwrap_or(100)
                    }
                } else if is_switch {
                    match frame.value {
                        0xF1 => 1,
                        0xF2 => 0,
                        _ => return Err(SimError::InvalidValue),
                    }
                } else {
                    if frame.value > 100 {
                        return Err(SimError::InvalidValue);
                    }
                    frame.value
                };
                if !query && !is_switch && value > 0 {
                    self.toggle_restore.insert(key, value);
                }
                self.values.insert(key, value);
                self.schedule(
                    RawFrame {
                        address: frame.address,
                        command: 0xD0 + channel,
                        value: if is_switch {
                            if value == 1 { 0xF1 } else { 0xF2 }
                        } else {
                            value
                        },
                    },
                    now_ms,
                )?;
            }
            ModuleKind::Motor => {
                if query || ramp || pause || toggle {
                    return Err(SimError::UnsupportedCommand);
                }
                let motion = match frame.value {
                    0xF0 => MotorMotion::Stop,
                    0xF1 => MotorMotion::Up,
                    0xF2 => MotorMotion::Down,
                    _ => return Err(SimError::InvalidValue),
                };
                self.motors.insert(key, motion); // No position or ACK is fabricated.
            }
            ModuleKind::Keypad => {
                if query || ramp || pause || toggle || module.led_mode() != PanelLedMode::Manual {
                    return Err(SimError::UnsupportedCommand);
                }
                if ![0xF0, 0xF1, 0xF2].contains(&frame.value) {
                    return Err(SimError::InvalidValue);
                }
                self.values.insert(key, frame.value); // LED writes do not synthesize button events.
            }
        }
        Ok(())
    }
    pub fn button(
        &mut self,
        address: BusAddress,
        channel: Channel,
        pressed: bool,
        now_ms: u64,
    ) -> Result<(), SimError> {
        self.advance(now_ms)?;
        let module = self.modules.get(&address).ok_or(SimError::UnknownModule)?;
        if module.kind() != ModuleKind::Keypad || channel.get() > module.channels() {
            return Err(SimError::UnsupportedCommand);
        }
        self.schedule(
            RawFrame {
                address: address.get(),
                command: 0xD0 + channel.get(),
                value: if pressed { 0xF1 } else { 0xF2 },
            },
            now_ms,
        )
    }
    pub fn drain_due(&mut self, now_ms: u64) -> Result<Vec<Vec<u8>>, SimError> {
        self.advance(now_ms)?;
        let mut bytes = Vec::new();
        while self.pending.front().is_some_and(|(due, _)| *due <= now_ms) {
            bytes.extend(self.pending.pop_front().unwrap().1.bytes());
        }
        Ok(bytes
            .chunks(self.faults.output_chunk_size)
            .map(|chunk| chunk.to_vec())
            .collect())
    }
    /// Disconnect discards pending network bytes, not simulated physical state.
    pub fn disconnect(&mut self) {
        self.decoder.reset();
        self.pending.clear();
    }
}

#[cfg(test)]
mod native_toggle_tests {
    use super::*;
    use homesense_core::{ProtocolProfile, Verification};
    fn gateway(profile: ProtocolProfile) -> Gateway {
        Gateway::new(vec![ModuleSpec::new(BusAddress::new(0x15).unwrap(), profile,
            Verification::Simulated).unwrap()], Faults::default(), 8).unwrap()
    }
    fn run(gateway: &mut Gateway, command: u8, value: u8, at: u64) -> Vec<u8> {
        assert_eq!(gateway.receive(&[0xae, 0x15, command, value, 0xee], at).unwrap(), vec![Ok(())]);
        gateway.drain_due(at).unwrap().into_iter().flatten().collect()
    }
    #[test]
    fn relay_toggle_then_query_returns_actual_value_without_repeating_the_flip() {
        let mut gateway = gateway(ProtocolProfile::R8i8DinPro);
        assert_eq!(run(&mut gateway, 0xa1, 0xf1, 0), [0xae, 0x15, 0xd1, 0xf1, 0xee]);
        assert_eq!(run(&mut gateway, 0xa1, 0xf2, 1), [0xae, 0x15, 0xd1, 0xf1, 0xee]);
        assert_eq!(run(&mut gateway, 0xa1, 0xf1, 2), [0xae, 0x15, 0xd1, 0xf2, 0xee]);
        gateway.disconnect();
        assert_eq!(run(&mut gateway, 0xa1, 0xf2, 3), [0xae, 0x15, 0xd1, 0xf2, 0xee]);
    }
    #[test]
    fn dimmer_toggle_uses_the_documented_synthetic_restore_policy_only() {
        let mut gateway = gateway(ProtocolProfile::Ad8Din);
        assert_eq!(run(&mut gateway, 0xb1, 17, 0), [0xae, 0x15, 0xd1, 17, 0xee]);
        assert_eq!(run(&mut gateway, 0xa1, 0xf1, 1), [0xae, 0x15, 0xd1, 0, 0xee]);
        assert_eq!(run(&mut gateway, 0xa1, 0xf2, 2), [0xae, 0x15, 0xd1, 0, 0xee]);
        assert_eq!(run(&mut gateway, 0xa1, 0xf1, 3), [0xae, 0x15, 0xd1, 17, 0xee]);
    }
    #[test]
    fn toggle_never_becomes_motor_motion_or_a_panel_led_command() {
        for profile in [ProtocolProfile::C4DinPro, ProtocolProfile::Bt86cRs485] {
            let mut gateway = gateway(profile);
            assert_eq!(gateway.receive(&[0xae, 0x15, 0xa1, 0xf1, 0xee], 0).unwrap(),
                vec![Err(SimError::UnsupportedCommand)]);
            assert!(gateway.drain_due(0).unwrap().is_empty());
        }
    }
}
