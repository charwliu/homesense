use hb_protocol::{RawFrame, StreamDecoder};
use homesense_core::{BusAddress, Channel, ModuleSpec, MotorMotion, ProtocolProfile, Verification};
use homesense_simulator::{Faults, Gateway, SimError};
fn module(profile: ProtocolProfile) -> ModuleSpec {
    ModuleSpec::new(
        BusAddress::new(0x15).unwrap(),
        profile,
        Verification::Simulated,
    )
    .unwrap()
}
#[test]
fn delayed_fragmented_duplicate_feedback_is_generated_from_written_value() {
    let mut gateway = Gateway::new(
        vec![module(ProtocolProfile::GtD8Rl)],
        Faults {
            reply_delay_ms: 10,
            duplicate_replies: true,
            output_chunk_size: 2,
            ..Faults::default()
        },
        8,
    )
    .unwrap();
    assert!(gateway.receive(&[0xAE, 0x15], 0).unwrap().is_empty());
    assert_eq!(gateway.receive(&[3, 45, 0xEE], 1).unwrap(), [Ok(())]);
    assert!(gateway.drain_due(10).unwrap().is_empty());
    let chunks = gateway.drain_due(11).unwrap();
    let mut decoder = StreamDecoder::default();
    let frames: Vec<_> = chunks
        .iter()
        .flat_map(|chunk| decoder.feed(chunk))
        .collect();
    assert_eq!(
        frames,
        vec![
            RawFrame {
                address: 0x15,
                command: 0xD3,
                value: 45
            };
            2
        ]
    );
}
#[test]
fn lost_replies_do_not_prevent_physical_state_change_and_disconnect_drops_residual_bytes() {
    let mut gateway =
        Gateway::new(vec![module(ProtocolProfile::GtD8Rl)], Faults::default(), 1).unwrap();
    assert_eq!(
        gateway
            .receive(&[0xAE, 0x15, 1, 10, 0xEE, 0xAE, 0x15, 1, 90, 0xEE], 0)
            .unwrap(),
        [Ok(()), Ok(())]
    );
    assert_eq!(gateway.dropped_replies(), 1);
    gateway.disconnect();
    assert_eq!(
        gateway.receive(&[0xAE, 0x15, 0xA1, 0xF2, 0xEE], 1).unwrap(),
        [Ok(())]
    );
    assert_eq!(
        gateway.drain_due(1).unwrap(),
        [vec![0xAE, 0x15, 0xD1, 90, 0xEE]]
    );
    gateway.receive(&[0xAE, 0x15, 1], 2).unwrap();
    gateway.disconnect();
    assert!(gateway.receive(&[0, 0xEE], 3).unwrap().is_empty());
}
#[test]
fn motor_does_not_fake_feedback_and_broadcasts_are_rejected() {
    let mut gateway =
        Gateway::new(vec![module(ProtocolProfile::GtM4Rl)], Faults::default(), 2).unwrap();
    assert_eq!(
        gateway.receive(&[0xAE, 0x15, 2, 0xF1, 0xEE], 0).unwrap(),
        [Ok(())]
    );
    assert!(gateway.drain_due(0).unwrap().is_empty());
    assert_eq!(
        gateway.last_motor_command(BusAddress::new(0x15).unwrap(), Channel::new(2).unwrap()),
        Some(MotorMotion::Up)
    );
    assert_eq!(
        gateway.receive(&[0xAE, 0, 2, 0xF0, 0xEE], 1).unwrap(),
        [Err(SimError::UnknownModule)]
    );
    assert_eq!(
        gateway.receive(&[0xAE, 0x15, 0xA2, 0xF2, 0xEE], 1).unwrap(),
        [Err(SimError::UnsupportedCommand)]
    );
}
#[test]
fn silent_gateway_and_invalid_commands_never_invent_responses() {
    let mut gateway = Gateway::new(
        vec![module(ProtocolProfile::GtE4Rl)],
        Faults {
            drop_replies: true,
            ..Faults::default()
        },
        2,
    )
    .unwrap();
    assert_eq!(
        gateway
            .receive(&[0xAE, 0x15, 1, 100, 0xEE, 0xAE, 0x15, 5, 100, 0xEE], 0)
            .unwrap(),
        [Ok(()), Err(SimError::UnsupportedCommand)]
    );
    assert!(gateway.drain_due(100).unwrap().is_empty());
    assert_eq!(gateway.dropped_replies(), 1);
    assert_eq!(gateway.drain_due(99), Err(SimError::ClockWentBackwards));
}
