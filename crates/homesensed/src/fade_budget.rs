//! Shared fade allocation for every producer using one admission owner.
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Used {
    tasks: u64,
    commands: u64,
    rate: u64,
}
#[derive(Clone, Default)]
pub(crate) struct Budget(Arc<Mutex<Used>>);
pub(crate) struct Reservation {
    owner: Budget,
    commands: u64,
    rate: u64,
}
impl Budget {
    pub(crate) fn reserve(&self, commands: u64, rate: u64) -> Result<Reservation, String> {
        let mut used = self.0.lock().map_err(|_| "fade allocation poisoned")?;
        if commands > 1024
            || rate > 20
            || used.tasks >= 8
            || used.commands > 1024 - commands
            || used.rate > 20 - rate
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
        if let Ok(mut used) = self.owner.0.lock() {
            used.tasks -= 1;
            used.commands -= self.commands;
            used.rate -= self.rate;
        }
    }
}
