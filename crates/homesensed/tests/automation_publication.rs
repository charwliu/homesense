use homesensed::automation_publication::channel;
use std::{future::Future, task::Poll};
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(std::pin::Pin::new(&mut *future).poll(cx))).await
}
#[tokio::test]
async fn bounded_queue_preserves_payload_and_releases_waiters_on_close() {
    let (publisher, mut receiver) = channel();
    let mut first = Box::pin(publisher.publish(b"devices".to_vec(), b"rules".to_vec(), 3));
    let mut second = Box::pin(publisher.publish(vec![1], vec![2], 3));
    assert!(poll_once(&mut first).await.is_pending());
    assert!(poll_once(&mut second).await.is_pending());
    assert!(
        publisher
            .publish(vec![1], vec![2], 3)
            .await
            .unwrap_err()
            .contains("busy")
    );
    let pending = receiver.receive().await.unwrap();
    assert_eq!(pending.devices(), b"devices");
    assert_eq!(pending.rules(), b"rules");
    assert_eq!(pending.expected_revision(), 3);
    pending.complete(Ok(4));
    assert_eq!(first.await.unwrap(), 4);
    receiver.close();
    assert!(second.await.unwrap_err().contains("before acceptance"));
    assert!(publisher.publish(vec![1], vec![2], 4).await.is_err());
}
#[tokio::test]
async fn abandoned_request_is_skipped_before_live_request() {
    let (publisher, mut receiver) = channel();
    let mut abandoned = Box::pin(publisher.publish(vec![1], vec![2], 1));
    assert!(poll_once(&mut abandoned).await.is_pending());
    drop(abandoned);
    let mut live = Box::pin(publisher.publish(vec![3], vec![4], 2));
    assert!(poll_once(&mut live).await.is_pending());
    let request = receiver.receive().await.unwrap();
    assert_eq!(request.expected_revision(), 2);
    drop(live);
    assert!(request.abandoned());
    request.complete(Ok(3));
}
#[tokio::test]
async fn invalid_sizes_and_revision_do_not_enter_queue() {
    let (publisher, mut receiver) = channel();
    for (devices, rules, revision) in [
        (vec![], vec![1], 1),
        (vec![1], vec![], 1),
        (vec![1], vec![1], 0),
        (vec![0; 4 * 1024 * 1024 + 1], vec![1], 1),
    ] {
        assert!(publisher.publish(devices, rules, revision).await.is_err());
    }
    drop(publisher);
    assert!(receiver.receive().await.is_none());
}
