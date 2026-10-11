//! One immutable device/automation revision and its persistent declarations.
//! Publication is storage-only; callers still own the stop/generation barrier.
use crate::{CancellationAudit, Error, Store};
use homesense_config::rule_document::Document;
use homesense_core::{
    ExecutionContext,
    rule::{Lifetime, ValueType},
};
use rusqlite::{OptionalExtension, Transaction, params};

pub struct Bundle {
    pub configuration: homesense_config::Configuration,
    pub automation: Option<Document>,
}
impl Store {
    /// Validate both documents before a single CAS transaction. Same-name,
    /// same-type persistent values survive publication; new values are unknown.
    /// Type changes require an explicit migration and are rejected here.
    pub fn publish_automation(
        &mut self,
        expected: Option<u64>,
        devices: &[u8],
        rules: &[u8],
        context: ExecutionContext,
        cancellations: &[CancellationAudit<'_>],
    ) -> Result<Bundle, Error> {
        let configuration = homesense_config::Configuration::parse(devices, context)
            .map_err(Error::Configuration)?;
        let automation = validate(rules, &configuration)?;
        self.publish_inner(
            expected,
            configuration.revision(),
            devices,
            cancellations,
            Some((rules, &automation)),
        )?;
        Ok(Bundle {
            configuration,
            automation: Some(automation),
        })
    }
    /// Read both halves from one SQLite snapshot, then revalidate stored data.
    /// Legacy revisions explicitly return None, never another revision's rules.
    pub fn active_bundle(&self, context: ExecutionContext) -> Result<Option<Bundle>, Error> {
        let tx = self.connection.unchecked_transaction()?;
        let stored: Option<(i64, Vec<u8>, Option<Vec<u8>>)> = tx.query_row(
            "SELECT c.revision,c.payload,r.payload FROM active_configuration a JOIN configuration_revision c ON c.revision=a.revision LEFT JOIN automation_revision r ON r.revision=c.revision WHERE a.singleton=1",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        tx.commit()?;
        let Some((revision, devices, rules)) = stored else {
            return Ok(None);
        };
        let configuration = homesense_config::Configuration::parse(&devices, context)
            .map_err(Error::Configuration)?;
        if crate::sql_revision(configuration.revision())? != revision {
            return Err(Error::InvalidRevision);
        }
        let automation = rules
            .map(|rules| validate(&rules, &configuration))
            .transpose()?;
        Ok(Some(Bundle {
            configuration,
            automation,
        }))
    }
}
fn validate(bytes: &[u8], config: &homesense_config::Configuration) -> Result<Document, Error> {
    let document = Document::parse(bytes, config).map_err(|_| Error::InvalidPayload)?;
    document
        .linked_rules(
            &document.immediate_scenes,
            &document.schedules.iter().map(|s| s.id.clone()).collect(),
        )
        .map_err(|_| Error::InvalidPayload)?;
    Ok(document)
}
pub(crate) fn insert(
    tx: &Transaction<'_>,
    previous: Option<i64>,
    revision: i64,
    payload: &[u8],
    document: &Document,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO automation_revision(revision,payload) VALUES(?1,?2)",
        params![revision, payload],
    )?;
    for (name, variable) in &document.variables {
        if variable.lifetime != Lifetime::Persistent {
            continue;
        }
        let kind = match variable.value_type {
            ValueType::Bool => "bool",
            ValueType::Integer => "integer",
        };
        let old: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT kind,value FROM logical_variable WHERE revision=?1 AND name=?2",
                params![previous, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let value = match old {
            Some((old_kind, value)) if old_kind == kind => value,
            Some(_) => return Err(Error::Conflict),
            None => None,
        };
        tx.execute(
            "INSERT INTO logical_variable(revision,name,kind,value) VALUES(?1,?2,?3,?4)",
            params![revision, name, kind, value],
        )?;
    }
    Ok(())
}
