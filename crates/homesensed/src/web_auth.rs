//! Revocable scoped bearer credentials. The HTTP owner must additionally check
//! origin/host and transport; never accept credentials from URLs or JSON bodies.
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};
use subtle::ConstantTimeEq;
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Role {
    Viewer,
    Member,
    Administrator,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub controller: Uuid,
    pub role: Role,
    pub scenes: BTreeSet<String>,
    pub devices: BTreeSet<String>,
}
#[derive(serde::Serialize)]
pub struct CredentialSummary {
    pub id:Uuid,
    pub grant:Grant,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequest {
    pub role:Role,
    pub scenes:BTreeSet<String>,
    pub devices:BTreeSet<String>,
}
#[derive(serde::Serialize)]
pub struct IssuedCredential {pub id:Uuid,pub secret:String,pub grant:Grant}
struct Credential {
    generation: Uuid,
    hash: [u8; 32],
    grant: Grant,
}
#[derive(Default)]
pub struct Tokens {
    entries: RwLock<BTreeMap<Uuid, Credential>>,
    persistence: Option<std::sync::Mutex<crate::web_credentials::Credentials>>,
    sessions: RwLock<BTreeMap<[u8; 32], Session>>,
}
#[derive(Clone)]
struct Session {
    generation: Uuid,
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
fn administrator<'a>(entries:&'a BTreeMap<Uuid,Credential>, sessions:&BTreeMap<[u8;32],Session>, bearer:Option<&str>,cookie:Option<&str>,csrf:Option<&str>)->Option<&'a Grant> {
    let credential=match (bearer,cookie) {
        (Some(header),None)=>{
            let secret=header.strip_prefix("Bearer ")?;
            if !(32..=256).contains(&secret.len()){return None;}
            let hash=digest(secret);
            entries.values().find(|entry|bool::from(entry.hash.ct_eq(&hash)))?
        },
        (None,Some(cookie))=>{
            if cookie.len()!=64{return None;}
            let session=sessions.get(&digest(cookie))?;
            if session.expires<=std::time::Instant::now() || !bool::from(session.csrf.ct_eq(&digest(csrf?))){return None;}
            entries.values().find(|entry|entry.generation==session.generation && bool::from(entry.hash.ct_eq(&session.credential)))?
        },
        _=>return None,
    };
    (credential.grant.role==Role::Administrator).then_some(&credential.grant)
}
impl Tokens {
    /// Administrative projection; intentionally excludes hashes and session material.
    pub fn summaries(&self)->Result<Vec<CredentialSummary>,String> {
        let entries=self.entries.read().map_err(|_|"credential lock unavailable")?;
        Ok(entries.iter().map(|(id,c)|CredentialSummary{id:*id,grant:c.grant.clone()}).collect())
    }
    /// Load a private credential database owned by this service instance.
    /// Sessions are deliberately process-local and require login after restart.
    pub fn open(path:&std::path::Path)->Result<Self,String> {
        let store=crate::web_credentials::Credentials::open(path)?;
        Self::from_store(store)
    }
    fn from_store(store:crate::web_credentials::Credentials)->Result<Self,String> {
        let records=store.list()?;
        let entries=records.into_iter().map(|r|(r.id,Credential {
            generation:Uuid::new_v4(),hash:r.digest,grant:r.grant,
        })).collect();
        Ok(Self{entries:RwLock::new(entries),sessions:RwLock::new(BTreeMap::new()),persistence:Some(std::sync::Mutex::new(store))})
    }

    /// The supplied grant is used only on first initialization, never on restart.
    pub fn open_bootstrapped(path:&std::path::Path,secret:&str,grant:Grant)->Result<Self,String> {
        let id=Uuid::new_v4();
        // Reuse exactly the same secret and scope validation as ordinary inserts.
        Self::default().insert(id,secret,grant.clone())?;
        let mut store=crate::web_credentials::Credentials::open(path)?;
        store.bootstrap(&crate::web_credentials::Record{id,digest:digest(secret),grant})?;
        Self::from_store(store)
    }

