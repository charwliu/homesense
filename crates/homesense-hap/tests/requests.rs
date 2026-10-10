use homesense_core::{Command, DeviceId, LevelTransition, Percent};
use homesense_hap::requests::{LightBinding, LightRequests, RequestError};
use serde_json::json;
use uuid::Uuid;
fn adapter(default: Option<u8>) -> LightRequests {
    LightRequests::new(vec![LightBinding {
        device: DeviceId::new("light").unwrap(),
        aid: 2,
        on_iid: 10,
        brightness_iid: Some(11),
        default_on_level: default.map(|v| Percent::new(v).unwrap()),
    }])
    .unwrap()
}
fn value(on: bool, level: u8, reverse: bool) -> Vec<u8> {
    let mut writes = vec![
        json!({"aid":2,"iid":10,"value":on}),
        json!({"aid":2,"iid":11,"value":level}),
    ];
    if reverse {
        writes.reverse();
    }
    serde_json::to_vec(&json!({"characteristics":writes})).unwrap()
}
#[test]
fn grouped_on_and_brightness_are_order_independent_with_off_winning() {
    let a = adapter(Some(30));
    let controller = Uuid::new_v4();
    for (on, level, expected) in [(true, 73, 73), (false, 73, 0), (true, 0, 0), (false, 0, 0)] {
        for reverse in [false, true] {
            let batch = a
                .normalize(controller, &value(on, level, reverse), |_| None)
                .unwrap();
            assert_eq!(batch.controller, controller);
            assert_eq!(batch.intents.len(), 1);
            assert_eq!(
                batch.intents[0].command,
                Command::SetLevel {
                    level: Percent::new(expected).unwrap(),
                    transition: LevelTransition::Immediate
                }
            );
        }
    }
}
#[test]
fn on_only_uses_confirmed_restore_then_configured_default_and_never_guesses() {
    let body = br#"{"characteristics":[{"aid":2,"iid":10,"value":true}]}"#;
    for (last, default, expected) in [
        (Some(73), Some(30), 73),
        (None, Some(30), 30),
        (Some(0), Some(30), 30),
    ] {
        let batch = adapter(default)
            .normalize(Uuid::new_v4(), body, |_| {
                last.map(|v| Percent::new(v).unwrap())
            })
            .unwrap();
        assert_eq!(
            batch.intents[0].command,
            Command::SetLevel {
                level: Percent::new(expected).unwrap(),
                transition: LevelTransition::Immediate
            }
        );
    }
    assert_eq!(
        adapter(None)
            .normalize(Uuid::new_v4(), body, |_| None)
            .unwrap_err(),
        RequestError::MissingRestoreLevel
    );
}
#[test]
fn malformed_duplicate_and_out_of_range_requests_produce_no_batch() {
    let a = adapter(Some(30));
    for invalid in [json!(-1), json!(101), json!(50.5), json!("30"), json!(true)] {
        let body=serde_json::to_vec(&json!({"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":2,"iid":11,"value":invalid}]})).unwrap();
        assert!(
            a.normalize(Uuid::new_v4(), &body, |_| panic!(
                "input must be checked before snapshot resolution"
            ))
            .is_err()
        );
    }
    for body in [br#"{"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":2,"iid":10,"value":false}]}"#.as_slice(),
        br#"{"characteristics":[{"aid":2,"iid":12,"value":1}]}"#,
        br#"{"characteristics":[{"aid":2,"iid":10,"value":true,"authData":"x"}]}"#,
        br#"{"characteristics":[{"aid":2,"iid":10,"value":true}],"controller":"forged"}"#] {
        assert!(a.normalize(Uuid::new_v4(),body, |_|None).is_err());
    }
}
#[test]
fn subscriptions_do_not_create_control_intents_and_binding_collisions_are_rejected() {
    let a = adapter(None);
    let batch = a
        .normalize(
            Uuid::new_v4(),
            br#"{"characteristics":[{"aid":2,"iid":10,"ev":true}]}"#,
            |_| panic!("subscription must not read restore state"),
        )
        .unwrap();
    assert!(batch.intents.is_empty());
    assert_eq!(batch.subscriptions.len(), 1);
    assert!(
        LightRequests::new(vec![LightBinding {
            device: DeviceId::new("x").unwrap(),
            aid: 2,
            on_iid: 10,
            brightness_iid: Some(10),
            default_on_level: None
        }])
        .is_err()
    );
}
#[test]
fn switch_mapping_and_input_limits_are_enforced() {
    let a = LightRequests::new(vec![LightBinding {
        device: DeviceId::new("switch").unwrap(),
        aid: 2,
        on_iid: 10,
        brightness_iid: None,
        default_on_level: None,
    }])
    .unwrap();
    let batch = a
        .normalize(
            Uuid::new_v4(),
            br#"{"characteristics":[{"aid":2,"iid":10,"value":1}]}"#,
            |_| None,
        )
        .unwrap();
    assert_eq!(batch.intents[0].command, Command::Switch(true));
    assert_eq!(
        a.normalize(Uuid::new_v4(), &vec![b' '; 65537], |_| None)
            .unwrap_err(),
        RequestError::TooLarge
    );
    let body = serde_json::to_vec(
        &json!({"characteristics":vec![json!({"aid":2,"iid":10,"value":true});129]}),
    )
    .unwrap();
    assert_eq!(
        a.normalize(Uuid::new_v4(), &body, |_| None).unwrap_err(),
        RequestError::TooLarge
    );
}
