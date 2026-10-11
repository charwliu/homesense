use hap::server::{WriteBatchHandler, WriteStatus};
use homesense_hap::{
    scene_graph::SceneBinding, scene_requests::SceneRequests, scene_write::SceneWriteHandler,
};
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn authorization_precedes_reset_and_failed_acceptance_never_resets() {
    for reject in [false, true] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let accepted = trace.clone();
        let reset = trace.clone();
        let handler = SceneWriteHandler::new(
            SceneRequests::new(vec![SceneBinding {
                scene: "test".into(),
                aid: 2,
                on_iid: 9,
            }])
            .unwrap(),
            move |batch: homesense_hap::scene_requests::SceneBatch| {
                let trace = accepted.clone();
                async move {
                    trace
                        .lock()
                        .unwrap()
                        .push(("authorize", batch.starts.len()));
                    if reject {
                        Err(WriteStatus::ServiceCommunicationFailure)
                    } else {
                        Ok(())
                    }
                }
            },
            move |scenes: Vec<String>| {
                let trace = reset.clone();
                async move {
                    trace.lock().unwrap().push(("reset", scenes.len()));
                    Ok(())
                }
            },
        );
        let id = uuid::Uuid::new_v4();
        let body = br#"{"characteristics":[{"aid":2,"iid":9,"value":true}]}"#;
        assert!(handler.write(id, body).is_err());
        assert!(trace.lock().unwrap().is_empty());
        assert_eq!(handler.write_async(id, body).await.is_err(), reject);
        assert_eq!(
            *trace.lock().unwrap(),
            if reject {
                vec![("authorize", 1)]
            } else {
                vec![("authorize", 1), ("reset", 1)]
            }
        );
        trace.lock().unwrap().clear();
        let _ = handler
            .write_async(
                id,
                br#"{"characteristics":[{"aid":2,"iid":9,"value":false}]}"#,
            )
            .await;
        assert_eq!(*trace.lock().unwrap(), vec![("authorize", 0)]);
    }
}
#[tokio::test]
async fn reset_failure_does_not_resubmit_accepted_action() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let handler = SceneWriteHandler::new(
        SceneRequests::new(vec![SceneBinding {
            scene: "test".into(),
            aid: 2,
            on_iid: 9,
        }])
        .unwrap(),
        move |_: homesense_hap::scene_requests::SceneBatch| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(()) }
        },
        |_: Vec<String>| async { Err(WriteStatus::ServiceCommunicationFailure) },
    );
    assert!(
        handler
            .write_async(
                uuid::Uuid::new_v4(),
                br#"{"characteristics":[{"aid":2,"iid":9,"value":true}]}"#
            )
            .await
            .is_err()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
