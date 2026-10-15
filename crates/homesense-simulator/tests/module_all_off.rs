use hb_protocol::{RawFrame, StreamDecoder, encode_module_all_off};
use homesense_core::{BusAddress, ExecutionContext, ModuleSpec, ProtocolProfile, Verification};
use homesense_simulator::{Faults, Gateway, SimError};

fn module(address: u8, profile: ProtocolProfile) -> ModuleSpec {
    ModuleSpec::new(
        BusAddress::new(address).unwrap(),
        profile,
        Verification::Simulated,
    )
    .unwrap()
}
fn drain(gateway: &mut Gateway, now: u64) -> Vec<RawFrame> {
    let mut decoder = StreamDecoder::default();
    gateway
        .drain_due(now)
        .unwrap()
        .into_iter()
        .flat_map(|chunk| decoder.feed(&chunk))
        .collect()
}

#[test]
fn native_module_off_changes_all_eight_only_at_address_and_retains_restore_levels() {
    for profile in [ProtocolProfile::R8i8DinPro, ProtocolProfile::Ad8Din] {
        let target = module(0x15, profile);
        let switch = profile == ProtocolProfile::R8i8DinPro;
        let mut gateway = Gateway::new(
            vec![target.clone(), module(0x16, profile)],
            Faults {
                reply_delay_ms: 10,
                output_chunk_size: 2,
                ..Faults::default()
            },
            64,
        )
        .unwrap();
        for channel in 1..=8 {
            let level = if switch { 0xF1 } else { 37 + channel };
            assert_eq!(
                gateway
                    .receive(&[0xAE, 0x15, channel, level, 0xEE], 0)
                    .unwrap(),
                [Ok(())]
            );
        }
        let other_level = if switch { 0xF1 } else { 63 };
        gateway
            .receive(&[0xAE, 0x16, 1, other_level, 0xEE], 0)
            .unwrap()[0]
            .unwrap();
        assert_eq!(drain(&mut gateway, 10).len(), 9);
        let frame = encode_module_all_off(&target, ExecutionContext::Simulation)
            .unwrap()
            .bytes();
        assert!(gateway.receive(&frame[..2], 11).unwrap().is_empty());
        assert_eq!(gateway.receive(&frame[2..], 11).unwrap(), [Ok(())]);
        assert!(drain(&mut gateway, 20).is_empty());
        let off = if switch { 0xF2 } else { 0 };
        assert_eq!(
            drain(&mut gateway, 21),
            (1..=8)
                .map(|c| RawFrame {
                    address: 0x15,
                    command: 0xD0 + c,
                    value: off
                })
                .collect::<Vec<_>>()
        );
        for channel in 1..=8 {
            gateway
                .receive(&[0xAE, 0x15, 0xA0 + channel, 0xF2, 0xEE], 22)
                .unwrap()[0]
                .unwrap();
        }
        assert!(drain(&mut gateway, 32).iter().all(|f| f.value == off));
        gateway
            .receive(&[0xAE, 0x16, 0xA1, 0xF2, 0xEE], 33)
            .unwrap()[0]
            .unwrap();
        assert_eq!(
            drain(&mut gateway, 43),
            [RawFrame {
                address: 0x16,
                command: 0xD1,
                value: other_level
            }]
        );
        for channel in 1..=8 {
            gateway
                .receive(&[0xAE, 0x15, 0xA0 + channel, 0xF1, 0xEE], 44)
                .unwrap()[0]
                .unwrap();
        }
        assert_eq!(
            drain(&mut gateway, 54),
            (1..=8)
                .map(|c| RawFrame {
                    address: 0x15,
                    command: 0xD0 + c,
                    value: if switch { 0xF1 } else { 37 + c }
                })
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn module_off_rejects_other_models_wrong_values_and_broadcast_without_effects() {
    for profile in [
        ProtocolProfile::GtC8Rl,
        ProtocolProfile::GtE4Rl,
        ProtocolProfile::GtD8Rl,
        ProtocolProfile::GtL8Rl,
        ProtocolProfile::C4DinPro,
        ProtocolProfile::GtM4Rl,
        ProtocolProfile::Bt86cRs485,
        ProtocolProfile::Ts686Rs485SixKey,
    ] {
        let mut gateway = Gateway::new(vec![module(0x15, profile)], Faults::default(), 16).unwrap();
        for value in [0, 0xF2] {
            assert_eq!(
                gateway.receive(&[0xAE, 0x15, 0, value, 0xEE], 0).unwrap(),
                [Err(SimError::UnsupportedCommand)]
            );
            assert!(drain(&mut gateway, 0).is_empty());
        }
    }
    for (profile, wrong_value) in [
        (ProtocolProfile::R8i8DinPro, 0),
        (ProtocolProfile::Ad8Din, 0xF2),
    ] {
        let mut gateway = Gateway::new(vec![module(0x15, profile)], Faults::default(), 16).unwrap();
        let on = if profile == ProtocolProfile::Ad8Din {
            49
        } else {
            0xF1
        };
        gateway.receive(&[0xAE, 0x15, 1, on, 0xEE], 0).unwrap()[0].unwrap();
        drain(&mut gateway, 0);
        assert_eq!(
            gateway
                .receive(&[0xAE, 0x15, 0, wrong_value, 0xEE], 1)
                .unwrap(),
            [Err(SimError::UnsupportedCommand)]
        );
        assert_eq!(
            gateway
                .receive(&[0xAE, 0, 0, if on == 49 { 0 } else { 0xF2 }, 0xEE], 1)
                .unwrap(),
            [Err(SimError::UnknownModule)]
        );
        assert!(drain(&mut gateway, 1).is_empty());
        gateway.receive(&[0xAE, 0x15, 0xA1, 0xF2, 0xEE], 2).unwrap()[0].unwrap();
        assert_eq!(
            drain(&mut gateway, 2),
            [RawFrame {
                address: 0x15,
                command: 0xD1,
                value: on
            }]
        );
    }
}
