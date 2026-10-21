//! Local mode inference provenance, separate from EA-5 or hardware observations.
use crate::{
    Error, Store, sql_revision,
    variables::{ReadCheck, Update},
};
use rusqlite::{OptionalExtension, params};
pub struct Audit {
    pub provenance: String,
    pub policy: String,
    pub controller: String,
}
impl Store {
    pub fn ensure_component_mode_audit(&self) -> Result<(), Error> {
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS component_mode_audit(revision INTEGER NOT NULL,name TEXT NOT NULL,version INTEGER NOT NULL,policy TEXT NOT NULL,boot TEXT NOT NULL,controller TEXT NOT NULL,at_ms INTEGER NOT NULL,provenance TEXT NOT NULL,tickets TEXT NOT NULL,PRIMARY KEY(revision,name,version));CREATE TABLE IF NOT EXISTS component_mode_source(revision INTEGER NOT NULL,name TEXT NOT NULL,version INTEGER NOT NULL,source_name TEXT NOT NULL,source_version INTEGER NOT NULL,source_value INTEGER NOT NULL,PRIMARY KEY(revision,name,version,source_name))")?;
        Ok(())
    }
    pub fn component_mode_audit(
        &self,
        revision: u64,
        name: &str,
        version: u64,
    ) -> Result<Option<Audit>, Error> {
        let row:Option<(String,String,String)>=self.connection.query_row("SELECT policy,controller,provenance FROM component_mode_audit WHERE revision=?1 AND name=?2 AND version=?3 AND provenance IN ('local_written_with_feedback_inference','local_source_canonicalization')",params![sql_revision(revision)?,name,i64::try_from(version).map_err(|_|Error::InvalidAudit)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        row.map(|(policy, controller, provenance)| {
            Ok(Audit {
                policy,
                controller,
                provenance,
            })
        })
        .transpose()
    }
    /// CAS the value and provenance in one FULL transaction. Input guards are
    /// the acceptance snapshot's persistent reads, never assumed zero defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_component_mode_inference(
        &mut self,
        updates: &[(Update, String)],
        reads: &[ReadCheck],
        boot: &str,
        controller: &str,
        now: u64,
        tickets: &[u64],
        confirmed: bool,
    ) -> Result<(), Error> {
        if updates.is_empty()
            || updates.len() > 64
            || tickets.len() > 512
            || boot.is_empty()
            || boot.len() > 128
            || controller.is_empty()
            || controller.len() > 128
        {
            return Err(Error::InvalidAudit);
        }
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
        crate::variables::check_reads(&tx, reads, active)?;
        crate::variables::apply(
            &tx,
            &updates.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
            active,
        )?;
        let provenance = if confirmed {
            "local_written_with_feedback_inference"
        } else {
            "inference_invalidated"
        };
        let encoded = tickets
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        for (u, policy) in updates {
            if policy.is_empty() || policy.len() > 256 {
                return Err(Error::InvalidAudit);
            }
            tx.execute(
                "INSERT INTO component_mode_audit VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    sql_revision(u.revision)?,
                    u.name,
                    i64::try_from(
                        u.expected_version
                            .checked_add(1)
                            .ok_or(Error::InvalidAudit)?
                    )
                    .map_err(|_| Error::InvalidAudit)?,
                    policy,
                    boot,
                    controller,
                    i64::try_from(now).map_err(|_| Error::InvalidAudit)?,
                    provenance,
                    encoded
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
impl Store {
    pub fn component_mode_ticket(
        &self,
        boot: &str,
        id: u64,
    ) -> Result<Option<(String, u64)>, Error> {
        let row: Option<(String, i64)> = self
            .connection
            .prepare_cached("SELECT status,updated_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2")?
            .query_row(
                params![boot, i64::try_from(id).map_err(|_| Error::InvalidAudit)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(status, at)| Ok((status, u64::try_from(at).map_err(|_| Error::InvalidAudit)?)))
            .transpose()
    }
}
impl Store {
    /// A known local write followed by Observed retains its original write
    /// fence. A read-only Query receipt is never actuator ownership evidence.
    pub fn component_mode_feedback_settled(
        &self,
        boot: &str,
        device: &homesense_core::DeviceId,
        at: Option<u64>,
        epoch: u64,
        owned_written: &std::collections::BTreeMap<u64, u64>,
    ) -> Result<bool, Error> {
        let Some(at) = at else { return Ok(false) };
        let mut query=self.connection.prepare("SELECT command_id,status,updated_ms,intent,transport_epoch FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND (status IN ('accepted','writing') OR (status IN ('written','observed','failed','uncertain') AND transport_epoch>=?3 AND updated_ms>=?4))")?;
        let rows = query.query_map(
            params![
                boot,
                device.as_str(),
                i64::try_from(epoch).map_err(|_| Error::InvalidAudit)?,
                i64::try_from(at).map_err(|_| Error::InvalidAudit)?
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;
        for row in rows {
            let (id, status, _, intent, _) = row?;
            if intent.as_slice() == [1, 0, 0, 0] {
                continue;
            }
            let id = u64::try_from(id).map_err(|_| Error::InvalidAudit)?;
            if status == "observed" && owned_written.get(&id).is_some_and(|written| *written < at) {
                continue;
            }
            return Ok(false);
        }
        Ok(true)
    }
}

/// Pure local source normalization, never a hardware receipt.
#[derive(Clone)]
pub struct Canonical {
    pub update: Update,
    pub policy: String,
    pub sources: Vec<SourceValue>,
}
#[derive(Clone)]
pub struct SourceValue {
    pub check: ReadCheck,
    pub value: homesense_core::rule::Value,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_canonical(
    tx: &rusqlite::Transaction<'_>,
    boot: &str,
    controller: &str,
    updates: &[Update],
    entries: &[Canonical],
    now: u64,
    active: Option<i64>,
) -> Result<(), Error> {
    if entries.len() > 16 || controller.is_empty() || controller.len() > 128 {
        return Err(Error::InvalidAudit);
    }
    let mut names = std::collections::BTreeSet::new();
    for entry in entries {
        let u = &entry.update;
        if !names.insert(&u.name)
            || entry.policy.is_empty()
            || entry.policy.len() > 256
            || entry.sources.is_empty()
            || entry.sources.len() > 16
            || !updates.iter().any(|v| {
                v.name == u.name
                    && v.revision == u.revision
                    && v.expected_version == u.expected_version
                    && v.value == u.value
            })
        {
            return Err(Error::InvalidAudit);
        }
        crate::variables::check_reads(
            tx,
            &entry
                .sources
                .iter()
                .map(|s| s.check.clone())
                .collect::<Vec<_>>(),
            active,
        )?;
        let version = u
            .expected_version
            .checked_add(1)
            .ok_or(Error::InvalidAudit)?;
        tx.execute("INSERT INTO component_mode_audit VALUES(?1,?2,?3,?4,?5,?6,?7,'local_source_canonicalization','')",params![sql_revision(u.revision)?,u.name,i64::try_from(version).map_err(|_|Error::InvalidAudit)?,entry.policy,boot,controller,i64::try_from(now).map_err(|_|Error::InvalidAudit)?])?;
        for source in &entry.sources {
            if source.check.revision != u.revision || source.check.name == u.name {
                return Err(Error::InvalidAudit);
            }
            let (kind, value) = match source.value {
                homesense_core::rule::Value::Bool(v) => ("bool", i64::from(v)),
                homesense_core::rule::Value::Integer(v) => ("integer", v),
            };
            let actual: Option<(String, Option<i64>)> = tx
                .query_row(
                    "SELECT kind,value FROM logical_variable WHERE revision=?1 AND name=?2",
                    params![sql_revision(source.check.revision)?, source.check.name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if actual != Some((kind.into(), Some(value))) {
                return Err(Error::Conflict);
            }
            tx.execute(
                "INSERT INTO component_mode_source VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    sql_revision(u.revision)?,
                    u.name,
                    i64::try_from(version).map_err(|_| Error::InvalidAudit)?,
                    source.check.name,
                    i64::try_from(source.check.expected_version)
                        .map_err(|_| Error::InvalidAudit)?,
                    value
                ],
            )?;
        }
    }
    Ok(())
}
