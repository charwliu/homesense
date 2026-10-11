//! Bounded, read-only configuration history. Export never activates a revision.
use crate::{Error, Store};
use homesense_core::ExecutionContext;
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub revision: u64,
    pub device_bytes: u64,
    pub automation_bytes: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub active_revision: Option<u64>,
    pub revisions: Vec<Revision>,
    /// Exclusive upper bound for the next descending page.
    pub next_before: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub revision: u64,
    pub devices: Vec<u8>,
    pub automation: Option<Vec<u8>>,
}
impl Store {
    /// Keyset pagination avoids loading private payloads just to list versions.
    /// The active pointer and page are read from one SQLite snapshot.
    pub fn configuration_history(&self, before: Option<u64>, limit: usize) -> Result<Page, Error> {
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidPayload);
        }
        let before = before.map(crate::sql_revision).transpose()?;
        let tx = self.connection.unchecked_transaction()?;
        let active_revision = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        let active_revision = active_revision.map(|v| v as u64);
        let mut revisions = {
            let mut query=tx.prepare("SELECT c.revision,length(c.payload),length(a.payload) FROM configuration_revision c LEFT JOIN automation_revision a ON a.revision=c.revision WHERE (?1 IS NULL OR c.revision < ?1) ORDER BY c.revision DESC LIMIT ?2")?;
            query
                .query_map(params![before, (limit + 1) as i64], |r| {
                    Ok(Revision {
                        revision: r.get::<_, i64>(0)? as u64,
                        device_bytes: r.get::<_, i64>(1)? as u64,
                        automation_bytes: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        let next_before = if revisions.len() > limit {
            revisions.truncate(limit);
            revisions.last().map(|r| r.revision)
        } else {
            None
        };
        Ok(Page {
            active_revision,
            revisions,
            next_before,
        })
    }
    /// Read matching immutable documents only. Legacy device-only versions
    /// remain explicit; never substitute automation from another revision.
    pub fn export_configuration(
        &self,
        revision: u64,
        context: ExecutionContext,
    ) -> Result<Option<Export>, Error> {
        let revision_sql = crate::sql_revision(revision)?;
        let tx = self.connection.unchecked_transaction()?;
        let sizes:Option<(i64,Option<i64>)>=tx.query_row("SELECT length(c.payload),length(a.payload) FROM configuration_revision c LEFT JOIN automation_revision a ON a.revision=c.revision WHERE c.revision=?1",[revision_sql],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((devices, automation)) = sizes else {
            tx.commit()?;
            return Ok(None);
        };
        if devices <= 0
            || devices > crate::MAX_PAYLOAD as i64
            || automation.is_some_and(|n| n <= 0 || n > crate::MAX_PAYLOAD as i64)
        {
            return Err(Error::InvalidPayload);
        }
        let (devices,automation):(Vec<u8>,Option<Vec<u8>>)=tx.query_row("SELECT c.payload,a.payload FROM configuration_revision c LEFT JOIN automation_revision a ON a.revision=c.revision WHERE c.revision=?1",[revision_sql],|r|Ok((r.get(0)?,r.get(1)?)))?;
        tx.commit()?;
        let config = homesense_config::Configuration::parse(&devices, context)
            .map_err(Error::Configuration)?;
        if config.revision() != revision {
            return Err(Error::InvalidRevision);
        }
        if let Some(bytes) = &automation {
            crate::automation::validate(bytes, &config)?;
        }
        Ok(Some(Export {
            revision,
            devices,
            automation,
        }))
    }
}
