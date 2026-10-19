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
        ProtocolProfile::Ts686Rs485SixKey,
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

#[test]
fn r8i8_pro_manual_single_channel_vectors() {
    let module = ModuleSpec::new(address(), ProtocolProfile::R8i8DinPro, verified()).unwrap();
    let mut registry = Registry::default();
    registry.register(module.clone()).unwrap();
    for c in 1..=8 {
        for (on, value) in [(true, 0xF1), (false, 0xF2)] {
            assert_eq!(
                encode(&module, channel(c), Command::Switch(on))
                    .unwrap()
                    .bytes(),
                [0xAE, 0x41, c, value, 0xEE]
            );
            assert_eq!(
                registry.decode(RawFrame {
                    address: 0x41,
                    command: 0xD0 + c,
                    value
                }),
                Decoded::Feedback {
                    address: address(),
                    channel: channel(c),
                    observation: Observation::Switch(on)
                }
            );
        }
        assert_eq!(
            encode(&module, channel(c), Command::Query).unwrap().bytes(),
            [0xAE, 0x41, 0xA0 + c, 0xF2, 0xEE]
        );
    }
    assert!(encode(&module, channel(9), Command::Switch(true)).is_err());
    let pending = ModuleSpec::new(
        address(),
        ProtocolProfile::R8i8DinPro,
        Verification::Pending,
    )
    .unwrap();
    assert!(encode(&pending, channel(1), Command::Switch(true)).is_err());
}

#[test]
fn c4_pro_manual_motion_vectors_do_not_invent_feedback() {
    use homesense_core::MotorMotion;
    let module = ModuleSpec::new(address(), ProtocolProfile::C4DinPro, verified()).unwrap();
    let mut registry = Registry::default();
    registry.register(module.clone()).unwrap();
    for c in 1..=4 {
        for (motion, value) in [
            (MotorMotion::Up, 0xF1),
            (MotorMotion::Down, 0xF2),
            (MotorMotion::Stop, 0xF0),
        ] {
            assert_eq!(
                encode(&module, channel(c), Command::Motor(motion))
                    .unwrap()
                    .bytes(),
                [0xAE, 0x41, c, value, 0xEE]
            );
            assert!(matches!(
                registry.decode(RawFrame {
                    address: 0x41,
                    command: 0xD0 + c,
                    value
                }),
                Decoded::Unknown { .. }
            ));
        }
        assert!(encode(&module, channel(c), Command::Query).is_err());
    }
    assert!(encode(&module, channel(5), Command::Motor(MotorMotion::Stop)).is_err());
}

#[test]
fn ts6_six_key_vectors_and_led_mode_gate() {
    let module = ModuleSpec::new(address(), ProtocolProfile::Ts686Rs485SixKey, verified()).unwrap();
    assert!(encode(&module, channel(1), Command::PanelLed(true)).is_err());
    let manual = module
        .clone()
        .with_panel_led_mode(PanelLedMode::Manual)
        .unwrap();
    let mut registry = Registry::default();
    registry.register(module).unwrap();
    for c in 1..=6 {
        for (active, value) in [(true, 0xF1), (false, 0xF2)] {
            assert_eq!(
                encode(&manual, channel(c), Command::PanelLed(active))
                    .unwrap()
                    .bytes(),
                [0xAE, 0x41, c, value, 0xEE]
            );
            assert_eq!(
                registry.decode(RawFrame {
                    address: 0x41,
                    command: 0xD0 + c,
                    value
                }),
                Decoded::Feedback {
                    address: address(),
                    channel: channel(c),
                    observation: Observation::Button { pressed: active }
                }
            );
        }
        // F0 controls backlight-off, but is not a documented button event.
        assert!(matches!(
            registry.decode(RawFrame {
                address: 0x41,
                command: 0xD0 + c,
                value: 0xF0
            }),
            Decoded::Unknown {
                reason: UnknownReason::InvalidValue,
                ..
            }
        ));
    }
    assert!(encode(&manual, channel(7), Command::PanelLed(true)).is_err());
    assert!(matches!(
        registry.decode(RawFrame {
            address: 0x41,
            command: 0xD7,
            value: 0xF1
        }),
        Decoded::Unknown {
            reason: UnknownReason::InvalidChannel,
            ..
        }
    ));
}

#[test]
fn ad8_encodes_direct_percentages_and_explicit_legacy_ramps() {
    use homesense_core::{LevelTransition, Percent};
    let module = ModuleSpec::new(address(), ProtocolProfile::Ad8Din, verified()).unwrap();
    let mut registry = Registry::default();
    registry.register(module.clone()).unwrap();
    for c in 1..=8 {
        for value in 0..=100 {
            let level = Percent::new(value).unwrap();
            assert_eq!(
                encode(
                    &module,
                    channel(c),
                    Command::SetLevel {
                        level,
                        transition: LevelTransition::Ramp
                    }
                )
                .unwrap()
                .bytes(),
                [0xAE, 0x41, 0xB0 + c, value, 0xEE]
            );
            let immediate = encode(
                &module,
                channel(c),
                Command::SetLevel {
                    level,
                    transition: LevelTransition::Immediate,
                },
            );
            assert_eq!(immediate.unwrap().bytes(), [0xAE, 0x41, c, value, 0xEE]);
            assert_eq!(
                registry.decode(RawFrame {
                    address: 0x41,
                    command: 0xD0 + c,
                    value
                }),
                Decoded::Feedback {
                    address: address(),
                    channel: channel(c),
                    observation: Observation::Level(level)
                }
            );
        }
        assert_eq!(
            encode(&module, channel(c), Command::Query).unwrap().bytes(),
            [0xAE, 0x41, 0xA0 + c, 0xF2, 0xEE]
        );
        assert_eq!(
            encode(&module, channel(c), Command::PauseRamp)
                .unwrap()
                .bytes(),
            [0xAE, 0x41, 0xA0 + c, 0x05, 0xEE]
        );
    }
    assert!(encode(&module, channel(9), Command::Query).is_err());
}

#[test]
fn whole_module_query_requires_explicit_same_model_capability_and_verification() {
    for profile in [
        ProtocolProfile::Ad8Din,
        ProtocolProfile::GtE4Rl,
        ProtocolProfile::GtD8Rl,
        ProtocolProfile::GtL8Rl,
    ] {
        let module = ModuleSpec::new(address(), profile, verified()).unwrap();
        assert_eq!(
            hb_protocol::encode_module_query(&module, ExecutionContext::Live)
                .unwrap()
                .bytes(),
            [0xAE, 0x41, 0xA0, 0xF2, 0xEE]
        );
        let module = ModuleSpec::new(address(), profile, Verification::Simulated).unwrap();
        assert!(hb_protocol::encode_module_query(&module, ExecutionContext::Live).is_err());
        assert!(hb_protocol::encode_module_query(&module, ExecutionContext::Simulation).is_ok());
    }
    for profile in [
        ProtocolProfile::R8i8DinPro,
        ProtocolProfile::GtC8Rl,
        ProtocolProfile::C4DinPro,
        ProtocolProfile::GtM4Rl,
        ProtocolProfile::Bt86cRs485,
    ] {
        let module = ModuleSpec::new(address(), profile, verified()).unwrap();
        assert_eq!(
            hb_protocol::encode_module_query(&module, ExecutionContext::Live),
            Err(ProtocolError::UnsupportedCommand)
        );
    }
    assert!(BusAddress::new(0).is_err());
}
