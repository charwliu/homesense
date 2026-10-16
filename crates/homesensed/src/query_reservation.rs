//! Unique in-process physical read claim; not a write ticket or retry permission.
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
pub(crate) struct Permit {pub(super) slot:Arc<AtomicU64>,pub(super) id:u64}
impl Drop for Permit {fn drop(&mut self) {let _=self.slot.compare_exchange(self.id,0,Ordering::AcqRel,Ordering::Acquire);}}

/// The actor owns the bounded wait estimate; dropping the transport's unique
/// permit clears active without allowing an old permit to release a newer read.
pub(super) struct Slot {pub active:Arc<AtomicU64>,pub budget_end_ms:u64}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_read_cannot_release_another_reads_claim() {
        let slot=Arc::new(AtomicU64::new(1));
        let old=Permit {slot:slot.clone(),id:1};
        slot.store(2,Ordering::Release);
        let new=Permit {slot:slot.clone(),id:2};
        drop(old);assert_eq!(slot.load(Ordering::Acquire),2);
        drop(new);assert_eq!(slot.load(Ordering::Acquire),0);
    }
}
