//! Persistent logical values only. Temporary and feedback-cache state belongs to
//! the boot-scoped rule snapshot and must never be restored from this table.
use crate::{Error, Store, sql_revision};
use homesense_core::rule::{Value, ValueType};
use rusqlite::{OptionalExtension, Transaction, params};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub lifetime: homesense_core::rule::Lifetime,
    pub kind: ValueType,
    pub value: Option<Value>,
    pub version: u64,
}
#[derive(Debug, Clone)]
pub struct Update {
    pub revision: u64,
    pub name: String,
    pub expected_version: u64,
    pub value: Option<Value>,
}
/// Validate a condition-only persistent read without changing its value,
/// version, or version-bound provenance. Checked in the output transaction.
#[derive(Debug, Clone)]
pub struct ReadCheck {
    pub revision: u64,
    pub name: String,
    pub expected_version: u64,
}
pub(crate) fn check_reads(
    tx: &Transaction<'_>,
    reads: &[ReadCheck],
    active: Option<i64>,
) -> Result<(), Error> {
    if reads.len() > 1024 {
        return Err(Error::InvalidAudit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for read in reads {
        let revision = sql_revision(read.revision)?;
        if active != Some(revision) || !name_valid(&read.name) || !seen.insert(&read.name) {
            return Err(Error::Conflict);
        }
        let expected = i64::try_from(read.expected_version).map_err(|_| Error::InvalidAudit)?;
        let actual: Option<i64> = tx
            .query_row(
                "SELECT version FROM logical_variable WHERE revision=?1 AND name=?2",
                params![revision, read.name],
                |r| r.get(0),
            )
            .optional()?;
        if actual != Some(expected) {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
fn name_valid(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}
fn encode(value: &Option<Value>, kind: &str) -> Result<Option<i64>, Error> {
    match (kind, value) {
        (_, None) => Ok(None),
        ("bool", Some(Value::Bool(v))) => Ok(Some(i64::from(*v))),
        ("integer", Some(Value::Integer(v))) => Ok(Some(*v)),
        _ => Err(Error::InvalidAudit),
    }
}
impl Store {
    /// Idempotent declaration batch. Existing values and versions are preserved;
    /// a conflicting type or any insertion failure rolls back the entire batch.
    pub fn ensure_logical_variables(
        &mut self,
        revision: u64,
        definitions: &[(String, ValueType)],
    ) -> Result<(), Error> {
        if definitions.len() > 1024 {
            return Err(Error::InvalidAudit);
        }
        let revision = sql_revision(revision)?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if active != Some(revision) {
            return Err(Error::Conflict);
        }
        let mut seen = std::collections::BTreeSet::new();
        for (name, kind) in definitions {
            if !name_valid(name) || !seen.insert(name) {
                return Err(Error::InvalidAudit);
            }
            let kind = match kind {
                ValueType::Bool => "bool",
                ValueType::Integer => "integer",
            };
            let existing: Option<String> = tx
                .query_row(
                    "SELECT kind FROM logical_variable WHERE revision=?1 AND name=?2",
                    params![revision, name],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(existing) if existing != kind => return Err(Error::Conflict),
                Some(_) => {}
                None => {
                    tx.execute(
                        "INSERT INTO logical_variable(revision,name,kind) VALUES(?1,?2,?3)",
                        params![revision, name, kind],
                    )?;
                }
            }
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM logical_variable WHERE revision=?1",
            [revision],
            |r| r.get(0),
        )?;
        if count > 1024 {
            return Err(Error::InvalidAudit);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn define_logical_variable(
        &mut self,
        revision: u64,
        name: &str,
        kind: ValueType,
    ) -> Result<(), Error> {
        if !name_valid(name) {
            return Err(Error::InvalidAudit);
        }
        let revision = sql_revision(revision)?;
        let tx = self.connection.transaction()?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if active != Some(revision) {
            return Err(Error::Conflict);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM logical_variable WHERE revision=?1",
            [revision],
            |r| r.get(0),
        )?;
        if count >= 1024 {
            return Err(Error::InvalidAudit);
        }
        tx.execute(
            "INSERT INTO logical_variable(revision,name,kind) VALUES(?1,?2,?3)",
            params![
                revision,
                name,
                match kind {
                    ValueType::Bool => "bool",
                    ValueType::Integer => "integer",
                }
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Ordinary rules see matching inferred selections as unknown. Use the
    /// scoped scene-selection snapshot only for observation and explicit UI actions.
    pub fn logical_variable(&self, revision: u64, name: &str) -> Result<Option<Stored>, Error> {
        let row: Option<(String, Option<i64>, i64)> = self
            .connection
            .query_row(
                "SELECT v.kind,CASE WHEN EXISTS(SELECT 1 FROM scene_observation o WHERE o.revision=v.revision AND o.name=v.name AND o.variable_version=v.version) THEN NULL ELSE v.value END,v.version FROM logical_variable v WHERE v.revision=?1 AND v.name=?2",
                params![sql_revision(revision)?, name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(kind, value, version)| {
            Ok(Stored {
                lifetime: homesense_core::rule::Lifetime::Persistent,
                kind: match kind.as_str() {
                    "bool" => ValueType::Bool,
                    "integer" => ValueType::Integer,
                    _ => return Err(Error::InvalidAudit),
                },
                value: match kind.as_str() {
                    "bool" => match value {
                        None => None,
                        Some(0) => Some(Value::Bool(false)),
                        Some(1) => Some(Value::Bool(true)),
                        _ => return Err(Error::InvalidAudit),
                    },
                    _ => value.map(Value::Integer),
                },
                version: u64::try_from(version).map_err(|_| Error::InvalidAudit)?,
            })
        })
        .transpose()
    }
}
pub(crate) fn apply(
    tx: &Transaction<'_>,
    updates: &[Update],
    active: Option<i64>,
) -> Result<(), Error> {
    let mut seen = std::collections::BTreeSet::new();
    for update in updates {
        let revision = sql_revision(update.revision)?;
        if active != Some(revision) || !name_valid(&update.name) || !seen.insert(&update.name) {
            return Err(Error::Conflict);
        }
        let expected = i64::try_from(update.expected_version).map_err(|_| Error::InvalidAudit)?;
        let next = expected.checked_add(1).ok_or(Error::InvalidAudit)?;
        let kind: Option<String> = tx
            .query_row(
                "SELECT kind FROM logical_variable WHERE revision=?1 AND name=?2",
                params![revision, update.name],
                |r| r.get(0),
            )
            .optional()?;
        let value = encode(&update.value, &kind.ok_or(Error::InvalidAudit)?)?;
        if tx.execute("UPDATE logical_variable SET value=?1,version=?2 WHERE revision=?3 AND name=?4 AND version=?5",params![value,next,revision,update.name,expected])?!=1 {return Err(Error::Conflict);}
    }
    Ok(())
}
