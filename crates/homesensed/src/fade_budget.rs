//! Shared fade allocation for every producer using one admission owner.
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Used {
    tasks: u64,
    commands: u64,
    rate: u64,
}
#[derive(Clone)]
pub(crate) struct Budget { used: Arc<Mutex<Used>>, max_rate: u64 }
pub(crate) struct Reservation {
    owner: Budget,
    commands: u64,
    rate: u64,
}
impl Budget {
    pub(crate) fn for_write_interval(interval: std::time::Duration) -> Self {
        // Admission validates the interval before constructing this budget.
        // Count physical frames, including expansion of combined-light samples.
        let max_rate = (1_000_000_000u128 / interval.as_nanos().max(1)).min(20) as u64;
        Self { used: Default::default(), max_rate }
    }
    pub(crate) fn reserve(&self, commands: u64, rate: u64) -> Result<Reservation, String> {
        let mut used = self.used.lock().map_err(|_| "fade allocation poisoned")?;
        if commands > 1024
            || rate > self.max_rate
            || used.tasks >= 8
            || used.commands > 1024 - commands
            || used.rate > self.max_rate - rate
        {
            return Err("shared fade allocation exhausted".into());
        }
        used.tasks += 1;
        used.commands += commands;
        used.rate += rate;
        Ok(Reservation {
            owner: self.clone(),
            commands,
            rate,
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut used) = self.owner.used.lock() {
            used.tasks -= 1;
            used.commands -= self.commands;
            used.rate -= self.rate;
        }
    }
}
