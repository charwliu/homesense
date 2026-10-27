//! One immutable device/automation revision and its persistent declarations.
//! Publication is storage-only; callers still own the stop/generation barrier.
use crate::{CancellationAudit, Error, Store};
use homesense_config::rule_document::Document;
use homesense_core::{
    ExecutionContext,
    rule::{Lifetime, Value, ValueType},
};
use rusqlite::{OptionalExtension, Transaction, params};
use std::sync::Arc;

#[derive(Clone)]
pub struct Bundle {
    pub configuration: homesense_config::Configuration,
    pub automation: Option<Document>,
}
/// Only validated decoded data is cached. A SQLite snapshot/version fence
/// detects both local writes and commits by other connections, including
/// edits at the same revision. Runtime values and permissions are never cached.
pub(crate) struct CachedBundle {
    revision: i64,
    devices: Vec<u8>,
    rules: Option<Vec<u8>>,
    context: ExecutionContext,
    bundle: Arc<Bundle>,
    data_version: i64,
    schema_version: i64,
    local_changes: u64,
}
impl Store {
    pub fn automation_payload(&self, revision: u64) -> Result<Option<Vec<u8>>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT payload FROM automation_revision WHERE revision=?1",
                [crate::sql_revision(revision)?],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Validate both documents before a single CAS transaction. Same-name,
    /// same-type persistent values survive publication; new values are unknown.
    /// Type changes require an explicit reviewed migration in the document.
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
        Ok(self.active_bundle_shared(context)?.map(|bundle| (*bundle).clone()))
    }

    /// Share immutable definitions, with the read snapshot established before
    /// checking SQLite's change counters. Changed counters force both payloads
    /// to be rechecked; unchanged counters avoid rereading multi-MB blobs.
    pub fn active_bundle_shared(&self, context: ExecutionContext) -> Result<Option<Arc<Bundle>>, Error> {
        let tx = self.connection.unchecked_transaction()?;
        // Establish the read snapshot before asking for data_version: a commit
        // between a version read and the first SELECT must not lend an old cache
        // entry to a new snapshot at the same revision.
        let revision: Option<i64> = tx.query_row(
            "SELECT revision FROM active_configuration WHERE singleton=1", [], |r| r.get(0),
        ).optional()?;
        let data_version = tx.pragma_query_value(None, "data_version", |r| r.get(0))?;
        let schema_version = tx.pragma_query_value(None, "schema_version", |r| r.get(0))?;
        let local_changes = self.connection.total_changes();
        if let Some(cached) = self.active_bundle_cache.borrow().as_ref()
            && Some(cached.revision) == revision && cached.context == context
            && cached.data_version == data_version && cached.schema_version == schema_version
            && cached.local_changes == local_changes {
            let bundle = cached.bundle.clone();
            tx.commit()?;
            return Ok(Some(bundle));
        }
        let stored: Option<(i64, Vec<u8>, Option<Vec<u8>>)> = tx.query_row(
            "SELECT c.revision,c.payload,r.payload FROM active_configuration a JOIN configuration_revision c ON c.revision=a.revision LEFT JOIN automation_revision r ON r.revision=c.revision WHERE a.singleton=1",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        tx.commit()?;
        let Some((revision, devices, rules)) = stored else {
            self.active_bundle_cache.replace(None);
            return Ok(None);
        };
        if let Some(cached)=self.active_bundle_cache.borrow_mut().as_mut()
            && cached.revision==revision && cached.context==context
            && cached.devices==devices && cached.rules==rules {
            cached.data_version = data_version;
            cached.schema_version = schema_version;
            cached.local_changes = local_changes;
            return Ok(Some(cached.bundle.clone()));
        }
        // Clear before validating a changed payload or execution context so an
        // invalid current snapshot cannot preserve or expose a previous result.
        self.active_bundle_cache.replace(None);
        let configuration = homesense_config::Configuration::parse(&devices, context)
            .map_err(Error::Configuration)?;
        if crate::sql_revision(configuration.revision())? != revision {
            return Err(Error::InvalidRevision);
        }
        let automation = rules.as_ref()
            .map(|rules| validate(rules, &configuration))
            .transpose()?;
        let bundle=Arc::new(Bundle{configuration,automation});
        self.active_bundle_cache.replace(Some(CachedBundle{revision,devices,rules,context,bundle:bundle.clone(),data_version,schema_version,local_changes}));
        Ok(Some(bundle))
    }
}
pub(crate) fn validate(bytes: &[u8], config: &homesense_config::Configuration) -> Result<Document, Error> {
    let document = Document::parse(bytes, config).map_err(|_| Error::InvalidPayload)?;
    document
.linked_rules_with_deferred()
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
                "SELECT v.kind,CASE WHEN EXISTS(SELECT 1 FROM scene_observation o WHERE o.revision=v.revision AND o.name=v.name AND o.variable_version=v.version) THEN NULL ELSE v.value END FROM logical_variable v WHERE v.revision=?1 AND v.name=?2",
                params![previous, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let old=old.map(|(kind,value)| -> Result<_,Error> {
            match kind.as_str() {
                "bool"=>Ok((ValueType::Bool,match value {None=>None,Some(0)=>Some(Value::Bool(false)),Some(1)=>Some(Value::Bool(true)),_=>return Err(Error::InvalidPayload)})),
                "integer"=>Ok((ValueType::Integer,value.map(Value::Integer))),
                _=>Err(Error::InvalidPayload),
            }
        }).transpose()?;
        let converted=document.published_variable_value(previous.map(|v|v as u64),name,old.as_ref().map(|(kind,value)|(*kind,value.as_ref()))).map_err(|_|Error::Conflict)?;
        let value=converted.map(|value|match value {Value::Bool(v)=>i64::from(v),Value::Integer(v)=>v});
        tx.execute(
            "INSERT INTO logical_variable(revision,name,kind,value) VALUES(?1,?2,?3,?4)",
            params![revision, name, kind, value],
        )?;
    }
    Ok(())
}
