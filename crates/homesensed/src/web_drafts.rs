//! Immutable, private configuration drafts. Saving never changes the active revision.
use homesense_core::ExecutionContext;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use uuid::Uuid;

pub const MAX_BYTES: usize = 256 * 1024;
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct Draft {
    pub id: Uuid,
    pub expected_revision: u64,
    pub revision: u64,
    pub devices: serde_json::Value,
    pub automation: serde_json::Value,
}
type StoredDraft = (String, i64, i64, Vec<u8>, Vec<u8>);
pub struct Drafts(Connection);
impl Drafts {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS configuration_draft (
          id TEXT PRIMARY KEY, principal TEXT NOT NULL, expected_revision INTEGER NOT NULL,
          revision INTEGER NOT NULL, devices BLOB NOT NULL, automation BLOB NOT NULL
        );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self(db))
    }
    pub fn save(
        &mut self,
        principal: Uuid,
        draft: &Draft,
        context: ExecutionContext,
    ) -> Result<(), String> {
        let devices = serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?;
        let automation = serde_json::to_vec(&draft.automation).map_err(|e| e.to_string())?;
        if devices.len() + automation.len() > MAX_BYTES
            || draft.expected_revision == 0
            || draft.revision <= draft.expected_revision
            || draft.revision > i64::MAX as u64
        {
            return Err("invalid draft size or revision".into());
        }
        let config =
            homesense_config::Configuration::parse(&devices, context).map_err(|e| e.to_string())?;
        if config.revision() != draft.revision {
            return Err("draft revision mismatch".into());
        }
        crate::configuration_publish::validate_automation(&config, &automation, context)
            .map_err(|e| e.to_string())?;
        let tx = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let existing:Option<StoredDraft>=tx.query_row("SELECT principal,expected_revision,revision,devices,automation FROM configuration_draft WHERE id=?1",[draft.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|e|e.to_string())?;
        if let Some(old) = existing {
            if old
                != (
                    principal.to_string(),
                    draft.expected_revision as i64,
                    draft.revision as i64,
                    devices,
                    automation,
                )
            {
                return Err("draft id conflict".into());
            }
            return Ok(());
        }
        let count: i64 = tx
            .query_row("SELECT count(*) FROM configuration_draft", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count >= 128 {
            return Err("draft capacity exceeded".into());
        }
        tx.execute(
            "INSERT INTO configuration_draft VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                draft.id.to_string(),
                principal.to_string(),
                draft.expected_revision as i64,
                draft.revision as i64,
                devices,
                automation
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn get(&self, principal: Uuid, id: Uuid) -> Result<Option<Draft>, String> {
        let row:Option<(i64,i64,Vec<u8>,Vec<u8>)>=self.0.query_row("SELECT expected_revision,revision,devices,automation FROM configuration_draft WHERE principal=?1 AND id=?2",params![principal.to_string(),id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
        row.map(|(expected_revision, revision, devices, automation)| {
            Ok(Draft {
                id,
                expected_revision: u64::try_from(expected_revision)
                    .map_err(|_| "invalid stored revision")?,
                revision: u64::try_from(revision).map_err(|_| "invalid stored revision")?,
                devices: serde_json::from_slice(&devices).map_err(|e| e.to_string())?,
                automation: serde_json::from_slice(&automation).map_err(|e| e.to_string())?,
            })
        })
        .transpose()
    }
}
