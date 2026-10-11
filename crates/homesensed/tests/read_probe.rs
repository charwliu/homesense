use homesense_core::{BusAddress, Channel, Observation, ProtocolProfile};
use homesensed::read_probe::{Outcome, ReadProbe};
use std::{
    io::{self, Read, Write},
    time::Duration,
};
struct Stream {
    input: io::Cursor<Vec<u8>>,
    output: Vec<u8>,
    fail: bool,
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = bytes.len().min(2);
        self.input.read(&mut bytes[..n])
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail && !self.output.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = bytes.len().min(2);
        self.output.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("unbuffered query should not flush")
    }
}
#[test]
fn query_accepts_only_matching_valid_observation_without_control_frame() {
    let probe = ReadProbe::new(
        BusAddress::from_hex("03").unwrap(),
        Channel::new(4).unwrap(),
        ProtocolProfile::R8i8DinPro,
    )
    .unwrap();
    let mut stream = Stream {
        input: io::Cursor::new(vec![
            0xAE, 0x07, 0xD4, 0xF1, 0xEE, 0xAE, 0x03, 0xD4, 0x05, 0xEE, 0xAE, 0x03, 0xD4, 0xF2,
            0xEE,
        ]),
        output: vec![],
        fail: false,
    };
    let mut frames = vec![];
    assert_eq!(
        probe
            .run(&mut stream, Duration::from_secs(1), |frame, _| {
                frames.push(frame);
                Ok(())
            })
            .unwrap(),
        Outcome::Observation(Observation::Switch(false))
    );
    assert_eq!(stream.output, [0xAE, 0x03, 0xA4, 0xF2, 0xEE]);
    assert_eq!(frames.len(), 3);
    assert!(
        ReadProbe::new(
            BusAddress::from_hex("22").unwrap(),
            Channel::new(2).unwrap(),
            ProtocolProfile::C4DinPro
        )
        .is_err()
    );
}
#[test]
fn partial_write_fails_without_replay_or_reading() {
    let probe = ReadProbe::new(
        BusAddress::from_hex("11").unwrap(),
        Channel::new(5).unwrap(),
        ProtocolProfile::Ad8Din,
    )
    .unwrap();
    let mut stream = Stream {
        input: io::Cursor::new(vec![0xAE, 0x11, 0xD5, 30, 0xEE]),
        output: vec![],
        fail: true,
    };
    assert!(
        probe
            .run(&mut stream, Duration::from_secs(1), |_, _| panic!(
                "must not read after write failure"
            ))
            .is_err()
    );
    assert_eq!(stream.output, [0xAE, 0x11]);
    assert_eq!(stream.input.position(), 0);
}

#[test]
fn idle_query_times_out_after_exactly_one_request() {
    struct Idle(Vec<u8>);
    impl Read for Idle {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    impl Write for Idle {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let probe = ReadProbe::new(
        BusAddress::from_hex("11").unwrap(),
        Channel::new(5).unwrap(),
        ProtocolProfile::Ad8Din,
    )
    .unwrap();
    let mut stream = Idle(vec![]);
    assert_eq!(
        probe
            .run(&mut stream, Duration::from_millis(2), |_, _| panic!(
                "idle stream has no frames"
            ))
            .unwrap(),
        Outcome::Timeout
    );
    assert_eq!(stream.0, [0xAE, 0x11, 0xA5, 0xF2, 0xEE]);
}
