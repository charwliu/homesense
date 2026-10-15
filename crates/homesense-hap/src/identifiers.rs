//! Persistent semantic identifiers. Removed resources are retained permanently;
//! no allocation method deletes/recycles IDs or changes pairing identity.
use crate::storage::{IdentityStore, storage_error};
use hap::{Config, Error, Result};
use homesense_core::DeviceId;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MotorOperation { Open, Close, Stop }
impl MotorOperation {
    pub fn action(self)->homesense_core::MotorAction {match self {Self::Open=>homesense_core::MotorAction::Open,Self::Close=>homesense_core::MotorAction::Close,Self::Stop=>homesense_core::MotorAction::Stop}}
    pub fn label(self)->&'static str {match self {Self::Open=>"Open",Self::Close=>"Close",Self::Stop=>"Stop"}}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessoryKey {
    AwayOff,
    Bridge,
    Device(DeviceId),
    /// Explicit scene action; its identity is independent of display names.
    Scene(String),
    /// Read-only activity has its own namespace; never encoded as a scene ID suffix.
    SceneActivity(String),
    Motor(DeviceId, MotorOperation),
    /// An explicitly selected physical panel key; independent of its label.
    PanelKey(DeviceId, u8),
}
impl AccessoryKey {
    fn storage_key(&self) -> String {
        match self {
            Self::AwayOff => "operation:away-off".into(),
            Self::PanelKey(device, key) => format!("panel:{}:{}:{key}", device.as_str().len(), device.as_str()),
            Self::Bridge => "bridge".into(),
            Self::Device(device) => format!("device:{}", device.as_str()),
            Self::Scene(id) => format!("scene:{id}"),
            Self::SceneActivity(id) => format!("sact:{id}"),
            Self::Motor(device,action)=>format!("motor:{}:{}:{}",device.as_str().len(),device.as_str(),action.label().to_ascii_lowercase()),
        }
    }
}
#[derive(Debug, Clone)]
pub struct AccessoryDefinition {
    pub key: AccessoryKey,
    /// Stable semantic names for BOTH services and characteristics. Never use
    /// display names, list indexes, or mutable addresses as these keys.
    pub resources: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryIds {
    pub aid: u64,
    pub instances: BTreeMap<String, u64>,
}
impl IdentityStore {
    /// Allocates in one SQLite transaction, serialized across store connections.
    /// Call before exposing a graph. A result is not permission to publish it:
    /// graph/configuration-number publication is coordinated by the service owner.
    /// Requires an existing valid identity; missing mappings with existing paired
    /// controllers or a populated upstream AID cache fail closed.
    pub async fn allocate_identifiers(
        &self,
        definitions: Vec<AccessoryDefinition>,
    ) -> Result<BTreeMap<AccessoryKey, AccessoryIds>> {
        if definitions.is_empty() || definitions.len() > 4096 {
            return Err(Error::Storage);
        }
        let mut sorted = BTreeMap::new();
        let mut total = 0usize;
        for definition in definitions {
            if matches!(&definition.key, AccessoryKey::Scene(id) | AccessoryKey::SceneActivity(id) if id.trim().is_empty() || id.chars().any(char::is_control))
                || matches!(&definition.key, AccessoryKey::PanelKey(device, key) if *key == 0 || *key > 6 || device.as_str().chars().any(char::is_control))
                || definition.key.storage_key().len() > 256
                || definition.resources.is_empty()
                || definition.resources.len() > 128
            {
                return Err(Error::Storage);
            }
            let mut resources = BTreeSet::new();
            for resource in definition.resources {
                total += resource.len();
                if resource.is_empty()
                    || resource.len() > 128
                    || total > 1024 * 1024
                    || resource.chars().any(char::is_control)
                    || !resources.insert(resource)
                {
                    return Err(Error::Storage);
                }
            }
            if sorted.insert(definition.key, resources).is_some() {
                return Err(Error::Storage);
            }
        }
        self.with(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(storage_error)?;
            let identity: Vec<u8> = tx.query_row("SELECT value FROM entries WHERE key='config'", [], |r| r.get(0)).map_err(storage_error)?;
            let _: Config = serde_json::from_slice(&identity)?;
            let count: i64 = tx.query_row("SELECT count(*) FROM accessory_ids", [], |r| r.get(0)).map_err(storage_error)?;
            let initialized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key='identifiers_initialized')", [], |r| r.get(0)).map_err(storage_error)?;
            if (count == 0) == initialized { return Err(Error::Storage); }
            if count == 0 {
                let paired: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key LIKE 'pair:%')", [], |r| r.get(0)).map_err(storage_error)?;
                let cache: Option<Vec<u8>> = tx.query_row("SELECT value FROM entries WHERE key='aids'", [], |r| r.get(0)).optional().map_err(storage_error)?;
                if paired || cache.map(|v| serde_json::from_slice::<Vec<u64>>(&v)).transpose()?.is_some_and(|v| !v.is_empty()) {
                    return Err(Error::Storage);
                }
                tx.execute("INSERT INTO entries(key,value) VALUES ('identifiers_initialized',X'01')", []).map_err(storage_error)?;
                tx.execute("INSERT INTO accessory_ids(device,aid) VALUES ('bridge',1)", []).map_err(storage_error)?;
            }
            let bridge: Option<u32> = tx.query_row("SELECT aid FROM accessory_ids WHERE device='bridge'", [], |r| r.get(0)).optional().map_err(storage_error)?;
            if bridge != Some(1) { return Err(Error::Storage); }
            let mut result = BTreeMap::new();
            for (key, resources) in sorted {
                let storage_key = key.storage_key();
                let existing: Option<u32> = tx.query_row("SELECT aid FROM accessory_ids WHERE device=?1", [&storage_key], |r| r.get(0)).optional().map_err(storage_error)?;
                let aid = if let Some(aid) = existing { aid } else {
                    let (count, max): (u32,u32) = tx.query_row("SELECT count(*), max(aid) FROM accessory_ids", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(storage_error)?;
                    if count >= 4097 || max == u32::MAX { return Err(Error::Storage); }
                    tx.execute("INSERT INTO accessory_ids(device,aid) VALUES (?1,?2)", params![storage_key,max+1]).map_err(storage_error)?;
                    max+1
                };
                let mut instances = BTreeMap::new();
                for resource in resources {
                    let existing: Option<u32> = tx.query_row("SELECT iid FROM instance_ids WHERE aid=?1 AND resource=?2", params![aid,resource], |r| r.get(0)).optional().map_err(storage_error)?;
                    let iid = if let Some(iid) = existing { iid } else {
                        let (count,max): (u32,u32) = tx.query_row("SELECT count(*), coalesce(max(iid),0) FROM instance_ids WHERE aid=?1", [aid], |r| Ok((r.get(0)?,r.get(1)?))).map_err(storage_error)?;
                        if count >= 128 || max == u32::MAX { return Err(Error::Storage); }
                        tx.execute("INSERT INTO instance_ids(aid,resource,iid) VALUES (?1,?2,?3)", params![aid,resource,max+1]).map_err(storage_error)?;
                        max+1
                    };
                    instances.insert(resource,u64::from(iid));
                }
                result.insert(key,AccessoryIds { aid: u64::from(aid), instances });
            }
            tx.commit().map_err(storage_error)?;
            Ok(result)
        }).await
    }
}
