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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub controller: Uuid,
    pub role: Role,
    pub scenes: BTreeSet<String>,
    pub devices: BTreeSet<String>,
}
struct Credential {
    hash: [u8; 32],
    grant: Grant,
}
#[derive(Default)]
pub struct Tokens {
    entries: RwLock<BTreeMap<Uuid, Credential>>,
    sessions: RwLock<BTreeMap<[u8; 32], Session>>,
}
#[derive(Clone)]
struct Session {
    credential: [u8; 32],
    csrf: [u8; 32],
    expires: std::time::Instant,
}
pub struct SessionSecret {
    pub cookie: String,
    pub csrf: String,
}
fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
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
            || grant.devices.len() > 256
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
    pub fn login(&self, header: &str) -> Option<SessionSecret> {
        self.authenticate(header)?;
        let credential = digest(header.strip_prefix("Bearer ")?);
        let mut sessions = self.sessions.write().ok()?;
        let now = std::time::Instant::now();
        sessions.retain(|_, s| s.expires > now);
        if sessions.len() >= 128 {
            return None;
        }
        let cookie = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let csrf = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sessions.insert(
            digest(&cookie),
            Session {
                credential,
                csrf: digest(&csrf),
                expires: now + std::time::Duration::from_secs(900),
            },
        );
        Some(SessionSecret { cookie, csrf })
    }
    pub fn session(&self, cookie: &str, csrf: Option<&str>, write: bool) -> Option<Grant> {
        if cookie.len() != 64 {
            return None;
        }
        let session = self.sessions.read().ok()?.get(&digest(cookie))?.clone();
        if session.expires <= std::time::Instant::now()
            || (write && !bool::from(session.csrf.ct_eq(&digest(csrf?))))
        {
            return None;
        }
        let entries = self.entries.read().ok()?;
        entries
            .values()
            .find(|v| bool::from(v.hash.ct_eq(&session.credential)))
            .map(|v| v.grant.clone())
    }
    pub fn logout(&self, cookie: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(&digest(cookie));
        }
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
    pub fn may_read_device(&self, id: &str) -> bool {
        self.devices.contains(id)
    }
    pub fn may_control_device(&self, id: &str) -> bool {
        self.role != Role::Viewer && self.may_read_device(id)
    }
    pub fn may_read(&self, scene: &str) -> bool {
        self.scenes.contains(scene)
    }
    pub fn may_control(&self, scene: &str) -> bool {
        self.role != Role::Viewer && self.may_read(scene)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_sessions_fail_closed_and_capacity_is_bounded() {
        let tokens = Tokens::default();
        let id = Uuid::new_v4();
        let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
        tokens
            .insert(
                id,
                secret,
                Grant {
                    controller: id,
                    role: Role::Viewer,
                    scenes: BTreeSet::new(),
                    devices: BTreeSet::new(),
                },
            )
            .unwrap();
        let header = format!("Bearer {secret}");
        let first = tokens.login(&header).unwrap();
        for _ in 1..128 {
            assert!(tokens.login(&header).is_some());
        }
        assert!(tokens.login(&header).is_none());
        for session in tokens.sessions.write().unwrap().values_mut() {
            session.expires = std::time::Instant::now() - std::time::Duration::from_secs(1);
        }
        assert!(tokens.session(&first.cookie, None, false).is_none());
        assert!(tokens.login(&header).is_some());
        assert_eq!(tokens.sessions.read().unwrap().len(), 1);
    }
}
