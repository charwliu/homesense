//! Exact source capability for a single immutable administrative publication.
//! Valid only before the actor enters the STOP/atomic-switch barrier.
use crate::web_auth::{Grant, ModuleOffLease};
use sha2::{Digest, Sha256};
#[derive(Clone)]
pub(crate) struct Source {
    proof: std::sync::Arc<PublicationProof>,
}
struct PublicationProof {
    lease: ModuleOffLease,
    expected: u64,
    devices: [u8; 32],
    rules: [u8; 32],
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicationSource(<opaque>)")
    }
}
impl Source {
    pub(crate) fn capture(
        lease: ModuleOffLease,
        grant: &Grant,
        devices: &[u8],
        rules: &[u8],
        expected: u64,
    ) -> Option<Self> {
        if !lease.matches_grant(grant) || !lease.is_administrator() || !lease.valid() {
            return None;
        }
        Some(Self {
            proof: std::sync::Arc::new(PublicationProof {
                lease,
                expected,
                devices: Sha256::digest(devices).into(),
                rules: Sha256::digest(rules).into(),
            }),
        })
    }
    pub(crate) fn controller(&self) -> uuid::Uuid {
        self.proof.lease.controller()
    }
    pub(crate) fn valid(&self) -> bool {
        self.proof.lease.is_administrator() && self.proof.lease.valid()
    }
    pub(crate) fn matches(&self, devices: &[u8], rules: Option<&[u8]>, expected: u64) -> bool {
        self.proof.expected == expected
            && self.proof.devices == <[u8; 32]>::from(Sha256::digest(devices))
            && rules
                .is_some_and(|rules| self.proof.rules == <[u8; 32]>::from(Sha256::digest(rules)))
            && self.valid()
    }
    pub(crate) fn with_valid<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        self.proof.lease.with_valid(operation)
    }
}

/// Single-use authority transferred to an accepted publication for one already
/// queued safety STOP. It cannot authorize other commands, targets or epochs.
/// No token is re-authenticated after the publication barrier has been accepted.
#[derive(Debug)]
pub(crate) struct DrainStop {
    binding: Box<StopBinding>,
}
#[derive(Debug)]
struct StopBinding {
    reserved: homesense_core::queue::QueuedCommand,
    control_epoch: u64,
}
impl DrainStop {
    pub(crate) fn capture(
        reserved: homesense_core::queue::QueuedCommand,
        control_epoch: u64,
    ) -> Option<Self> {
        if !reserved.is_stop() || control_epoch <= reserved.epochs.control {
            return None;
        }
        Some(Self {
            binding: Box::new(StopBinding {
                reserved,
                control_epoch,
            }),
        })
    }
    pub(crate) fn matches_reserved(&self, command: &homesense_core::queue::QueuedCommand) -> bool {
        command.is_stop() && command == &self.binding.reserved
    }
    pub(crate) fn matches_write(&self, command: &homesense_core::queue::QueuedCommand) -> bool {
        let mut expected = self.binding.reserved.clone();
        expected.epochs.control = self.binding.control_epoch;
        command.is_stop() && command == &expected
    }
}
#[cfg(test)]
#[test]
fn reserved_stop_authority_cannot_be_reused_for_a_different_ticket() {
    use homesense_core::{
        Command, DeviceId, MotorMotion,
        queue::{Epochs, Origin, QueuedCommand},
    };
    let original = QueuedCommand {
        id: 1,
        device_id: DeviceId::new("synthetic-motor").unwrap(),
        command: Command::Motor(MotorMotion::Stop),
        origin: Origin::Http,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        deadline_ms: 100,
        run_id: None,
    };
    let lease = DrainStop::capture(original.clone(), 2).unwrap();
    assert!(lease.matches_reserved(&original));
    let mut written = original.clone();
    written.epochs.control = 2;
    assert!(lease.matches_write(&written));
    let mut wrong = written.clone();
    wrong.command = Command::Switch(true);
    assert!(!lease.matches_write(&wrong));
    assert!(DrainStop::capture(wrong, 2).is_none());
    let mut wrong = written.clone();
    wrong.id += 1;
    assert!(!lease.matches_write(&wrong));
    let mut wrong = written.clone();
    wrong.device_id = DeviceId::new("other-motor").unwrap();
    assert!(!lease.matches_write(&wrong));
    let mut wrong = written.clone();
    wrong.deadline_ms += 1;
    assert!(!lease.matches_write(&wrong));
    let mut wrong = written.clone();
    wrong.epochs.configuration += 1;
    assert!(!lease.matches_write(&wrong));
    let mut wrong = written.clone();
    wrong.epochs.transport += 1;
    assert!(!lease.matches_write(&wrong));
    assert!(!lease.matches_write(&original));
    assert!(std::mem::size_of::<DrainStop>() <= 2 * std::mem::size_of::<usize>());
    assert_eq!(std::mem::size_of::<Source>(), std::mem::size_of::<usize>());
}
