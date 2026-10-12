//! Shared fade allocation for every producer using one admission owner.
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Used {
    tasks: u64,
    commands: u64,
    rate: u64,
}
#[derive(Clone)]
pub(crate) struct Budget {
    used: Arc<Mutex<Used>>,
    capacity: Capacity,
}
/// Conservative allocation of the one shared physical writer. Counts expanded
/// wire frames, not logical targets; external controllers are outside this budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Capacity {
    pub physical_frames_per_second: u64,
    pub ordinary_reserved_frames_per_second: u64,
    pub fade_frames_per_second: u64,
}
impl Capacity {
    pub(crate) fn for_write_interval(interval: std::time::Duration) -> Self {
        let physical = (1_000_000_000u128 / interval.as_nanos().max(1)) as u64;
        let fade = (physical / 2).min(20);
        Self {
            physical_frames_per_second: physical,
            ordinary_reserved_frames_per_second: physical - fade,
            fade_frames_per_second: fade,
        }
    }
}
pub(crate) struct Reservation {
    owner: Budget,
    commands: u64,
    rate: u64,
}
impl Budget {
    pub(crate) fn for_write_interval(interval: std::time::Duration) -> Self {
        // Admission validates the interval before constructing this budget.
        // Count physical frames, including expansion of combined-light samples.
        // Fades may reserve at most half the physical rate, capped at 20 Hz.
        // On a 1 Hz writer there is no whole-frame fade allocation; retain its
        // one slot for ordinary controls/queries instead of claiming headroom.
        Self {
            used: Default::default(),
            capacity: Capacity::for_write_interval(interval),
        }
    }
    pub(crate) fn reserve(&self, commands: u64, rate: u64) -> Result<Reservation, String> {
        let mut used = self.used.lock().map_err(|_| "fade allocation poisoned")?;
        if commands > 1024
            || rate > self.capacity.fade_frames_per_second
            || used.tasks >= 8
            || used.commands > 1024 - commands
            || used.rate > self.capacity.fade_frames_per_second - rate
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_leaves_ordinary_capacity_even_on_a_slow_writer() {
        for (interval, physical, ordinary, fade) in [
            (20, 50, 30, 20),
            (50, 20, 10, 10),
            (100, 10, 5, 5),
            (200, 5, 3, 2),
            (500, 2, 1, 1),
            (1000, 1, 1, 0),
        ] {
            let budget = Budget::for_write_interval(std::time::Duration::from_millis(interval));
            assert_eq!(
                budget.capacity,
                Capacity {
                    physical_frames_per_second: physical,
                    ordinary_reserved_frames_per_second: ordinary,
                    fade_frames_per_second: fade
                }
            );
            assert!(budget.reserve(10, fade + 1).is_err());
            if fade > 0 {
                let held = budget.reserve(10, fade).unwrap();
                assert!(budget.reserve(10, 1).is_err());
                drop(held);
                assert!(budget.reserve(10, fade).is_ok());
            }
        }
    }
    #[test]
    fn all_producers_share_the_same_rate_and_cancel_releases_only_the_owner() {
        let first = Budget::for_write_interval(std::time::Duration::from_millis(100));
        let second = first.clone();
        let held = first.reserve(10, 3).unwrap();
        let other = second.reserve(10, 2).unwrap();
        assert!(first.reserve(10, 1).is_err());
        drop(held);
        let replacement = first.reserve(10, 3).unwrap();
        assert!(second.reserve(10, 1).is_err());
        drop(other);
        assert!(second.reserve(10, 2).is_ok());
        drop(replacement);
    }
}
