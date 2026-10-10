use hb_transport::{Connection, PreparedFrame, Receive, SendFailure};
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, ModuleSpec, ProtocolProfile, Verification,
};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};

struct FaultStream {
    reads: VecDeque<io::Result<Vec<u8>>>,
    writes: Arc<Mutex<Vec<u8>>>,
    fail_after: usize,
}
impl Read for FaultStream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let data = self.reads.pop_front().unwrap_or(Ok(vec![]))?;
        b[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }
}
impl Write for FaultStream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let mut written = self.writes.lock().unwrap();
        if written.len() >= self.fail_after {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = b.len().min(2).min(self.fail_after - written.len());
        written.extend_from_slice(&b[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("unbuffered transport must not flush");
    }
}
fn frame(epoch: u64) -> PreparedFrame {
    let module = ModuleSpec::new(
        BusAddress::new(3).unwrap(),
        ProtocolProfile::GtC8Rl,
        Verification::Simulated,
    )
    .unwrap();
    PreparedFrame::prepare(
        &module,
        Channel::new(1).unwrap(),
        Command::Switch(true),
        ExecutionContext::Simulation,
        epoch,
    )
    .unwrap()
}
fn stream(fail_after: usize) -> FaultStream {
    FaultStream {
        reads: VecDeque::new(),
        writes: Arc::default(),
        fail_after,
    }
}
#[test]
fn short_writes_complete_but_partial_failures_disconnect_without_replay() {
    for fail_after in [0, 2, 4, 5] {
        let s = stream(fail_after);
        let bytes = s.writes.clone();
        let mut connection = Connection::for_simulation(s, 7);
        let result = connection.send(&frame(7), 100, || 0);
        if fail_after == 5 {
            assert!(result.is_ok());
            assert!(connection.is_connected());
        } else {
            assert_eq!(result.unwrap_err().bytes_written, fail_after);
            assert!(!connection.is_connected());
            assert_eq!(
                connection.send(&frame(7), 100, || 0).unwrap_err().reason,
                SendFailure::Disconnected
            );
        }
        assert_eq!(bytes.lock().unwrap().len(), fail_after);
    }
}
#[test]
fn stale_and_expired_frames_never_write_and_mid_frame_expiration_closes_stream() {
    let s = stream(99);
    let bytes = s.writes.clone();
    let mut c = Connection::for_simulation(s, 2);
    assert_eq!(
        c.send(&frame(1), 100, || 0).unwrap_err().reason,
        SendFailure::StaleEpoch
    );
    assert_eq!(
        c.send(&frame(2), 100, || 100).unwrap_err().reason,
        SendFailure::Expired
    );
    assert!(bytes.lock().unwrap().is_empty());
    assert!(c.is_connected());
    let mut time = 0;
    let e = c
        .send(&frame(2), 2, || {
            time += 1;
            time
        })
        .unwrap_err();
    assert_eq!(e.bytes_written, 2);
    assert!(!c.is_connected());
}
#[test]
fn read_timeout_preserves_fragment_but_eof_discards_it() {
    let mut s = stream(99);
    s.reads = VecDeque::from([
        Ok(vec![0xAE, 3]),
        Err(io::ErrorKind::TimedOut.into()),
        Ok(vec![0xD1, 0xF1, 0xEE, 0xAE, 3]),
        Ok(vec![]),
    ]);
    let mut c = Connection::for_simulation(s, 8);
    assert!(matches!(c.receive().unwrap(), Receive::Frames { frames, .. } if frames.is_empty()));
    assert!(matches!(c.receive().unwrap(), Receive::Idle));
    assert!(
        matches!(c.receive().unwrap(), Receive::Frames { transport_epoch: 8, frames } if frames.len() == 1)
    );
    assert!(matches!(
        c.receive().unwrap(),
        Receive::Disconnected {
            discarded_partial_bytes: 2
        }
    ));
    assert!(!c.is_connected());
}

#[test]
fn live_transport_rejects_simulation_frame_before_writing() {
    let s = stream(99);
    let bytes = s.writes.clone();
    let mut c = Connection::new(s, 1);
    assert_eq!(
        c.send(&frame(1), 100, || 0).unwrap_err().reason,
        SendFailure::SimulationOnly
    );
    assert!(bytes.lock().unwrap().is_empty());
}
