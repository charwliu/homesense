//! Bounded synchronous transport primitive for a single gateway owner.
//! No reconnect or replay. Runtime admission, configuration barriers and durable
//! acceptance are separate requirements before calling `send`.
use hb_protocol::{ProtocolError, RawFrame, StreamDecoder, encode_in_context};
use homesense_core::{Channel, Command, ExecutionContext, ModuleSpec};
use std::io::{self, Read, Write};

/// An encoded capability-checked frame. Raw bytes cannot bypass the encoder.
pub struct PreparedFrame {
    bytes: [u8; 5],
    transport_epoch: u64,
    context: ExecutionContext,
}
impl PreparedFrame {
    pub fn prepare(
        module: &ModuleSpec,
        channel: Channel,
        command: Command,
        context: ExecutionContext,
        transport_epoch: u64,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            bytes: encode_in_context(module, channel, command, context)?.bytes(),
            transport_epoch,
            context,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    Disconnected,
    StaleEpoch,
    SimulationOnly,
    Expired,
    Io(io::ErrorKind),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendError {
    pub reason: SendFailure,
    /// Bytes accepted by this local stream, not proof of device receipt.
    pub bytes_written: usize,
}
#[derive(Debug)]
pub enum Receive {
    Frames {
        transport_epoch: u64,
        frames: Vec<RawFrame>,
    },
    Idle,
    Disconnected {
        discarded_partial_bytes: usize,
    },
}

/// S must implement bounded I/O (timeouts or nonblocking mode). The logical
/// deadline cannot interrupt a blocking Read/Write supplied by the caller.
pub struct Connection<S> {
    stream: Option<S>,
    epoch: u64,
    decoder: StreamDecoder,
    context: ExecutionContext,
}
impl Connection<std::net::TcpStream> {
    /// Explicit connection only; callers must first verify the gateway permits
    /// multiple clients so this connection cannot displace the existing EA-5.
    pub fn connect(
        address: std::net::SocketAddr,
        epoch: u64,
        connect_timeout: std::time::Duration,
        io_timeout: std::time::Duration,
    ) -> io::Result<Self> {
        if connect_timeout.is_zero() || io_timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "timeouts must be nonzero",
            ));
        }
        let stream = std::net::TcpStream::connect_timeout(&address, connect_timeout)?;
        stream.set_read_timeout(Some(io_timeout))?;
        stream.set_write_timeout(Some(io_timeout))?;
        stream.set_nodelay(true)?;
        Ok(Self::new(stream, epoch))
    }
}
impl<S: Read + Write> Connection<S> {
    pub fn new(stream: S, epoch: u64) -> Self {
        Self {
            stream: Some(stream),
            epoch,
            decoder: StreamDecoder::default(),
            context: ExecutionContext::Live,
        }
    }
    /// Explicit offline fixture path; never use with a physical gateway stream.
    pub fn for_simulation(stream: S, epoch: u64) -> Self {
        let mut connection = Self::new(stream, epoch);
        connection.context = ExecutionContext::Simulation;
        connection
    }
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn disconnect(&mut self) -> usize {
        self.stream.take();
        self.decoder.reset()
    }
    /// One complete frame or a terminal connection failure. Partial frames are
    /// never resumed on another connection. The owner assigns a new epoch there.
    /// `now` is the same monotonic millisecond clock used for queue deadlines.
    pub fn send(
        &mut self,
        frame: &PreparedFrame,
        deadline_ms: u64,
        mut now: impl FnMut() -> u64,
    ) -> Result<(), SendError> {
        if !self.is_connected() {
            return Err(SendError {
                reason: SendFailure::Disconnected,
                bytes_written: 0,
            });
        }
        if frame.transport_epoch != self.epoch {
            return Err(SendError {
                reason: SendFailure::StaleEpoch,
                bytes_written: 0,
            });
        }
        if self.context == ExecutionContext::Live && frame.context == ExecutionContext::Simulation {
            return Err(SendError {
                reason: SendFailure::SimulationOnly,
                bytes_written: 0,
            });
        }
        let mut written = 0;
        let mut interruptions = 0;
        while written < frame.bytes.len() {
            if now() >= deadline_ms {
                if written > 0 {
                    self.disconnect();
                }
                return Err(SendError {
                    reason: SendFailure::Expired,
                    bytes_written: written,
                });
            }
            match self.stream.as_mut().unwrap().write(&frame.bytes[written..]) {
                Ok(0) => {
                    self.disconnect();
                    return Err(SendError {
                        reason: SendFailure::Io(io::ErrorKind::WriteZero),
                        bytes_written: written,
                    });
                }
                Ok(n) => {
                    written += n;
                    interruptions = 0;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted && interruptions < 16 => {
                    interruptions += 1;
                }
                Err(e) => {
                    self.disconnect();
                    return Err(SendError {
                        reason: SendFailure::Io(e.kind()),
                        bytes_written: written,
                    });
                }
            }
        }
        // No flush: this adapter requires an unbuffered stream (e.g. TcpStream).
        Ok(())
    }
    /// At most one bounded read and 512 bytes per call. Idle keeps partial input;
    /// EOF or errors discard it. Returned frames still require registry decoding.
    pub fn receive(&mut self) -> io::Result<Receive> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(Receive::Disconnected {
                discarded_partial_bytes: 0,
            });
        };
        let mut buffer = [0; 512];
        match stream.read(&mut buffer) {
            Ok(0) => Ok(Receive::Disconnected {
                discarded_partial_bytes: self.disconnect(),
            }),
            Ok(n) => Ok(Receive::Frames {
                transport_epoch: self.epoch,
                frames: self.decoder.feed(&buffer[..n]),
            }),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(Receive::Idle)
            }
            Err(e) => {
                self.disconnect();
                Err(e)
            }
        }
    }
}
