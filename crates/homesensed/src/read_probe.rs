//! Commissioning-only, single-channel query. Never enables a control binding.
use hb_protocol::StreamDecoder;
use homesense_core::{BusAddress, Channel, Observation, Percent, ProtocolProfile};
use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};

/// Only manually reviewed R8I8 and AD8 status queries are available here.
/// Constructing a probe records no verification and grants no control capability.
#[derive(Clone, Copy)]
pub struct ReadProbe {
    address: BusAddress,
    channel: Channel,
    profile: ProtocolProfile,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Matching endpoint observation, not proof that this query caused it.
    Observation(Observation),
    Timeout,
    Disconnected,
    FrameLimit,
}
impl ReadProbe {
    pub fn new(
        address: BusAddress,
        channel: Channel,
        profile: ProtocolProfile,
    ) -> Result<Self, &'static str> {
        if !matches!(
            profile,
            ProtocolProfile::R8i8DinPro | ProtocolProfile::Ad8Din
        ) || channel.get() > profile.channels()
        {
            return Err("probe supports only R8I8/AD8 single-channel status queries");
        }
        Ok(Self {
            address,
            channel,
            profile,
        })
    }
    pub fn bytes(&self) -> [u8; 5] {
        [
            0xAE,
            self.address.get(),
            0xA0 + self.channel.get(),
            0xF2,
            0xEE,
        ]
    }
    /// The stream must have bounded I/O. No reconnect, retries or parameter writes.
    /// Every received frame is reported, including unknown and unrelated frames.
    pub fn run(
        &self,
        stream: &mut (impl Read + Write),
        timeout: Duration,
        mut received: impl FnMut([u8; 5], Duration) -> io::Result<()>,
    ) -> io::Result<Outcome> {
        if timeout.is_zero() || timeout > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "probe timeout must be in (0,5s]",
            ));
        }
        let start = Instant::now();
        let bytes = self.bytes();
        let mut written = 0;
        while written < bytes.len() {
            if start.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("query expired after {written} local bytes"),
                ));
            }
            // An error, including Interrupted, terminates this attempt. A partial
            // query may have reached the gateway; it must not be replayed.
            let count = stream.write(&bytes[written..])?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            written += count;
        }
        let mut decoder = StreamDecoder::default();
        let mut buffer = [0; 512];
        let mut frames = 0;
        loop {
            if start.elapsed() >= timeout {
                return Ok(Outcome::Timeout);
            }
            match stream.read(&mut buffer) {
                Ok(0) => return Ok(Outcome::Disconnected),
                Ok(n) => {
                    for frame in decoder.feed(&buffer[..n]) {
                        frames += 1;
                        received(frame.bytes(), start.elapsed())?;
                        if start.elapsed() >= timeout {
                            return Ok(Outcome::Timeout);
                        }
                        if frame.address == self.address.get()
                            && frame.command == 0xD0 + self.channel.get()
                        {
                            let observation = match self.profile {
                                ProtocolProfile::R8i8DinPro => match frame.value {
                                    0xF1 => Some(Observation::Switch(true)),
                                    0xF2 => Some(Observation::Switch(false)),
                                    _ => None,
                                },
                                ProtocolProfile::Ad8Din => {
                                    Percent::new(frame.value).ok().map(Observation::Level)
                                }
                                _ => None,
                            };
                            if let Some(observation) = observation {
                                return Ok(Outcome::Observation(observation));
                            }
                        }
                        if frames >= 4096 {
                            return Ok(Outcome::FrameLimit);
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => return Err(error),
            }
        }
    }
}
