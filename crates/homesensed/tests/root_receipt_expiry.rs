//! Root store contract over SQLite only; trusted synthetic clock, no Web/hardware.
use homesensed::{web_receipts::{Receipts,Endpoint,Claim},web_receipt_policy::{Policy,Expiry}};
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
#[test]
fn once_expired_a_receipt_lookup_stays_expired_across_clock_rollback_and_reopen() {
 let root=std::env::temp_dir().join(format!("root-receipt-expiry-{}",uuid::Uuid::new_v4()));
 std::fs::create_dir(&root).unwrap();let path=root.join("web.sqlite");
 let time=Arc::new(AtomicU64::new(100));let captured=time.clone();
 let clock:Arc<dyn Fn()->Result<u64,String>+Send+Sync>=Arc::new(move||Ok(captured.load(Ordering::SeqCst)));
 let policy=Policy{max_records:2,expiry:Expiry::MarkExpiredPreserveFence{window_ms:10}};
 let mut receipts=Receipts::open_with_clock(&path,policy,clock.clone()).unwrap();
 assert_eq!(receipts.claim_scoped("synthetic-principal",&Endpoint::SceneActions,"one","original").unwrap().claim,Claim::New);
 receipts.finish_scoped("synthetic-principal",&Endpoint::SceneActions,"one","original-response").unwrap();
 time.store(111,Ordering::SeqCst);
 let expired=receipts.claim_scoped("synthetic-principal",&Endpoint::SceneActions,"one","original").unwrap();
 assert!(expired.expired);assert_eq!(expired.claim,Claim::Existing(Some("original-response".into())));
 time.store(100,Ordering::SeqCst);drop(receipts);
 let receipts=Receipts::open_with_clock(&path,Policy::default(),clock).unwrap();
 let view=receipts.lookup_scoped("synthetic-principal",&Endpoint::SceneActions,"one").unwrap().unwrap();
 drop(receipts);std::fs::remove_dir_all(root).unwrap();
 assert_eq!(view.response.as_deref(),Some("original-response"));
 assert!(view.expired,"persisted expiry must survive time rollback and switching policy toNever");
}

#[test]
fn expiry_first_observed_by_lookup_does_not_revive_after_clock_rollback() {
 let root=std::env::temp_dir().join(format!("root-receipt-expiry-{}",uuid::Uuid::new_v4()));
 std::fs::create_dir(&root).unwrap();let path=root.join("web.sqlite");
 let time=Arc::new(AtomicU64::new(100));let captured=time.clone();
 let clock:Arc<dyn Fn()->Result<u64,String>+Send+Sync>=Arc::new(move||Ok(captured.load(Ordering::SeqCst)));
 let policy=Policy{max_records:2,expiry:Expiry::MarkExpiredPreserveFence{window_ms:10}};
 let mut receipts=Receipts::open_with_clock(&path,policy,clock.clone()).unwrap();
 assert_eq!(receipts.claim_scoped("synthetic-principal",&Endpoint::SceneActions,"one","original").unwrap().claim,Claim::New);
 receipts.finish_scoped("synthetic-principal",&Endpoint::SceneActions,"one","original-response").unwrap();
 time.store(111,Ordering::SeqCst);
 let expired=receipts.lookup_scoped("synthetic-principal",&Endpoint::SceneActions,"one").unwrap().unwrap();
 assert!(expired.expired);assert_eq!(expired.response,Some("original-response".into()));
 time.store(100,Ordering::SeqCst);drop(receipts);
 let receipts=Receipts::open_with_clock(&path,Policy::default(),clock).unwrap();
 let view=receipts.lookup_scoped("synthetic-principal",&Endpoint::SceneActions,"one").unwrap().unwrap();
 drop(receipts);std::fs::remove_dir_all(root).unwrap();
 assert_eq!(view.response.as_deref(),Some("original-response"));
 assert!(view.expired,"persisted expiry must survive time rollback and switching policy toNever");
}
