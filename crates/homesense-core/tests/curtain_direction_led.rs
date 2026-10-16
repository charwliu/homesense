use homesense_core::{MotorAction,queue::Epochs,curtain_state::{Tracker,Timing,Scope,DirectionLedPolicy,DirectionIndicators}};
fn tracker(travel:u64)->(Tracker,Scope) {
    let scope=Scope{boot:1,epochs:Epochs{transport:1,configuration:1,control:1}};
    (Tracker::new(Timing{open_travel_ms:Some(travel),close_travel_ms:Some(travel),stop_led_idle_ms:Some(1)},scope,0).unwrap(),scope)
}
const POLICY:DirectionLedPolicy=DirectionLedPolicy::WrittenDirectionDelay{opposite_red_after_ms:2000};
#[test]
fn written_direction_delay_leaves_opposite_unknown_until_exact_edge_and_keeps_legacy() {
    for action in [MotorAction::Open,MotorAction::Close] {
        let(mut t,scope)=tracker(10000);
        assert_eq!(t.snapshot().indicators_for(POLICY),DirectionIndicators{open_red:None,close_red:None,stop_red:None});
        let p=t.accepted(scope,1,action,10).unwrap();
        assert_eq!(t.advance(scope,500).unwrap().direction_deadline(POLICY),None);
        t.written_at(p,500,900).unwrap();
        assert_eq!(t.snapshot().direction_deadline(POLICY),Some(2500));
        for at in [900,2499,2500] {
            let s=t.advance(scope,at).unwrap();let hint=s.indicators_for(POLICY);
            assert_eq!(hint.stop_red,Some(true));
            assert!(!s.indicators().open_red,"default legacy projection unchanged");
            assert!(!s.indicators().close_red);
            if action==MotorAction::Open {assert_eq!(hint.open_red,Some(true));assert_eq!(hint.close_red,(at>=2500).then_some(true));}
            else {assert_eq!(hint.close_red,Some(true));assert_eq!(hint.open_red,(at>=2500).then_some(true));}
        }
    }
}
#[test]
fn endpoint_before_delay_and_midstop_override_old_delay() {
    let(mut short,scope)=tracker(1000);let p=short.accepted(scope,1,MotorAction::Open,0).unwrap();short.written(p,10).unwrap();
    let end=short.advance(scope,1010).unwrap();assert_eq!(end.indicators_for(POLICY),DirectionIndicators{open_red:Some(false),close_red:Some(true),stop_red:Some(true)});
    assert_eq!(end.direction_deadline(POLICY),None);
    let end=short.advance(scope,2010).unwrap();assert_eq!(end.indicators_for(POLICY).open_red,Some(false));assert_eq!(end.indicators_for(POLICY).stop_red,Some(false));
    let(mut t,scope)=tracker(10000);let p=t.accepted(scope,1,MotorAction::Open,0).unwrap();t.written(p,10).unwrap();
    let p=t.accepted(scope,2,MotorAction::Stop,900).unwrap();t.written(p,1000).unwrap();
    let s=t.advance(scope,1001).unwrap();assert_eq!(s.indicators_for(POLICY),DirectionIndicators{open_red:Some(true),close_red:Some(true),stop_red:Some(false)});
    assert_eq!(s.direction_deadline(POLICY),None);
    assert_eq!(t.advance(scope,2010).unwrap().indicators_for(POLICY),s.indicators_for(POLICY));
}
#[test]
fn reverse_repeated_write_scope_invalidation_cancel_old_delay() {
    let(mut t,scope)=tracker(10000);let p=t.accepted(scope,1,MotorAction::Open,0).unwrap();t.written(p,10).unwrap();
    let p=t.accepted(scope,2,MotorAction::Close,900).unwrap();t.written(p,1000).unwrap();
    let s=t.advance(scope,2010).unwrap();assert_eq!(s.indicators_for(POLICY).open_red,None);assert_eq!(s.direction_deadline(POLICY),Some(3000));
    let p=t.accepted(scope,3,MotorAction::Close,2200).unwrap();t.written(p,2300).unwrap();
    let s=t.advance(scope,3000).unwrap();assert_eq!(s.indicators_for(POLICY).open_red,None);assert_eq!(s.direction_deadline(POLICY),Some(4300));
    t.invalidate();let s=t.advance(scope,4300).unwrap();assert_eq!(s.indicators_for(POLICY),DirectionIndicators{open_red:None,close_red:None,stop_red:None});assert_eq!(s.direction_deadline(POLICY),None);
    let mut next=scope;next.epochs.transport=2;let s=t.advance(next,4500).unwrap();assert_eq!(s.direction_deadline(POLICY),None);assert_eq!(s.indicators_for(POLICY).close_red,None);
}
#[test]
fn policy_validation_is_bounded_and_zero_stop_idle_stays_invalid() {
    for n in [0,86_400_001,u64::MAX] {assert!(DirectionLedPolicy::WrittenDirectionDelay{opposite_red_after_ms:n}.validate().is_err());}
    for n in [1,2000,86_400_000] {assert!(DirectionLedPolicy::WrittenDirectionDelay{opposite_red_after_ms:n}.validate().is_ok());}
    assert!(Timing{stop_led_idle_ms:Some(0),..Default::default()}.validate().is_err());
}