    /// Caller obtains a random secret out of band; only its digest is retained.
    /// Minimum 32 bytes is an input bound, not a claim of password strength.
    pub fn insert(&self, id: Uuid, secret: &str, grant: Grant) -> Result<(), String> {
        crate::web_credentials::validate(&grant)?;
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
        let mut sessions=self.sessions.write().map_err(|_|"session lock unavailable")?;
        if let Some(store)=&self.persistence {
            store.lock().map_err(|_|"credential storage lock unavailable")?
                .save(&crate::web_credentials::Record{id,digest:hash,grant:grant.clone()})?;
        }
        if let Some(previous)=entries.get(&id) {sessions.retain(|_,session|session.generation!=previous.generation);}
        entries.insert(id, Credential { generation:Uuid::new_v4(), hash, grant });
        Ok(())
    }
    pub fn revoke(&self, id: Uuid) -> Result<(), String> {
        let mut entries=self.entries.write().map_err(|_|"credential lock unavailable")?;
        let mut sessions=self.sessions.write().map_err(|_|"session lock unavailable")?;
        if let Some(store)=&self.persistence {
            store.lock().map_err(|_|"credential storage lock unavailable")?.revoke(id)?;
        }
        if let Some(previous)=entries.remove(&id) {sessions.retain(|_,session|session.generation!=previous.generation);}
        Ok(())
    }
    /// Recheck the requesting credential while holding the mutation lock.
    /// The last administrator cannot be removed through the management API.
    pub fn revoke_managed(&self, id: Uuid, bearer: Option<&str>, cookie: Option<&str>, csrf: Option<&str>) -> Result<(), &'static str> {
        let mut entries=self.entries.write().map_err(|_|"credential_store_unavailable")?;
        let mut sessions=self.sessions.write().map_err(|_|"credential_store_unavailable")?;
        if administrator(&entries,&sessions,bearer,cookie,csrf).is_none() {return Err("administrator_required");}
        let target=entries.get(&id).ok_or("credential_not_found")?;
        if target.grant.role==Role::Administrator && entries.values().filter(|entry|entry.grant.role==Role::Administrator).count()==1 {return Err("last_administrator");}
        if let Some(store)=&self.persistence {
            store.lock().map_err(|_|"credential_store_unavailable")?.revoke(id).map_err(|_|"credential_store_unavailable")?;
        }
        let previous=entries.remove(&id).ok_or("credential_not_found")?;
        sessions.retain(|_,session|session.generation!=previous.generation);
        Ok(())
    }
    /// Issue a server-generated secret once; persist only its digest.
    /// Delegation inherits the actor identity and cannot expand the caller's scope.
    pub fn issue(&self, request: CredentialRequest, bearer: Option<&str>, cookie: Option<&str>, csrf: Option<&str>) -> Result<IssuedCredential, &'static str> {
        let mut entries=self.entries.write().map_err(|_|"credential_store_unavailable")?;
        let sessions=self.sessions.read().map_err(|_|"credential_store_unavailable")?;
        let parent=administrator(&entries,&sessions,bearer,cookie,csrf).ok_or("administrator_required")?;
        if !request.devices.is_subset(&parent.devices) || !request.scenes.is_subset(&parent.scenes) {return Err("scope_expansion_forbidden");}
        let grant=Grant{controller:parent.controller,role:request.role,devices:request.devices,scenes:request.scenes};
        crate::web_credentials::validate(&grant).map_err(|_|"invalid_credential_scope")?;
        if entries.len()>=128 {return Err("credential_capacity_reached");}
        let id=Uuid::new_v4();let secret=format!("{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple());let hash=digest(&secret);
        if entries.contains_key(&id) || entries.values().any(|entry|bool::from(entry.hash.ct_eq(&hash))) {return Err("credential_collision");}
        if let Some(store)=&self.persistence {store.lock().map_err(|_|"credential_store_unavailable")?.save(&crate::web_credentials::Record{id,digest:hash,grant:grant.clone()}).map_err(|_|"credential_store_unavailable")?;}
        entries.insert(id,Credential{generation:Uuid::new_v4(),hash,grant:grant.clone()});
        Ok(IssuedCredential{id,secret,grant})
    }
    pub fn login(&self, header: &str) -> Option<SessionSecret> {
        let secret=header.strip_prefix("Bearer ")?;
        if !(32..=256).contains(&secret.len()) {return None;}
        let credential=digest(secret);
        // Hold the credential read lock through session insertion so revocation
        // cannot interleave between authentication and session creation.
        let entries=self.entries.read().ok()?;
        let mut generation=None;
        for entry in entries.values() {
            if bool::from(entry.hash.ct_eq(&credential)) {generation=Some(entry.generation);}
        }
        let generation=generation?;
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
                generation,
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
            .find(|v| v.generation==session.generation && bool::from(v.hash.ct_eq(&session.credential)))
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
