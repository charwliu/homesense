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

#[tokio::test]
async fn asynchronous_acceptance_waits_and_preserves_runtime_rejection() {
    use hap::futures::{channel::oneshot, poll};
    use homesense_hap::write::AsyncLightWriteHandler;
    use std::{sync::Arc, task::Poll};
    for outcome in [Ok(()), Err(WriteStatus::ResourceBusy)] {
        let controller = Uuid::new_v4();
        let requests = LightRequests::new(vec![LightBinding {
            device: DeviceId::new("light").unwrap(),
            aid: 2,
            on_iid: 10,
            brightness_iid: None,
            default_on_level: None,
        }])
        .unwrap();
        let (sender, receiver) = oneshot::channel();
        let receiver = Arc::new(Mutex::new(Some(receiver)));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let handler = AsyncLightWriteHandler::new(
            requests,
            |_: &DeviceId| None,
            move |batch: homesense_hap::requests::Batch| {
                captured.lock().unwrap().push(batch);
                let receive = receiver.lock().unwrap().take().unwrap();
                async move { receive.await.unwrap() }
            },
        );
        let body = br#"{"characteristics":[{"aid":2,"iid":10,"value":true}]}"#;
        assert_eq!(
            handler.write(controller, body),
            Err(WriteStatus::ServiceCommunicationFailure)
        );
        assert!(seen.lock().unwrap().is_empty());
        assert!(
            handler
                .write_async(
                    controller,
                    br#"{"characteristics":[{"aid":2,"iid":10,"value":2}]}"#
                )
                .await
                .is_err()
        );
        assert!(seen.lock().unwrap().is_empty());
        let mut pending = handler.write_async(controller, body);
        assert!(matches!(poll!(pending.as_mut()), Poll::Pending));
        assert_eq!(seen.lock().unwrap()[0].controller, controller);
        assert_eq!(
            seen.lock().unwrap()[0].intents[0].command,
            Command::Switch(true)
        );
        sender.send(outcome).unwrap();
        assert_eq!(pending.await, outcome);
    }
}
