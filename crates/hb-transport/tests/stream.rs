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
#[test]
fn ts6_settings_are_three_complete_frames_but_other_operations_stay_single() {
    use homesense_core::{
        PanelBacklight as B, PanelFeedbackMode as M, PanelLedMode, PanelParameter as P,
    };
    for (parameter, command, value, mode, copies) in [
        (
            P::Backlight(B::DimAfterTenSeconds),
            0xc6,
            1,
            PanelLedMode::Manual,
            3,
        ),
        (P::Backlight(B::Bright), 0xc6, 2, PanelLedMode::Manual, 3),
        (P::Backlight(B::Dim), 0xc6, 3, PanelLedMode::Manual, 3),
        (P::FeedbackMode(M::Manual), 0xc8, 2, PanelLedMode::Manual, 3),
        (
            P::FeedbackMode(M::SelfReset),
            0xc8,
            1,
            PanelLedMode::SelfReset,
            3,
        ),
        (P::ReadBacklight, 0xc5, 0, PanelLedMode::Manual, 1),
        (P::ReadFeedbackMode, 0xc7, 0, PanelLedMode::Manual, 1),
        (P::Sound(false), 0xc2, 2, PanelLedMode::Manual, 1),
    ] {
        let module = ModuleSpec::new(
            BusAddress::new(0x41).unwrap(),
            ProtocolProfile::Ts686Rs485SixKey,
            Verification::Simulated,
        )
        .unwrap()
        .with_panel_led_mode(mode)
        .unwrap();
        let prepared = PreparedFrame::prepare(
            &module,
            Channel::new(1).unwrap(),
            Command::PanelParameter(parameter),
            ExecutionContext::Simulation,
            1,
        )
        .unwrap();
        let s = stream(99);
        let bytes = s.writes.clone();
        let mut c = Connection::for_simulation(s, 1);
        c.send(&prepared, 100, || 0).unwrap();
        let bytes = bytes.lock().unwrap().clone();
        assert_eq!(bytes, [0xae, 0x41, command, value, 0xee].repeat(copies));
        for chunk in 1..=bytes.len() {
            let mut decoder = hb_protocol::StreamDecoder::default();
            let frames = bytes
                .chunks(chunk)
                .flat_map(|b| decoder.feed(b))
                .collect::<Vec<_>>();
            assert_eq!(frames.len(), copies);
            assert!(
                frames
                    .iter()
                    .all(|f| f.bytes() == [0xae, 0x41, command, value, 0xee])
            );
            assert_eq!(decoder.buffered_len(), 0);
        }
    }
    let module = ModuleSpec::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Ts686Rs485SixKey,
        Verification::Simulated,
    )
    .unwrap()
    .with_panel_led_mode(PanelLedMode::Manual)
    .unwrap();
    let prepared = PreparedFrame::prepare(
        &module,
        Channel::new(1).unwrap(),
        Command::PanelLed(true),
        ExecutionContext::Simulation,
        1,
    )
    .unwrap();
    let s = stream(99);
    let bytes = s.writes.clone();
    Connection::for_simulation(s, 1)
        .send(&prepared, 100, || 0)
        .unwrap();
    assert_eq!(*bytes.lock().unwrap(), [0xae, 0x41, 1, 0xf1, 0xee]);
    for profile in [ProtocolProfile::R8i8DinPro, ProtocolProfile::C4DinPro] {
        let module = ModuleSpec::new(
            BusAddress::new(3).unwrap(),
            profile,
            Verification::Simulated,
        )
        .unwrap();
        assert!(
            PreparedFrame::prepare(
                &module,
                Channel::new(1).unwrap(),
                Command::PanelParameter(P::Backlight(B::Bright)),
                ExecutionContext::Simulation,
                1
            )
            .is_err()
        );
    }
}
#[test]
fn ts6_partial_setting_never_succeeds_or_replays_and_original_deadline_is_retained() {
    use homesense_core::{PanelBacklight, PanelLedMode, PanelParameter};
    let module = ModuleSpec::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Ts686Rs485SixKey,
        Verification::Simulated,
    )
    .unwrap()
    .with_panel_led_mode(PanelLedMode::Manual)
    .unwrap();
    let prepared = PreparedFrame::prepare(
        &module,
        Channel::new(1).unwrap(),
        Command::PanelParameter(PanelParameter::Backlight(PanelBacklight::Bright)),
        ExecutionContext::Simulation,
        7,
    )
    .unwrap();
    for fail in 0..=15 {
        let s = stream(fail);
        let bytes = s.writes.clone();
        let mut c = Connection::for_simulation(s, 7);
        let result = c.send(&prepared, 100, || 0);
        assert_eq!(result.is_ok(), fail == 15);
        assert_eq!(bytes.lock().unwrap().len(), fail);
        if fail < 15 {
            assert_eq!(result.unwrap_err().bytes_written, fail);
            assert_eq!(
                c.send(&prepared, 100, || 0).unwrap_err().reason,
                SendFailure::Disconnected
            );
            assert_eq!(bytes.lock().unwrap().len(), fail);
        }
    }
    let s = stream(99);
    let bytes = s.writes.clone();
    let mut c = Connection::for_simulation(s, 7);
    let mut ticks = 0;
    let error = c
        .send(&prepared, 4, || {
            ticks += 1;
            ticks
        })
        .unwrap_err();
    assert_eq!(error.reason, SendFailure::Expired);
    assert_eq!(error.bytes_written, 6);
    assert_eq!(bytes.lock().unwrap().len(), 6);
    assert!(!c.is_connected());
}

