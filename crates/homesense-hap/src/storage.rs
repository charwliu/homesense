//! Dedicated identity database; never included in ordinary configuration exports.
use async_trait::async_trait;
use hap::{Config, Error, Result, pairing::Pairing, storage::Storage};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
const APPLICATION_ID: i64 = 0x48534850;

#[derive(Clone)]
pub struct IdentityStore {
    db: Arc<Mutex<Connection>>,
}
pub(crate) fn storage_error<E>(_: E) -> Error {
    Error::Storage
}
impl IdentityStore {
    /// Exact in-process database owner identity; does not expose pairings.
    pub fn same_owner(&self,other:&Self)->bool{Arc::ptr_eq(&self.db,&other.db)}

    pub async fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref().to_owned();
        let db = tokio::task::spawn_blocking(move || -> Result<Connection> {
            private_directory(&directory)?;
            let directory = std::fs::canonicalize(&directory)?;
            let path = directory.join("identity.sqlite");
            private_file(&path)?;
            let mut db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(storage_error)?;
            db.busy_timeout(Duration::from_millis(250)).map_err(storage_error)?;
            let app: i64 = db.pragma_query_value(None, "application_id", |r| r.get(0)).map_err(storage_error)?;
            let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(storage_error)?;
            if app == 0 && version == 0 {
                let count: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).map_err(storage_error)?;
                if count != 0 { return Err(Error::Storage); }
                let tx = db.transaction().map_err(storage_error)?;
                tx.execute_batch("CREATE TABLE entries (key TEXT PRIMARY KEY, value BLOB NOT NULL CHECK(length(value)<=65536));").map_err(storage_error)?;
                tx.pragma_update(None, "application_id", APPLICATION_ID).map_err(storage_error)?;
                tx.pragma_update(None, "user_version", 1).map_err(storage_error)?;
                tx.commit().map_err(storage_error)?;
            } else if app != APPLICATION_ID || !(1..=2).contains(&version) { return Err(Error::Storage); }
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage_error)?;
            let current: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(storage_error)?;
            if current == 1 {
                tx.execute_batch("CREATE TABLE accessory_ids (device TEXT PRIMARY KEY, aid INTEGER NOT NULL UNIQUE CHECK(aid BETWEEN 1 AND 4294967295));
                    CREATE TABLE instance_ids (aid INTEGER NOT NULL, resource TEXT NOT NULL, iid INTEGER NOT NULL CHECK(iid BETWEEN 1 AND 4294967295), PRIMARY KEY(aid,resource), UNIQUE(aid,iid), FOREIGN KEY(aid) REFERENCES accessory_ids(aid));").map_err(storage_error)?;
                tx.pragma_update(None, "user_version", 2).map_err(storage_error)?;
            }
            tx.commit().map_err(storage_error)?;
            db.pragma_update(None, "foreign_keys", "ON").map_err(storage_error)?;
            db.pragma_update(None, "journal_mode", "WAL").map_err(storage_error)?;
            db.pragma_update(None, "synchronous", "FULL").map_err(storage_error)?;
            Ok(db)
        }).await.map_err(storage_error)??;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }
    pub(crate) async fn with<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = db.lock().map_err(storage_error)?;
            action(&mut guard)
        })
        .await
        .map_err(storage_error)?
    }
    /// Blocking authorization check for the dedicated admission thread.
    /// Database errors and malformed records must be denied by the caller.
    pub fn controller_is_paired(&self, id: Uuid) -> Result<bool> {
        Ok(self.controller_public_key(id)?.is_some())
    }
    /// Blocking public identity snapshot for revocable queued capabilities.
    /// Private accessory keys never leave this store through this method.
    pub fn controller_public_key(&self, id: Uuid) -> Result<Option<[u8; 32]>> {
        let db = self.db.lock().map_err(storage_error)?;
        let bytes: Option<Vec<u8>> = db
            .query_row(
                "SELECT value FROM entries WHERE key=?1",
                [format!("pair:{id}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let pairing: Pairing = serde_json::from_slice(&bytes)?;
        if pairing.id != id {
            return Err(Error::Storage);
        }
        Ok(Some(pairing.public_key))
    }
    pub async fn has_config(&self) -> Result<bool> {
        self.with(|db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM entries WHERE key='config')",
                [],
                |r| r.get(0),
            )
            .map_err(storage_error)
        })
        .await
    }
    async fn commit_graph_state(
        &self,
        expected_number: u64,
        config: &Config,
        aids: &[u64],
        manifest: Option<Vec<u8>>,
    ) -> Result<()> {
        let config = serde_json::to_vec(config)?;
        let aids = serde_json::to_vec(aids)?;
        if config.len() > 65536
            || aids.len() > 65536
            || manifest
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 65536)
        {
            return Err(Error::Storage);
        }
        self.with(move |db| {
            let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage_error)?;
            let old:Vec<u8>=tx.query_row("SELECT value FROM entries WHERE key='config'",[],|r|r.get(0)).map_err(storage_error)?;
            let old:Config=serde_json::from_slice(&old)?;
            let new:Config=serde_json::from_slice(&config)?;
            let ids:Vec<u64>=serde_json::from_slice(&aids)?;
            let unique:std::collections::BTreeSet<_>=ids.iter().copied().collect();
            if old.configuration_number!=expected_number || new.configuration_number!=expected_number.checked_add(1).ok_or(Error::Storage)?
                || old.device_id!=new.device_id || old.device_ed25519_keypair.to_keypair_bytes()!=new.device_ed25519_keypair.to_keypair_bytes()
                || unique.len()!=ids.len() || unique.contains(&0) { return Err(Error::Storage); }
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key='aids')",[],|r|r.get(0)).map_err(storage_error)?;
            let total:i64=tx.query_row("SELECT count(*) FROM entries",[],|r|r.get(0)).map_err(storage_error)?;
            let manifest_exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key='graph_manifest')",[],|r|r.get(0)).map_err(storage_error)?;
            let needed=i64::from(!exists)+i64::from(manifest.is_some() && !manifest_exists);
            if total+needed>4096 { return Err(Error::Storage); }
            tx.execute("UPDATE entries SET value=?1 WHERE key='config'",[config]).map_err(storage_error)?;
            tx.execute("INSERT INTO entries(key,value) VALUES ('aids',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[aids]).map_err(storage_error)?;
            if let Some(manifest)=manifest {
                tx.execute("INSERT INTO entries(key,value) VALUES ('graph_manifest',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[manifest]).map_err(storage_error)?;
            }
            tx.commit().map_err(storage_error)
        }).await
    }
    async fn read(&self, key: String) -> Result<Vec<u8>> {
        self.with(move |db| {
            db.query_row("SELECT value FROM entries WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .map_err(storage_error)
        })
        .await
    }
    async fn write(&self, key: String, value: Vec<u8>) -> Result<()> {
        if value.len() > 65536 {
            return Err(Error::Storage);
        }
        self.with(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage_error)?;
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key=?1)", [&key], |r| r.get(0)).map_err(storage_error)?;
            if !exists {
                let total: i64 = tx.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).map_err(storage_error)?;
                if total >= 4096 { return Err(Error::Storage); }
                if key.starts_with("pair:") {
                    let pairs: i64 = tx.query_row("SELECT count(*) FROM entries WHERE key LIKE 'pair:%'", [], |r| r.get(0)).map_err(storage_error)?;
                    if pairs >= 32 { return Err(Error::Storage); }
                }
            }
            if key == "config" {
                let old: Option<Vec<u8>> = tx.query_row("SELECT value FROM entries WHERE key='config'", [], |r| r.get(0)).optional().map_err(storage_error)?;
                if let Some(old) = old {
                    let old: Config = serde_json::from_slice(&old)?;
                    let new: Config = serde_json::from_slice(&value)?;
                    if old.device_id != new.device_id || old.device_ed25519_keypair.to_keypair_bytes() != new.device_ed25519_keypair.to_keypair_bytes() { return Err(Error::Storage); }
                } else {
                    let count: i64 = tx.query_row("SELECT (SELECT count(*) FROM entries) + (SELECT count(*) FROM accessory_ids)", [], |r| r.get(0)).map_err(storage_error)?;
                    if count != 0 { return Err(Error::Storage); }
                }
            }
            tx.execute("INSERT INTO entries(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        }).await
    }
    async fn delete(&self, key: String) -> Result<()> {
        self.with(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            if key == "config" {
                let count: i64 = tx
                    .query_row(
                        "SELECT (SELECT count(*) FROM entries WHERE key LIKE 'pair:%') + (SELECT count(*) FROM accessory_ids)",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?;
                if count != 0 {
                    return Err(Error::Storage);
                }
            }
            tx.execute("DELETE FROM entries WHERE key=?1", [key])
                .map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
        .await
    }
}
fn misc_key(key: &str) -> Result<String> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::Storage);
    }
    Ok(format!("misc:{key}"))
}
#[cfg(unix)]
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Storage);
    }
    Ok(())
}
#[cfg(unix)]
fn private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Storage);
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_directory(_: &Path) -> Result<()> {
    Err(Error::Storage)
}
#[cfg(not(unix))]
fn private_file(_: &Path) -> Result<()> {
    Err(Error::Storage)
}

