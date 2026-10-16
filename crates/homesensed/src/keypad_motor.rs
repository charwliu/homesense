//! Card-only native motor press. The immutable mapping and current owner are
//! interpreted inside Admission; a client never selects STOP from cached LEDs.
use hap::server::WriteStatus;
use homesense_config::{Configuration, keypad::{KeyAction, KeyMotorAction, MotorPressBehavior}};
use homesense_core::{DeviceId, MotorAction, curtain_state::Movement};

pub(crate) struct Binding {
    pub endpoint: DeviceId,
    pub action: MotorAction,
}

pub(crate) fn resolve(config:&Configuration, device:&DeviceId, keypad:&str, number:u8)->Result<Binding,WriteStatus> {
    let invalid=WriteStatus::InvalidValueInRequest;
    let parent=DeviceId::new(keypad).map_err(|_|invalid)?;
    let key=config.keypads().get(&parent).filter(|p|p.enabled)
        .and_then(|p|p.keys.get(&number)).filter(|k|k.led_endpoint.enabled).ok_or(invalid)?;
    let Some(KeyAction::Motor{device:target,action,press_behavior:MotorPressBehavior::RepeatToStop,..})=&key.action else {return Err(invalid);};
    if device.as_str()!=target {return Err(invalid);}
    let action=match action {
        KeyMotorAction::Open=>MotorAction::Open,
        KeyMotorAction::Close=>MotorAction::Close,
        KeyMotorAction::Stop=>return Err(invalid),
    };
    let endpoint=DeviceId::new(&key.led_endpoint.id).map_err(|_|invalid)?;
    if config.binding(&endpoint).is_none() {return Err(invalid);}
    config.motor_command(device,action).map_err(|_|invalid)?;
    Ok(Binding{endpoint,action})
}

pub(crate) fn decide(requested:MotorAction,movement:Option<Movement>)->MotorAction {
    match (requested,movement) {
        (MotorAction::Open,Some(Movement::Opening)) | (MotorAction::Close,Some(Movement::Closing))=>MotorAction::Stop,
        _=>requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_same_written_direction_becomes_stop_unknown_stopped_and_reverse_stay_absolute() {
        for requested in [MotorAction::Open,MotorAction::Close] {
            for state in [None,Some(Movement::Stopped)] {assert_eq!(decide(requested,state),requested);}
        }
        assert_eq!(decide(MotorAction::Open,Some(Movement::Opening)),MotorAction::Stop);
        assert_eq!(decide(MotorAction::Close,Some(Movement::Closing)),MotorAction::Stop);
        assert_eq!(decide(MotorAction::Open,Some(Movement::Closing)),MotorAction::Open);
        assert_eq!(decide(MotorAction::Close,Some(Movement::Opening)),MotorAction::Close);
        for state in [None,Some(Movement::Stopped),Some(Movement::Opening),Some(Movement::Closing)] {
            assert_eq!(decide(MotorAction::Stop,state),MotorAction::Stop);
        }
    }
}

#[cfg(test)]
#[path="keypad_motor_tests.rs"]
mod admission_tests;
