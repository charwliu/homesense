//! Revocable scoped bearer credentials. The HTTP owner must additionally check
//! origin/host and transport; never accept credentials from URLs or JSON bodies.
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};
use subtle::ConstantTimeEq;
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Viewer,
    Member,
    Administrator,
}
#[derive(Clone, Debug)]
pub struct Grant {
    pub controller: Uuid,
    pub role: Role,
    pub scenes: BTreeSet<String>,
}
struct Credential {
    hash: [u8; 32],
    grant: Grant,
}
#[derive(Default)]
pub struct Tokens {
    entries: RwLock<BTreeMap<Uuid, Credential>>,
}
impl Tokens {
    /// Caller obtains a random secret out of band; only its digest is retained.
    /// Minimum 32 bytes is an input bound, not a claim of password strength.
    pub fn insert(&self, id: Uuid, secret: &str, grant: Grant) -> Result<(), String> {
        if secret.len() < 32
            || secret.len() > 256
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || grant.scenes.len() > 4095
        {
            return Err("invalid credential configuration".into());
        }
        let mut entries = self
            .entries
            .write()
            .map_err(|_| "credential lock unavailable")?;
        if entries.len() >= 128 && !entries.contains_key(&id) {
            return Err("credential capacity reached".into());
        }
        let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        if entries
            .iter()
            .any(|(other, credential)| *other != id && bool::from(hash.ct_eq(&credential.hash)))
        {
            return Err("credential already assigned".into());
        }
        entries.insert(id, Credential { hash, grant });
        Ok(())
    }
    pub fn revoke(&self, id: Uuid) -> Result<(), String> {
        self.entries
            .write()
            .map_err(|_| "credential lock unavailable")?
            .remove(&id);
        Ok(())
    }
    pub fn authenticate(&self, header: &str) -> Option<Grant> {
        let secret = header.strip_prefix("Bearer ")?;
        if secret.len() < 32 || secret.len() > 256 {
            return None;
        }
        let digest: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let entries = self.entries.read().ok()?;
        // Compare every configured credential without a prefix/early-match test.
        let mut result = None;
        for credential in entries.values() {
            if bool::from(digest.ct_eq(&credential.hash)) {
                result = Some(credential.grant.clone());
            }
        }
        result
    }
}
impl Grant {
    pub fn may_read(&self, scene: &str) -> bool {
        self.scenes.contains(scene)
    }
    pub fn may_control(&self, scene: &str) -> bool {
        self.role != Role::Viewer && self.may_read(scene)
    }
}