#[test]
fn batch_uses_one_write_for_complete_phases_and_counts_physical_frames() {
    struct Counting(Arc<Mutex<Vec<usize>>>);
    impl Read for Counting {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    impl Write for Counting {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().push(b.len());
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("no buffered flush")
        }
    }
    for count in [2, 3, 4, 10, 13, 32] {
        let frames = (0..count).map(|_| frame(7)).collect::<Vec<_>>();
        let refs = frames.iter().collect::<Vec<_>>();
        let calls = Arc::default();
        let mut connection = Connection::for_simulation(Counting(Arc::clone(&calls)), 7);
        connection.send_batch(&refs, 100, || 0).unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![count * 5]);
    }
    let frames = (0..33).map(|_| frame(7)).collect::<Vec<_>>();
    let calls = Arc::default();
    let mut connection = Connection::for_simulation(Counting(Arc::clone(&calls)), 7);
    assert_eq!(
        connection
            .send_batch(&frames.iter().collect::<Vec<_>>(), 100, || 0)
            .unwrap_err()
            .bytes_written,
        0
    );
    assert!(calls.lock().unwrap().is_empty());
}
#[test]
fn batch_short_writes_map_complete_partial_and_unsent_operations_without_replay() {
    use hb_transport::{OperationProgress::*, operation_progress};
    use homesense_core::{PanelBacklight, PanelParameter};
    let panel = ModuleSpec::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Ts686Rs485SixKey,
        Verification::Simulated,
    )
    .unwrap();
    let setting = PreparedFrame::prepare(
        &panel,
        Channel::new(1).unwrap(),
        Command::PanelParameter(PanelParameter::Backlight(
            PanelBacklight::DimAfterTenSeconds,
        )),
        ExecutionContext::Simulation,
        7,
    )
    .unwrap();
    let simple = frame(7);
    let frames = [&simple, &setting, &simple];
    assert_eq!(setting.encoded_len(), 15);
    for amount in 0..=25 {
        let s = stream(amount);
        let bytes = s.writes.clone();
        let mut connection = Connection::for_simulation(s, 7);
        let outcome = connection.send_batch(&frames, 100, || 0);
        let progress = operation_progress(&frames, amount).unwrap();
        assert_eq!(
            progress[0],
            if amount >= 5 {
                Complete
            } else if amount > 0 {
                Partial
            } else {
                Unsent
            }
        );
        assert_eq!(
            progress[1],
            if amount >= 20 {
                Complete
            } else if amount > 5 {
                Partial
            } else {
                Unsent
            }
        );
        assert_eq!(
            progress[2],
            if amount == 25 {
                Complete
            } else if amount > 20 {
                Partial
            } else {
                Unsent
            }
        );
        if amount < 25 {
            assert_eq!(outcome.unwrap_err().bytes_written, amount);
            assert_eq!(
                connection
                    .send_batch(&frames, 100, || 0)
                    .unwrap_err()
                    .reason,
                SendFailure::Disconnected
            );
        } else {
            outcome.unwrap();
        }
        assert_eq!(bytes.lock().unwrap().len(), amount);
    }
    assert!(operation_progress(&frames, 26).is_none());
    let s = stream(99);
    let bytes = s.writes.clone();
    let mut connection = Connection::for_simulation(s, 7);
    assert_eq!(
        connection
            .send_batch(&[&simple, &frame(8)], 100, || 0)
            .unwrap_err()
            .reason,
        SendFailure::StaleEpoch
    );
    assert!(bytes.lock().unwrap().is_empty());
}
