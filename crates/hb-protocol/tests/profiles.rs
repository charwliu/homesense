use hb_protocol::{
    Decoded, ProtocolError, RawFrame, Registry, UnknownReason, encode, encode_in_context,
};
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, ModuleSpec, Observation, PanelLedMode,
    ProtocolProfile, ValidationError, Verification, VerificationRecord,
};

fn address() -> BusAddress {
    BusAddress::new(0x41).unwrap()
}
fn channel(n: u8) -> Channel {
    Channel::new(n).unwrap()
}
fn verified() -> Verification {
    Verification::Verified(
        VerificationRecord::new("test fixture only; no site verification", 1).unwrap(),
    )
}
fn panel(verification: Verification) -> ModuleSpec {
    ModuleSpec::new(address(), ProtocolProfile::Bt86cRs485, verification).unwrap()
}

#[test]
fn bt86_accepts_exactly_six_documented_keys_and_no_extended_events() {
    let module = panel(verified())
        .with_panel_led_mode(PanelLedMode::Manual)
        .unwrap();
    let mut registry = Registry::default();
    registry.register(module.clone()).unwrap();
    for key in 1..=6 {
        for active in [false, true] {
            let value = if active { 0xF1 } else { 0xF2 };
            assert_eq!(
                encode(&module, channel(key), Command::PanelLed(active))
                    .unwrap()
                    .bytes(),
                [0xAE, 0x41, key, value, 0xEE]
            );
            assert_eq!(
                registry.decode(RawFrame {
                    address: 0x41,
                    command: 0xD0 + key,
                    value
                }),
                Decoded::Feedback {
                    address: address(),
                    channel: channel(key),
                    observation: Observation::Button { pressed: active }
                }
            );
        }
    }
    for key in 7..=16 {
        assert!(encode(&module, channel(key), Command::PanelLed(true)).is_err());
        let frame = RawFrame {
            address: 0x41,
            command: 0xD0 + key,
            value: 0xF1,
        };
        assert_eq!(
            registry.decode(frame),
            Decoded::Unknown {
                frame,
                reason: UnknownReason::InvalidChannel
            }
        );
    }
}

#[test]
fn pending_profile_never_authorizes_commands_or_events_even_in_simulation() {
    for profile in [
        ProtocolProfile::UnverifiedKeypad,
        ProtocolProfile::Bt86cRs485,
    ] {
        let module = ModuleSpec::new(address(), profile, Verification::Pending).unwrap();
        for context in [ExecutionContext::Live, ExecutionContext::Simulation] {
            assert_eq!(
                encode_in_context(&module, channel(1), Command::PanelLed(true), context),
                Err(ProtocolError::Validation(ValidationError::UnverifiedModule))
            );
        }
        for mut registry in [Registry::default(), Registry::for_simulation()] {
            registry.register(module.clone()).unwrap();
            let frame = RawFrame {
                address: 0x41,
                command: 0xD1,
                value: 0xF1,
            };
            assert_eq!(
                registry.decode(frame),
                Decoded::Unknown {
                    frame,
                    reason: UnknownReason::UnverifiedModule
                }
            );
        }
    }
    assert!(ModuleSpec::new(address(), ProtocolProfile::UnverifiedKeypad, verified()).is_err());
}

#[test]
fn simulator_evidence_cannot_be_used_by_live_encoder_or_registry() {
    let module = panel(Verification::Simulated)
        .with_panel_led_mode(PanelLedMode::Manual)
        .unwrap();
    assert_eq!(
        encode(&module, channel(1), Command::PanelLed(true)),
        Err(ProtocolError::Validation(ValidationError::SimulationOnly))
    );
    assert!(
        encode_in_context(
            &module,
            channel(1),
            Command::PanelLed(true),
            ExecutionContext::Simulation
        )
        .is_ok()
    );
    let frame = RawFrame {
        address: 0x41,
        command: 0xD1,
        value: 0xF1,
    };
    let mut live = Registry::default();
    live.register(module.clone()).unwrap();
    assert_eq!(
        live.decode(frame),
        Decoded::Unknown {
            frame,
            reason: UnknownReason::SimulationOnly
        }
    );
    let mut simulation = Registry::for_simulation();
    simulation.register(module).unwrap();
    assert!(matches!(simulation.decode(frame), Decoded::Feedback { .. }));
}

#[test]
fn led_mode_does_not_disable_buttons_but_only_manual_enables_led_writes() {
    for mode in [
        PanelLedMode::Unknown,
        PanelLedMode::SelfReset,
        PanelLedMode::Manual,
    ] {
        let module = panel(verified()).with_panel_led_mode(mode).unwrap();
        let result = encode(&module, channel(1), Command::PanelLed(true));
        if mode == PanelLedMode::Manual {
            assert!(result.is_ok());
        } else {
            assert_eq!(
                result,
                Err(ProtocolError::Validation(ValidationError::LedModeNotManual))
            );
        }
        let mut registry = Registry::default();
        registry.register(module).unwrap();
        assert!(matches!(
            registry.decode(RawFrame {
                address: 0x41,
                command: 0xD1,
                value: 0xF2
            }),
            Decoded::Feedback {
                observation: Observation::Button { pressed: false },
                ..
            }
        ));
    }
}

#[test]
fn profiles_and_evidence_cannot_be_mutated_into_other_device_capabilities() {
    assert!(VerificationRecord::new("", 1).is_err());
    assert!(VerificationRecord::new("inspection", 0).is_err());
    let module = ModuleSpec::new(address(), ProtocolProfile::GtC8Rl, verified()).unwrap();
    assert!(module.with_panel_led_mode(PanelLedMode::Manual).is_err());
    assert_eq!(ProtocolProfile::Bt86cRs485.channels(), 6);
    assert_eq!(ProtocolProfile::UnverifiedKeypad.channels(), 0);
}
