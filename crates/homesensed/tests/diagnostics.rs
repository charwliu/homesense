use hb_protocol::Registry;
use homesensed::diagnostics::capture;
use std::{
    io::{self, Read},
    time::Duration,
};
// Deliberately has no Write implementation.
struct Input(io::Cursor<Vec<u8>>);
impl Read for Input {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes.len().min(2);
        self.0.read(&mut bytes[..count])
    }
}
#[test]
fn passive_capture_retains_split_unknown_frames_and_partial_eof() {
    let mut input = Input(io::Cursor::new(vec![
        0, 0xAE, 0x41, 0xD1, 0xF1, 0xEE, 0xAE, 0x41,
    ]));
    let mut records = Vec::new();
    capture(
        &mut input,
        &Registry::default(),
        Duration::from_secs(1),
        10,
        |event| {
            records.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[1]["raw"],
        hap::serde_json::json!([174, 65, 209, 241, 238])
    );
    assert_eq!(records[1]["decoded"]["reason"], "UnregisteredAddress");
    assert_eq!(records[2]["reason"], "eof");
    assert_eq!(records[2]["incomplete_bytes"], 2);
    assert_eq!(records[2]["discarded_bytes"], 1);
}
#[test]
fn passive_capture_stops_at_frame_limit_and_propagates_sink_failure() {
    let bytes = [0xAE, 0x41, 0xD1, 0xF1, 0xEE].repeat(3);
    let mut records = Vec::new();
    capture(
        &mut io::Cursor::new(&bytes),
        &Registry::default(),
        Duration::from_secs(1),
        1,
        |event| {
            records.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(records[2]["reason"], "frame_limit");
    assert_eq!(records[2]["frames"], 1);
    assert!(
        capture(
            &mut io::Cursor::new(&bytes),
            &Registry::default(),
            Duration::from_secs(1),
            1,
            |_| Err(io::ErrorKind::BrokenPipe.into())
        )
        .is_err()
    );
}
