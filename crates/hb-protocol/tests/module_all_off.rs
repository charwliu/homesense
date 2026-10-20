use hb_protocol::{ProtocolError, encode_module_all_off, supports_module_all_off};
use homesense_core::{
    BusAddress, Channel, ExecutionContext, ModuleSpec, ProtocolProfile, ValidationError,
    Verification, VerificationRecord,
};
fn module(profile: ProtocolProfile, verification: Verification) -> ModuleSpec {
    ModuleSpec::new(BusAddress::new(0x37).unwrap(), profile, verification).unwrap()
}
fn verified() -> Verification {
    Verification::Verified(VerificationRecord::new("synthetic only", 1).unwrap())
}
#[test]
fn addressed_module_off_is_one_immediate_native_frame() {
    for (profile, value) in [
        (ProtocolProfile::R8i8DinPro, 0xF2),
        (ProtocolProfile::Ad8Din, 0),
    ] {
        let m = module(profile, verified());
        assert!(supports_module_all_off(profile));
        assert_eq!(
            encode_module_all_off(&m, ExecutionContext::Live)
                .unwrap()
                .bytes(),
            [0xAE, 0x37, 0, value, 0xEE]
        );
    }
    assert_eq!(BusAddress::new(0), Err(ValidationError::BroadcastAddress));
    assert_eq!(Channel::new(0), Err(ValidationError::ChannelOutOfRange));
}
#[test]
fn other_models_never_inherit_all_off_capability() {
    for p in [
        ProtocolProfile::GtC8Rl,
        ProtocolProfile::GtE4Rl,
        ProtocolProfile::GtD8Rl,
        ProtocolProfile::GtL8Rl,
        ProtocolProfile::C4DinPro,
        ProtocolProfile::GtM4Rl,
        ProtocolProfile::Bt86cRs485,
        ProtocolProfile::Ts686Rs485SixKey,
    ] {
        assert!(!supports_module_all_off(p));
        assert_eq!(
            encode_module_all_off(&module(p, verified()), ExecutionContext::Live),
            Err(ProtocolError::UnsupportedCommand)
        );
    }
}
#[test]
fn module_off_keeps_verification_and_execution_context_gates() {
    for p in [ProtocolProfile::R8i8DinPro, ProtocolProfile::Ad8Din] {
        for context in [ExecutionContext::Live, ExecutionContext::Simulation] {
            assert_eq!(
                encode_module_all_off(&module(p, Verification::Pending), context),
                Err(ProtocolError::Validation(ValidationError::UnverifiedModule))
            );
        }
        assert_eq!(
            encode_module_all_off(&module(p, Verification::Simulated), ExecutionContext::Live),
            Err(ProtocolError::Validation(ValidationError::SimulationOnly))
        );
        assert!(
            encode_module_all_off(
                &module(p, Verification::Simulated),
                ExecutionContext::Simulation
            )
            .is_ok()
        );
    }
}

#[test]
fn module_on_is_profile_specific_and_cannot_bypass_verification() {
    use hb_protocol::encode_module_all_on;
    for (profile,value) in [(ProtocolProfile::R8i8DinPro,0xf1),(ProtocolProfile::Ad8Din,100)] {
        assert_eq!(encode_module_all_on(&module(profile,verified()),ExecutionContext::Live).unwrap().bytes(),[0xae,0x37,0,value,0xee]);
        assert_eq!(encode_module_all_on(&module(profile,Verification::Pending),ExecutionContext::Live),Err(ProtocolError::Validation(ValidationError::UnverifiedModule)));
        assert_eq!(encode_module_all_on(&module(profile,Verification::Simulated),ExecutionContext::Live),Err(ProtocolError::Validation(ValidationError::SimulationOnly)));
    }
    assert_eq!(encode_module_all_on(&module(ProtocolProfile::GtC8Rl,verified()),ExecutionContext::Live),Err(ProtocolError::UnsupportedCommand));
}