#[async_trait]
impl Storage for IdentityStore {
    async fn load_config(&self) -> Result<Config> {
        Ok(serde_json::from_slice(&self.read("config".into()).await?)?)
    }
    async fn save_config(&mut self, config: &Config) -> Result<()> {
        self.write("config".into(), serde_json::to_vec(config)?)
            .await
    }
    async fn delete_config(&mut self) -> Result<()> {
        self.delete("config".into()).await
    }
    async fn load_aid_cache(&self) -> Result<Vec<u64>> {
        Ok(serde_json::from_slice(&self.read("aids".into()).await?)?)
    }
    async fn initialize_aid_cache(&mut self) -> Result<Vec<u64>> {
        self.with(|db| {
            let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage_error)?;
            let bytes:Option<Vec<u8>>=tx.query_row("SELECT value FROM entries WHERE key='aids'",[],|r|r.get(0)).optional().map_err(storage_error)?;
            let aids:Vec<u64>=if let Some(bytes)=bytes { serde_json::from_slice(&bytes)? } else {
                let prior:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key IN ('aids_initialized','graph_manifest') OR key LIKE 'pair:%')",[],|r|r.get(0)).map_err(storage_error)?;
                if prior { return Err(Error::Storage); }
                Vec::new()
            };
            let unique:std::collections::BTreeSet<_>=aids.iter().copied().collect();
            if unique.len()!=aids.len() || unique.contains(&0) { return Err(Error::Storage); }
            let count:i64=tx.query_row("SELECT count(*) FROM entries WHERE key NOT IN ('aids','aids_initialized')",[],|r|r.get(0)).map_err(storage_error)?;
            if count+2>4096 { return Err(Error::Storage); }
            tx.execute("INSERT OR IGNORE INTO entries(key,value) VALUES ('aids',?1)",[serde_json::to_vec(&aids)?]).map_err(storage_error)?;
            tx.execute("INSERT OR IGNORE INTO entries(key,value) VALUES ('aids_initialized',X'01')",[]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)?;
            Ok(aids)
        }).await
    }
    async fn save_aid_cache(&mut self, aids: &[u64]) -> Result<()> {
        self.write("aids".into(), serde_json::to_vec(aids)?).await
    }
    async fn save_graph_state(
        &mut self,
        expected_number: u64,
        config: &Config,
        aids: &[u64],
    ) -> Result<()> {
        self.commit_graph_state(expected_number, config, aids, None)
            .await
    }
    async fn save_graph_manifest(
        &mut self,
        expected_number: u64,
        config: &Config,
        aids: &[u64],
        manifest: &[u8],
    ) -> Result<()> {
        self.commit_graph_state(expected_number, config, aids, Some(manifest.to_vec()))
            .await
    }
    async fn load_graph_manifest(&self) -> Result<Option<Vec<u8>>> {
        self.with(|db| {
            db.query_row(
                "SELECT value FROM entries WHERE key='graph_manifest'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_error)
        })
        .await
    }
    async fn delete_aid_cache(&mut self) -> Result<()> {
        self.delete("aids".into()).await
    }
    async fn load_pairing(&self, id: &Uuid) -> Result<Pairing> {
        let pairing: Pairing = serde_json::from_slice(&self.read(format!("pair:{id}")).await?)?;
        if pairing.id != *id {
            return Err(Error::Storage);
        }
        Ok(pairing)
    }
    async fn save_pairing(&mut self, pairing: &Pairing) -> Result<()> {
        self.write(format!("pair:{}", pairing.id), serde_json::to_vec(pairing)?)
            .await
    }
    async fn delete_pairing(&mut self, id: &Uuid) -> Result<()> {
        self.delete(format!("pair:{id}")).await
    }
    async fn save_pairing_authorized(
        &mut self,
        actor: &Uuid,
        pairing: &Pairing,
        max_peers: Option<usize>,
    ) -> Result<Vec<Uuid>> {
        let actor = *actor;
        let pairing = pairing.clone();
        self.with(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage_error)?;
            let pairings = transaction_pairings(&tx)?;
            if !pairings.iter().any(|p| p.id == actor && p.permissions == hap::pairing::Permissions::Admin) { return Err(Error::Storage); }
            if let Some(old) = pairings.iter().find(|p| p.id == pairing.id) {
                if old.public_key != pairing.public_key { return Err(Error::Storage); }
            } else {
                let total: i64 = tx.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).map_err(storage_error)?;
                if total >= 4096 || pairings.len() >= max_peers.unwrap_or(32).min(32) {
                    return Err(Error::Storage);
                }
            }
            let revoked = if pairing.permissions != hap::pairing::Permissions::Admin && !pairings.iter().any(|p| p.id != pairing.id && p.permissions == hap::pairing::Permissions::Admin) {
                tx.execute("DELETE FROM entries WHERE key LIKE 'pair:%'", []).map_err(storage_error)?;
                pairings.iter().map(|p| p.id).collect()
            } else {
                tx.execute("INSERT INTO entries(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![format!("pair:{}", pairing.id), serde_json::to_vec(&pairing)?]).map_err(storage_error)?;
                Vec::new()
            };
            tx.commit().map_err(storage_error)?;
            Ok(revoked)
        }).await
    }
    async fn remove_pairing_authorized(
        &mut self,
        actor: &Uuid,
        target: &Uuid,
    ) -> Result<Vec<Uuid>> {
        let (actor, target) = (*actor, *target);
        self.with(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            let pairings = transaction_pairings(&tx)?;
            if !pairings
                .iter()
                .any(|p| p.id == actor && p.permissions == hap::pairing::Permissions::Admin)
            {
                return Err(Error::Storage);
            }
            let removed = if !pairings.iter().any(|p| p.id == target) {
                Vec::new()
            } else if pairings
                .iter()
                .any(|p| p.id != target && p.permissions == hap::pairing::Permissions::Admin)
            {
                vec![target]
            } else {
                pairings.iter().map(|p| p.id).collect()
            };
            for id in &removed {
                tx.execute("DELETE FROM entries WHERE key = ?1", [format!("pair:{id}")])
                    .map_err(storage_error)?;
            }
            tx.commit().map_err(storage_error)?;
            Ok(removed)
        })
        .await
    }
    async fn list_pairings(&self) -> Result<Vec<Pairing>> {
        self.with(|db| {
            let mut statement = db
                .prepare("SELECT value FROM entries WHERE key LIKE 'pair:%' ORDER BY key")
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .map_err(storage_error)?;
            rows.map(|r| Ok(serde_json::from_slice(&r.map_err(storage_error)?)?))
                .collect()
        })
        .await
    }
    async fn count_pairings(&self) -> Result<usize> {
        self.with(|db| {
            let count: i64 = db
                .query_row(
                    "SELECT count(*) FROM entries WHERE key LIKE 'pair:%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage_error)?;
            usize::try_from(count).map_err(storage_error)
        })
        .await
    }
    async fn load_bytes(&self, key: &str) -> Result<Vec<u8>> {
        self.read(misc_key(key)?).await
    }
    async fn save_bytes(&mut self, key: &str, value: &[u8]) -> Result<()> {
        self.write(misc_key(key)?, value.to_vec()).await
    }
    async fn delete_bytes(&mut self, key: &str) -> Result<()> {
        self.delete(misc_key(key)?).await
    }
}

fn transaction_pairings(tx: &rusqlite::Transaction<'_>) -> Result<Vec<Pairing>> {
    let mut statement = tx
        .prepare("SELECT key, value FROM entries WHERE key LIKE 'pair:%' ORDER BY key")
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(storage_error)?;
    let mut pairings = Vec::new();
    for row in rows {
        let (key, bytes) = row.map_err(storage_error)?;
        let pairing: Pairing = serde_json::from_slice(&bytes)?;
        if key != format!("pair:{}", pairing.id) {
            return Err(Error::Storage);
        }
        pairings.push(pairing);
    }
    Ok(pairings)
}
