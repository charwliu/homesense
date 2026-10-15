//! Keep transitional samples out of the last confirmed nonzero restore level.
use homesense_core::{Command, DeviceId, Percent, queue::Epochs};
use std::collections::BTreeMap;

struct Transition {
    target: Percent,
    deadline: u64,
    owner: Option<(Vec<u64>, Epochs)>,
}
#[derive(Default)]
pub(crate) struct RestorePolicy(BTreeMap<DeviceId, Transition>);
impl RestorePolicy {
    pub(crate) fn invalidate_device(&mut self,id:&DeviceId) {self.0.remove(id);}
    pub(crate) fn accepted(&mut self, id: DeviceId, command: Command, now: u64) {
        let target = match command {
            Command::SetLevel { level, .. } => level,
            Command::Switch(false) => Percent::new(0).unwrap(),
            _ => return,
        };
        self.0.insert(
            id,
            Transition {
                target,
                deadline: now.saturating_add(16_000),
                owner: None,
            },
        );
    }

    pub(crate) fn bind(&mut self, id: &DeviceId, commands: Vec<u64>, epochs: Epochs) {
        if let Some(entry) = self.0.get_mut(id) {
            entry.owner = Some((commands, epochs));
        }
    }
    /// Every target is bounded and owned by a still-valid accepted command set.
    pub(crate) fn targets(
        &mut self,
        epochs: Epochs,
        now: u64,
        mut status: impl FnMut(
            u64,
        ) -> Result<
            Option<homesense_storage::journal::Status>,
            homesense_storage::Error,
        >,
    ) -> Result<BTreeMap<DeviceId, Percent>, homesense_storage::Error> {
        let mut targets = BTreeMap::new();
        let mut remove = vec![];
        for (id, transition) in &self.0 {
            let valid = if let Some((commands, scope)) = &transition.owner {
                let mut valid =
                    scope == &epochs && now < transition.deadline && !commands.is_empty();
                for command in commands {
                    valid &= matches!(
                        status(*command)?,
                        Some(
                            homesense_storage::journal::Status::Accepted
                                | homesense_storage::journal::Status::Writing
                                | homesense_storage::journal::Status::Written
                                | homesense_storage::journal::Status::Observed
                        )
                    );
                }
                valid
            } else {
                false
            };
            if valid {
                targets.insert(id.clone(), transition.target);
            } else {
                remove.push(id.clone());
            }
        }
        for id in remove {
            self.0.remove(&id);
        }
        Ok(targets)
    }
    /// Only post-write, settled physical feedback may update restore preference.
    pub(crate) fn remember_settled(
        &mut self,
        id: &DeviceId,
        level: Percent,
        now: u64,
        settled: bool,
    ) -> bool {
        if !settled {
            return false;
        }
        if let Some(transition) = self.0.get(id) {
            if now < transition.deadline && level != transition.target {
                return false;
            }
            self.0.remove(id);
        }
        level.get() > 0
    }
    #[cfg(test)]
    fn remember(&mut self, id: &DeviceId, level: Percent, now: u64) -> bool {
        self.remember_settled(id, level, now, true)
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
    #[test]
    fn targets_require_live_owner_and_do_not_survive_cancel_failure_epoch_or_timeout() {
        use homesense_storage::journal::Status;
        let id = DeviceId::new("light").unwrap();
        let scope = Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        };
        for invalid in [
            None,
            Some(Status::Cancelled),
            Some(Status::Failed),
            Some(Status::Uncertain),
        ] {
            let mut policy = RestorePolicy::default();
            policy.accepted(
                id.clone(),
                Command::SetLevel {
                    level: Percent::new(100).unwrap(),
                    transition: homesense_core::LevelTransition::Ramp,
                },
                0,
            );
            policy.bind(&id, vec![1, 2], scope);
            assert!(
                policy
                    .targets(scope, 1, |command| Ok(if command == 1 {
                        Some(Status::Written)
                    } else {
                        invalid
                    }))
                    .unwrap()
                    .is_empty()
            );
            assert!(policy.remember_settled(&id, Percent::new(17).unwrap(), 2, true));
        }
        for next in [
            Epochs {
                transport: 2,
                ..scope
            },
            Epochs {
                configuration: 2,
                ..scope
            },
            Epochs {
                control: 2,
                ..scope
            },
        ] {
            let mut policy = RestorePolicy::default();
            policy.accepted(id.clone(), Command::Switch(false), 0);
            policy.bind(&id, vec![1], scope);
            assert!(
                policy
                    .targets(next, 1, |_| Ok(Some(Status::Written)))
                    .unwrap()
                    .is_empty()
            );
        }
        let mut policy = RestorePolicy::default();
        policy.accepted(id.clone(), Command::Switch(false), 0);
        policy.bind(&id, vec![1], scope);
        assert_eq!(
            policy
                .targets(scope, 15999, |_| Ok(Some(Status::Written)))
                .unwrap()[&id]
                .get(),
            0
        );
        assert!(
            policy
                .targets(scope, 16000, |_| Ok(Some(Status::Written)))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn matching_sample_cannot_settle_unwritten_owner_and_external_value_can_follow_completed_target()
     {
        use homesense_storage::journal::Status;
        let id = DeviceId::new("light").unwrap();
        let scope = Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        };
        let mut policy = RestorePolicy::default();
        policy.accepted(
            id.clone(),
            Command::SetLevel {
                level: Percent::new(40).unwrap(),
                transition: homesense_core::LevelTransition::Ramp,
            },
            0,
        );
        policy.bind(&id, vec![1], scope);
        assert!(!policy.remember_settled(&id, Percent::new(40).unwrap(), 1, false));
        assert_eq!(
            policy
                .targets(scope, 1, |_| Ok(Some(Status::Accepted)))
                .unwrap()[&id]
                .get(),
            40
        );
        assert!(!policy.remember_settled(&id, Percent::new(36).unwrap(), 2, true));
        assert!(policy.remember_settled(&id, Percent::new(40).unwrap(), 3, true));
        assert!(
            policy
                .targets(scope, 4, |_| Ok(Some(Status::Written)))
                .unwrap()
                .is_empty()
        );
        assert!(policy.remember_settled(&id, Percent::new(17).unwrap(), 5, true));
        assert!(!policy.remember_settled(&id, Percent::new(0).unwrap(), 6, true));
    }
}
