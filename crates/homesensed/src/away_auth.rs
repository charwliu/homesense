//! Source-specific Away authorization. HAP never borrows a Web credential.
use crate::web_auth::ModuleOffLease;
use homesense_core::queue::Origin;
use homesense_hap::storage::IdentityStore;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;
#[derive(Clone)]
pub struct HapLease {
    identity: IdentityStore,
    controller: Uuid,
    public_key: [u8; 32],
    devices: BTreeSet<String>,
    active: Arc<AtomicBool>,
}
impl std::fmt::Debug for HapLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HapAwayLease(<opaque>)")
    }
}
impl HapLease {
    pub(crate) fn new(
        identity: IdentityStore,
        controller: Uuid,
        devices: BTreeSet<String>,
        active: Arc<AtomicBool>,
    ) -> Option<Self> {
        let public_key = identity.controller_public_key(controller).ok()??;
        Some(Self {
            identity,
            controller,
            public_key,
            devices,
            active,
        })
    }
    fn valid(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self
                .identity
                .controller_public_key(self.controller)
                .ok()
                .flatten()
                == Some(self.public_key)
    }
}
#[derive(Clone, Debug)]
pub enum Authorization {
    Web(ModuleOffLease),
    Hap(HapLease),
}
impl From<ModuleOffLease> for Authorization {
    fn from(lease: ModuleOffLease) -> Self {
        Self::Web(lease)
    }
}
impl Authorization {
    pub fn controller(&self) -> Uuid {
        match self {
            Self::Web(lease) => lease.controller(),
            Self::Hap(lease) => lease.controller,
        }
    }
    pub fn devices(&self) -> &BTreeSet<String> {
        match self {
            Self::Web(lease) => lease.devices(),
            Self::Hap(lease) => &lease.devices,
        }
    }
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Web(lease) => lease.valid(),
            Self::Hap(lease) => lease.valid(),
        }
    }
    pub(crate) fn origin(&self) -> Origin {
        match self {
            Self::Web(_) => Origin::Http,
            Self::Hap(_) => Origin::Hap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hap::{
        pairing::{Pairing, Permissions},
        storage::Storage,
    };
    #[tokio::test]
    async fn hap_capability_is_bridge_local_key_bound_revocable_and_retired() {
        let root = std::env::temp_dir().join(format!("away-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut identity = IdentityStore::open(root.join("first")).await.unwrap();
        identity.save_config(&hap::Config::default()).await.unwrap();
        let mut other = IdentityStore::open(root.join("second")).await.unwrap();
        other.save_config(&hap::Config::default()).await.unwrap();
        let controller = Uuid::new_v4();
        let active = Arc::new(AtomicBool::new(true));
        let devices = BTreeSet::from(["synthetic:1".into()]);
        assert!(
            HapLease::new(
                identity.clone(),
                controller,
                devices.clone(),
                active.clone()
            )
            .is_none()
        );
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [5; 32]))
            .await
            .unwrap();
        assert!(
            HapLease::new(other.clone(), controller, devices.clone(), active.clone()).is_none()
        );
        let lease = HapLease::new(
            identity.clone(),
            controller,
            devices.clone(),
            active.clone(),
        )
        .unwrap();
        assert!(lease.valid());
        identity.delete_pairing(&controller).await.unwrap();
        assert!(!lease.valid());
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [6; 32]))
            .await
            .unwrap();
        assert!(!lease.valid());
        let replacement =
            HapLease::new(identity.clone(), controller, devices, active.clone()).unwrap();
        assert!(replacement.valid());
        active.store(false, Ordering::Release);
        assert!(!replacement.valid());
        drop((identity, other, lease, replacement));
        std::fs::remove_dir_all(root).unwrap();
    }
}
