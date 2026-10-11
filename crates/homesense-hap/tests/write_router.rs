use hap::server::{WriteBatchHandler, WriteStatus};
use homesense_hap::write_router::WriteRouter;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Owner(Arc<AtomicUsize>);
impl WriteBatchHandler for Owner {
    fn write(&self, _: uuid::Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[tokio::test]
async fn routes_entire_requests_and_rejects_mixed_unknown_and_duplicates_without_effects() {
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));
    let router = WriteRouter::new(vec![
        (vec![(2, 9), (2, 10)], Arc::new(Owner(a.clone()))),
        (vec![(3, 9)], Arc::new(Owner(b.clone()))),
    ])
    .unwrap();
    let caller = uuid::Uuid::new_v4();
    for body in [
        br#"{"characteristics":[{"aid":2,"iid":9,"value":true},{"aid":3,"iid":9,"value":true}]}"#
            .as_slice(),
        br#"{"characteristics":[{"aid":2,"iid":9},{"aid":2,"iid":9}]}"#,
        br#"{"characteristics":[{"aid":9,"iid":9}]}"#,
    ] {
        assert!(router.write_async(caller, body).await.is_err());
    }
    assert_eq!(a.load(Ordering::SeqCst) + b.load(Ordering::SeqCst), 0);
    router.write_async(caller,br#"{"characteristics":[{"aid":2,"iid":9,"value":true},{"aid":2,"iid":10,"value":30}]}"#).await.unwrap();
    router
        .write_async(
            caller,
            br#"{"characteristics":[{"aid":3,"iid":9,"ev":true}]}"#,
        )
        .await
        .unwrap();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
    assert!(router.write(caller, b"{}").is_err());
}
