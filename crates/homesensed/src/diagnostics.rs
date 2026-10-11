//! Passive bus capture: the capture API accepts Read, never Write.
use hap::serde_json::{Value, json};
use hb_protocol::{Decoded, Registry, StreamDecoder};
use std::{
    io::{self, Read},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
/// The supplied reader must use bounded reads (a socket timeout or nonblocking).
/// Each output is emitted immediately; frames and logs are not retained in memory.
pub fn capture(
    reader: &mut impl Read,
    registry: &Registry,
    duration: Duration,
    max_frames: usize,
    mut emit: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    if duration.is_zero()
        || duration > Duration::from_secs(3600)
        || !(1..=65536).contains(&max_frames)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture limits out of range",
        ));
    }
    let started = Instant::now();
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    emit(
        json!({"event":"capture_started", "unix_ms":unix_ms.to_string(), "duration_ms":duration.as_millis() as u64, "max_frames":max_frames}),
    )?;
    let mut decoder = StreamDecoder::default();
    let mut count = 0;
    let mut buffer = [0; 512];
    let mut io_error = None;
    let reason = 'capture: loop {
        if started.elapsed() >= duration {
            break "duration";
        }
        match reader.read(&mut buffer) {
            Ok(0) => break "eof",
            Ok(n) => {
                for frame in decoder.feed(&buffer[..n]) {
                    let decoded = match registry.decode(frame) {
                        Decoded::Feedback {
                            address,
                            channel,
                            observation,
                        } => {
                            json!({"kind":"feedback", "address":address.get(), "channel":channel.get(), "observation":format!("{observation:?}")})
                        }
                        Decoded::Unknown { reason, .. } => {
                            json!({"kind":"unknown", "reason":format!("{reason:?}")})
                        }
                    };
                    count += 1;
                    emit(
                        json!({"event":"frame", "elapsed_ms":started.elapsed().as_millis() as u64, "raw":frame.bytes(), "decoded":decoded}),
                    )?;
                    if count == max_frames {
                        break 'capture "frame_limit";
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => {
                io_error = Some(error);
                break "io_error";
            }
        }
    };
    emit(
        json!({"event":"capture_finished", "elapsed_ms":started.elapsed().as_millis() as u64, "reason":reason, "frames":count, "discarded_bytes":decoder.discarded_bytes(), "malformed_frames":decoder.malformed_frames(), "incomplete_bytes":decoder.buffered_len(), "error":io_error.as_ref().map(ToString::to_string)}),
    )?;
    if let Some(error) = io_error {
        Err(error)
    } else {
        Ok(())
    }
}
