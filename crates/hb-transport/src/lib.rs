//! Bounded synchronous transport primitive for a single gateway owner.
//! No reconnect or replay. Runtime admission, configuration barriers and durable
//! acceptance are separate requirements before calling `send`.
use hb_protocol::{ProtocolError, RawFrame, StreamDecoder, encode_in_context};
use homesense_core::{Channel, Command, ExecutionContext, ModuleSpec};
use std::io::{self, Read, Write};

/// A bounded write never exceeds 32 physical protocol frames (160 bytes).
pub const MAX_BATCH_FRAMES: usize = 32;
pub const MAX_BATCH_BYTES: usize = MAX_BATCH_FRAMES * 5;

/// An encoded capability-checked frame. Raw bytes cannot bypass the encoder.
pub struct PreparedFrame {
    bytes: [u8; 15],
    length: usize,
    transport_epoch: u64,
    context: ExecutionContext,
}
impl PreparedFrame {
    /// Actual complete five-byte protocol frames held by this logical operation.
    pub fn physical_frames(&self) -> usize {
        self.length / 5
    }
    pub fn encoded_len(&self) -> usize {
        self.length
    }

    fn operation(
        bytes: [u8; 5],
        copies: usize,
        context: ExecutionContext,
        transport_epoch: u64,
    ) -> Self {
        let mut operation = [0; 15];
        for chunk in operation[..copies * 5].as_chunks_mut::<5>().0 {
            chunk.copy_from_slice(&bytes);
        }
        Self {
            bytes: operation,
            length: copies * 5,
            transport_epoch,
            context,
        }
    }
    /// Encodes one addressed module operation. The runtime must reserve and
    /// authorize the entire affected channel set before handing it to send.
    pub fn prepare_module_all_off(
        module: &ModuleSpec,
        context: ExecutionContext,
        transport_epoch: u64,
    ) -> Result<Self, ProtocolError> {
        Ok(Self::operation(
            hb_protocol::encode_module_all_off(module, context)?.bytes(),
            1,
            context,
            transport_epoch,
        ))
    }

    pub fn prepare_module_query(
        module: &ModuleSpec,
        context: ExecutionContext,
        transport_epoch: u64,
    ) -> Result<Self, ProtocolError> {
        Ok(Self::operation(
            hb_protocol::encode_module_query(module, context)?.bytes(),
            1,
            context,
            transport_epoch,
        ))
    }

    pub fn prepare(
        module: &ModuleSpec,
        channel: Channel,
        command: Command,
        context: ExecutionContext,
        transport_epoch: u64,
    ) -> Result<Self, ProtocolError> {
        let frame = encode_in_context(module, channel, command, context)?.bytes();
        // TS6 C6/C8 settings require three complete consecutive frames. Keep
        // one logical ticket, one writer lease and one final completion audit.
        let copies = if module.profile() == homesense_core::ProtocolProfile::Ts686Rs485SixKey
            && matches!(
                command,
                Command::PanelParameter(
                    homesense_core::PanelParameter::Backlight(_)
                        | homesense_core::PanelParameter::FeedbackMode(_)
                )
            ) {
            3
        } else {
            1
        };
        Ok(Self::operation(frame, copies, context, transport_epoch))
    }
}

/// Local stream progress only; Written never asserts that an actuator applied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationProgress {
    Complete,
    Partial,
    Unsent,
}
pub fn operation_progress(
    frames: &[&PreparedFrame],
    bytes_written: usize,
) -> Option<Vec<OperationProgress>> {
    let total = frames.iter().map(|frame| frame.length).sum::<usize>();
    if frames.is_empty() || total > MAX_BATCH_BYTES || bytes_written > total {
        return None;
    }
    let mut end = 0;
    Some(
        frames
            .iter()
            .map(|frame| {
                let start = end;
                end += frame.length;
                if bytes_written >= end {
                    OperationProgress::Complete
                } else if bytes_written > start {
                    OperationProgress::Partial
                } else {
                    OperationProgress::Unsent
                }
            })
            .collect(),
    )
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
        // Reads are polling operations in the single transport owner. A long idle
        // wait on each loop starves refresh and controls despite fast replies.
        // Logical query deadlines remain independent of this polling bound.
        stream.set_read_timeout(Some(io_timeout.min(std::time::Duration::from_millis(1))))?;
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
    /// One complete bounded operation or a terminal failure. TS6 settings carry
    /// three complete frames; partial operations are
    /// never resumed on another connection. The owner assigns a new epoch there.
    /// `now` is the same monotonic millisecond clock used for queue deadlines.
    pub fn send(
        &mut self,
        frame: &PreparedFrame,
        deadline_ms: u64,
        mut now: impl FnMut() -> u64,
    ) -> Result<(), SendError> {
        self.send_batch(&[frame], deadline_ms, &mut now)
    }
    /// An already admitted homogeneous writer phase. Every operation retains its
    /// own capability proof; the runtime owns request grouping and authority.
    /// No buffering, waiting to fill, retries on a fresh socket, or new deadline.
    pub fn send_batch(
        &mut self,
        frames: &[&PreparedFrame],
        deadline_ms: u64,
        mut now: impl FnMut() -> u64,
    ) -> Result<(), SendError> {
        let denied = |reason| SendError {
            reason,
            bytes_written: 0,
        };
        if !self.is_connected() {
            return Err(denied(SendFailure::Disconnected));
        }
        let length = frames.iter().map(|frame| frame.length).sum::<usize>();
        if frames.is_empty() || length > MAX_BATCH_BYTES {
            return Err(denied(SendFailure::Io(io::ErrorKind::InvalidInput)));
        }
        let mut bytes = [0u8; MAX_BATCH_BYTES];
        let mut offset = 0;
        for frame in frames {
            if frame.transport_epoch != self.epoch {
                return Err(denied(SendFailure::StaleEpoch));
            }
            if self.context == ExecutionContext::Live
                && frame.context == ExecutionContext::Simulation
            {
                return Err(denied(SendFailure::SimulationOnly));
            }
            bytes[offset..offset + frame.length].copy_from_slice(&frame.bytes[..frame.length]);
            offset += frame.length;
        }
        let mut written = 0;
        let mut interruptions = 0;
        while written < length {
            if now() >= deadline_ms {
                if written > 0 {
                    self.disconnect();
                }
                return Err(SendError {
                    reason: SendFailure::Expired,
                    bytes_written: written,
                });
            }
            match self.stream.as_mut().unwrap().write(&bytes[written..length]) {
                Ok(0) => {
                    self.disconnect();
                    return Err(SendError {
                        reason: SendFailure::Io(io::ErrorKind::WriteZero),
                        bytes_written: written,
                    });
                }
                Ok(n) if n <= length - written => {
                    written += n;
                    interruptions = 0;
                }
                Ok(_) => {
                    self.disconnect();
                    return Err(SendError {
                        reason: SendFailure::Io(io::ErrorKind::InvalidData),
                        bytes_written: written,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted && interruptions < 16 => {
                    interruptions += 1
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
