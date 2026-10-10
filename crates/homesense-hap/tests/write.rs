use hap::server::{WriteBatchHandler, WriteStatus};
use homesense_core::{Command, DeviceId, LevelTransition, Percent};
use homesense_hap::{
    requests::{LightBinding, LightRequests},
    write::LightWriteHandler,
};
use std::sync::Mutex;
use uuid::Uuid;

#[test]
fn grouped_adapter_preserves_principal_and_never_submits_invalid_partial_intent() {
    let id = Uuid::new_v4();
    let accepted = Mutex::new(Vec::new());
    let requests = LightRequests::new(vec![LightBinding {
        device: DeviceId::new("light").unwrap(),
        aid: 2,
        on_iid: 10,
        brightness_iid: Some(11),
        default_on_level: Some(Percent::new(30).unwrap()),
    }])
    .unwrap();
    let handler = LightWriteHandler::new(
        requests,
        |_: &DeviceId| None,
        |batch: homesense_hap::requests::Batch| {
            assert_eq!(batch.controller, id);
            accepted.lock().unwrap().push(batch);
            Ok(())
        },
    );
    handler.write(id, br#"{"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":2,"iid":11,"value":73}]}"#).unwrap();
    assert_eq!(accepted.lock().unwrap()[0].intents.len(), 1);
    assert_eq!(
        accepted.lock().unwrap()[0].intents[0].command,
        Command::SetLevel {
            level: Percent::new(73).unwrap(),
            transition: LevelTransition::Immediate
        }
    );
    assert_eq!(handler.write(id, br#"{"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":2,"iid":11,"value":101}]}"#), Err(WriteStatus::InvalidValueInRequest));
    assert_eq!(accepted.lock().unwrap().len(), 1);
}

#[test]
fn runtime_backpressure_and_subscription_authorization_are_not_swallowed() {
    let requests = LightRequests::new(vec![LightBinding {
        device: DeviceId::new("light").unwrap(),
        aid: 2,
        on_iid: 10,
        brightness_iid: None,
        default_on_level: None,
    }])
    .unwrap();
    let handler = LightWriteHandler::new(
        requests,
        |_: &DeviceId| None,
        |batch: homesense_hap::requests::Batch| {
            assert!(batch.intents.is_empty());
            assert_eq!(batch.subscriptions.len(), 1);
            Err(WriteStatus::ResourceBusy)
        },
    );
    assert_eq!(
        handler.write(
            Uuid::new_v4(),
            br#"{"characteristics":[{"aid":2,"iid":10,"ev":true}]}"#
        ),
        Err(WriteStatus::ResourceBusy)
    );
}
