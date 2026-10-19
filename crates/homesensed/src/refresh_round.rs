//! One complete physical query round; freshness is never inferred from sends.
use std::collections::BTreeSet;
#[derive(Clone)]
pub(crate) struct CompletedRound {
    pub generation: crate::admission::TaskGeneration,
    pub started_ms: u64,
    pub completed_ms: u64,
    pub max_age_ms: u64,
}
#[derive(Default)]
pub(super) struct RoundTracker {
    seen: BTreeSet<usize>,
    started: Option<u64>,
    complete: Option<CompletedRound>,
}
impl RoundTracker {
    pub fn discard_completion(&mut self) {
        self.complete = None;
    }
    pub fn forget(&mut self, index: usize) {
        self.seen.remove(&index);
        self.complete = None;
    }
    pub fn contains(&self, index: usize) -> bool {
        self.seen.contains(&index)
    }
    pub fn begin(&mut self, now: u64) {
        self.started.get_or_insert(now);
    }
    pub fn reset(&mut self) {
        self.seen.clear();
        self.started = None;
        self.complete = None;
    }
    /// A timed-out target ends its turn, but cannot certify a successful round.
    pub fn missing(&mut self,index:usize,total:usize)->bool {
        self.seen.insert(index);
        self.complete=None;
        if self.seen.len()==total {self.reset();true} else {false}
    }
    pub fn take(&mut self) -> Option<CompletedRound> {
        self.complete.take()
    }
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        index: usize,
        targets: &[crate::physical_queries::Target],
        truth: &crate::module_truth::ModuleTruth,
        now: u64,
        max_age: u64,
        round_budget: u64,
        generation: &crate::admission::TaskGeneration,
    ) -> bool {
        self.seen.insert(index);
        if self.seen.len() != targets.len() {
            return false;
        }
        if self.started.is_some_and(|start| {
            now.checked_sub(start)
                .is_some_and(|elapsed| elapsed <= round_budget)
        }) && targets
            .iter()
            .all(|target| target.fresh(truth, now, max_age))
        {
            self.complete = Some(CompletedRound {
                generation: generation.clone(),
                started_ms: self.started.unwrap_or(now),
                completed_ms: now,
                max_age_ms: max_age,
            });
        }
        self.seen.clear();
        self.started = None;
        self.complete.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_and_timeouts_do_not_restart_an_unfinished_scan() {
        let mut round=RoundTracker::default();round.begin(10);
        assert!(!round.missing(0,3));
        round.discard_completion();round.begin(100);
        assert!(round.contains(0));assert_eq!(round.started,Some(10));
        assert!(!round.missing(1,3));assert!(round.contains(0));
        assert!(round.missing(2,3));assert!(round.take().is_none());
        assert!(!round.contains(0));round.begin(1000);assert_eq!(round.started,Some(1000));
    }
}
