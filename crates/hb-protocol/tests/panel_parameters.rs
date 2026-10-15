use hb_protocol::{RawFrame, encode_in_context};
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, ModuleSpec, PanelBacklight as B,
    PanelFeedbackMode as M, PanelLedMode, PanelParameter as P, ProtocolProfile, Verification,
};
fn panel(mode: PanelLedMode) -> ModuleSpec {
    ModuleSpec::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Ts686Rs485SixKey,
        Verification::Simulated,
    )
    .unwrap()
    .with_panel_led_mode(mode)
    .unwrap()
}
#[test]
fn parameter_frames_match_ts6_typed_manual_vectors_and_never_claim_feedback() {
    let spec = panel(PanelLedMode::Manual);
    let channel = Channel::new(1).unwrap();
    for (parameter, command, value) in [
        (P::ReadBacklight, 0xC5, 0),
        (P::Backlight(B::DimAfterTenSeconds), 0xC6, 1),
        (P::Backlight(B::Bright), 0xC6, 2),
        (P::Backlight(B::Dim), 0xC6, 3),
        (P::ReadSound, 0xC1, 0),
        (P::Sound(true), 0xC2, 1),
        (P::Sound(false), 0xC2, 2),
        (P::ReadFeedbackMode, 0xC7, 0),
        (P::FeedbackMode(M::Manual), 0xC8, 2),
    ] {
        let frame = encode_in_context(
            &spec,
            channel,
            Command::PanelParameter(parameter),
            ExecutionContext::Simulation,
        )
        .unwrap();
        assert_eq!(
            frame,
            RawFrame {
                address: 0x41,
                command,
                value
            }
        );
        assert!(
            !Command::PanelParameter(parameter)
                .target_matches(homesense_core::Observation::Button { pressed: true })
        );
    }
    assert_eq!(
        encode_in_context(
            &spec,
            channel,
            Command::PanelLedOff,
            ExecutionContext::Simulation
        )
        .unwrap()
        .bytes(),
        [0xAE, 0x41, 1, 0xF0, 0xEE]
    );
}
#[test]
fn parameter_profile_and_work_mode_must_match_reviewed_configuration() {
    let channel = Channel::new(1).unwrap();
    let manual = panel(PanelLedMode::Manual);
    let reset = panel(PanelLedMode::SelfReset);
    assert!(
        encode_in_context(
            &manual,
            channel,
            Command::PanelParameter(P::FeedbackMode(M::SelfReset)),
            ExecutionContext::Simulation
        )
        .is_err()
    );
    assert!(
        encode_in_context(
            &reset,
            channel,
            Command::PanelParameter(P::FeedbackMode(M::Manual)),
            ExecutionContext::Simulation
        )
        .is_err()
    );
    assert_eq!(
        encode_in_context(
            &reset,
            channel,
            Command::PanelParameter(P::FeedbackMode(M::SelfReset)),
            ExecutionContext::Simulation
        )
        .unwrap()
        .bytes(),
        [0xAE, 0x41, 0xC8, 1, 0xEE]
    );
    assert!(
        encode_in_context(
            &reset,
            channel,
            Command::PanelLedOff,
            ExecutionContext::Simulation
        )
        .is_err()
    );
    let other = ModuleSpec::new(
        BusAddress::new(0x41).unwrap(),
        ProtocolProfile::Bt86cRs485,
        Verification::Simulated,
    )
    .unwrap();
    assert!(
        encode_in_context(
            &other,
            channel,
            Command::PanelParameter(P::ReadBacklight),
            ExecutionContext::Simulation
        )
        .is_err()
    );
}
