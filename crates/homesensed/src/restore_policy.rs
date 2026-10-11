//! Keep transitional samples out of the last confirmed nonzero restore level.
use homesense_core::{Command, DeviceId, Percent};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct RestorePolicy(BTreeMap<DeviceId, (Percent, u64)>);
impl RestorePolicy {
    pub(crate) fn accepted(&mut self, id: DeviceId, command: Command, now: u64) {
        let target = match command {
            Command::SetLevel { level, .. } => level,
            Command::Switch(false) => Percent::new(0).unwrap(),
            _ => return,
        };
        self.0.insert(id, (target, now.saturating_add(16_000)));
    }

    /// `level` is the confirmed effective level (zero when supply is off).
    pub(crate) fn remember(&mut self, id: &DeviceId, level: Percent, now: u64) -> bool {
        if let Some(&(target, deadline)) = self.0.get(id) {
            if now < deadline && level != target {
                return false;
            }
            self.0.remove(id);
        }
        level.get() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::LevelTransition;
    #[test]
    fn intermediate_samples_off_and_latest_target_preserve_memory() {
        let mut policy = RestorePolicy::default();
        let id = DeviceId::new("light").unwrap();
        let p = |n| Percent::new(n).unwrap();
        let command = |n| Command::SetLevel {
            level: p(n),
            transition: LevelTransition::Ramp,
        };
        assert!(policy.remember(&id, p(50), 0));
        policy.accepted(id.clone(), command(30), 1);
        assert!(!policy.remember(&id, p(36), 2));
        policy.accepted(id.clone(), command(40), 3);
        assert!(!policy.remember(&id, p(30), 4));
        assert!(policy.remember(&id, p(40), 5));
        policy.accepted(id.clone(), Command::Switch(false), 6);
        assert!(!policy.remember(&id, p(36), 7));
        assert!(!policy.remember(&id, p(0), 8));
        policy.accepted(id.clone(), command(80), 9);
        assert!(policy.remember(&id, p(42), 16_009));
    }
}
