//! Explicit commissioning queries whose replies have no address or checksum.
//! Even a documented one-byte value is only a candidate response, never proof
//! of causality or the current per-key LED color. Does not alter configuration.
use homesense_core::{BusAddress, ProtocolProfile};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub enum Parameter {
    Backlight,
    LedSelfReset,
    MotorInterlock,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Value {
    DimAfterTenSeconds,
    AlwaysBright,
    AlwaysDim,
    SelfReset(bool),
    InterlockSeconds(u8),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Candidate(Value),
    Unknown(u8),
    Ambiguous,
    NoReply,
}
pub struct Probe {
    address: BusAddress,
    parameter: Parameter,
}
impl Probe {
    pub fn new(
        address: BusAddress,
        profile: ProtocolProfile,
        parameter: Parameter,
    ) -> Result<Self, &'static str> {
        if !matches!(
            (profile, parameter),
            (
                ProtocolProfile::Ts686Rs485SixKey,
                Parameter::Backlight | Parameter::LedSelfReset
            ) | (ProtocolProfile::C4DinPro, Parameter::MotorInterlock)
        ) {
            return Err("parameter unsupported for this confirmed model");
        }
        Ok(Self { address, parameter })
    }
    pub fn bytes(&self) -> [u8; 5] {
        [
            0xAE,
            self.address.get(),
            match self.parameter {
                Parameter::Backlight => 0xC5,
                Parameter::LedSelfReset => 0xC7,
                Parameter::MotorInterlock => 0xC1,
            },
            0,
            0xEE,
        ]
    }
    pub fn interpret(&self, bytes: &[u8]) -> Reply {
        let value = match bytes {
            [] => return Reply::NoReply,
            [v] => *v,
            _ => return Reply::Ambiguous,
        };
        let decoded = match (self.parameter, value) {
            (Parameter::Backlight, 1) => Value::DimAfterTenSeconds,
            (Parameter::Backlight, 2) => Value::AlwaysBright,
            (Parameter::Backlight, 3) => Value::AlwaysDim,
            (Parameter::LedSelfReset, 1) => Value::SelfReset(true),
            (Parameter::LedSelfReset, 2) => Value::SelfReset(false),
            (Parameter::MotorInterlock, 1..=180) => Value::InterlockSeconds(value),
            (Parameter::MotorInterlock, 255) => Value::InterlockSeconds(180),
            _ => return Reply::Unknown(value),
        };
        Reply::Candidate(decoded)
    }
    /// Dedicated fresh socket with bounded read/write timeouts required. Capture
    /// the complete window: do not mistake a leading byte of a bus frame for a
    /// parameter. A send error terminates immediately, without retry/replay.
    pub fn run(
        &self,
        stream: &mut (impl Read + Write),
        timeout: Duration,
    ) -> io::Result<(Vec<u8>, Reply)> {
        if timeout.is_zero() || timeout > Duration::from_secs(5) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let start = Instant::now();
        let request = self.bytes();
        let mut offset = 0;
        while offset < request.len() {
            if start.elapsed() >= timeout {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let count = stream.write(&request[offset..])?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            offset += count;
        }
        let mut raw = Vec::new();
        let mut buffer = [0; 256];
        while start.elapsed() < timeout {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&buffer[..n]);
                    if raw.len() >= 4096 {
                        return Ok((raw, Reply::Ambiguous));
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) => return Err(e),
            }
        }
        let reply = self.interpret(&raw);
        Ok((raw, reply))
    }
}
