use homesense_core::{BusAddress, ProtocolProfile};
use homesensed::parameter_probe::{Parameter, Probe, Reply, Value};
use std::{
    io::{self, Read, Write},
    time::Duration,
};
fn panel(parameter: Parameter) -> Probe {
    Probe::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Ts686Rs485SixKey,
        parameter,
    )
    .unwrap()
}
#[test]
fn field_values_preserve_unknown_backlight_and_do_not_claim_led_colors() {
    let records: hap::serde_json::Value = hap::serde_json::from_str(include_str!(
        "../../../deployment/nanopc/evidence/20261010-a3/panel-41-modes.json"
    ))
    .unwrap();
    assert_eq!(records[0]["response"], "FF");
    assert_eq!(records[1]["response"], "02");
    assert_eq!(
        panel(Parameter::Backlight).interpret(&[255]),
        Reply::Unknown(255)
    );
    assert_eq!(
        panel(Parameter::LedSelfReset).interpret(&[2]),
        Reply::Candidate(Value::SelfReset(false))
    );
    let motor = Probe::new(
        BusAddress::new(0x22).unwrap(),
        ProtocolProfile::C4DinPro,
        Parameter::MotorInterlock,
    )
    .unwrap();
    assert_eq!(motor.bytes(), [0xAE, 0x22, 0xC1, 0, 0xEE]);
    assert_eq!(
        motor.interpret(&[180]),
        Reply::Candidate(Value::InterlockSeconds(180))
    );
    assert_eq!(
        motor.interpret(&[255]),
        Reply::Candidate(Value::InterlockSeconds(180))
    );
    assert_eq!(motor.interpret(&[0]), Reply::Unknown(0));
    assert!(
        Probe::new(
            BusAddress::new(3).unwrap(),
            ProtocolProfile::R8i8DinPro,
            Parameter::Backlight
        )
        .is_err()
    );
}
struct Stream {
    input: io::Cursor<Vec<u8>>,
    output: Vec<u8>,
    fail: bool,
}
impl Read for Stream {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        self.input.read(&mut data[..1])
    }
}
impl Write for Stream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fail && !self.output.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.output.push(data[0]);
        Ok(1)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("no buffered output")
    }
}
#[test]
fn full_response_window_rejects_interleaved_or_duplicate_bytes() {
    for input in [
        vec![],
        vec![2],
        vec![2, 2],
        vec![0xAE, 0x41, 0xD1, 0xF1, 0xEE, 2],
    ] {
        let mut stream = Stream {
            input: io::Cursor::new(input.clone()),
            output: vec![],
            fail: false,
        };
        let probe = panel(Parameter::Backlight);
        let (raw, reply) = probe.run(&mut stream, Duration::from_secs(1)).unwrap();
        assert_eq!(raw, input);
        assert_eq!(
            reply,
            match input.len() {
                0 => Reply::NoReply,
                1 => Reply::Candidate(Value::AlwaysBright),
                _ => Reply::Ambiguous,
            }
        );
        assert_eq!(stream.output, [0xAE, 0x41, 0xC5, 0, 0xEE]);
    }
}
#[test]
fn partial_query_is_not_replayed_after_write_failure() {
    let mut stream = Stream {
        input: io::Cursor::new(vec![2]),
        output: vec![],
        fail: true,
    };
    assert!(
        panel(Parameter::LedSelfReset)
            .run(&mut stream, Duration::from_secs(1))
            .is_err()
    );
    assert_eq!(stream.output, [0xAE]);
    assert_eq!(stream.input.position(), 0);
}
