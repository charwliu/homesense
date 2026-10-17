//! Bounded per-endpoint software state, never physical LED/settings feedback.
use crate::{Error, Store, module_history::Record, sql_revision};
use homesense_core::DeviceId;
use rusqlite::{OptionalExtension, Row, params};
use std::collections::BTreeSet;

// Exact supported intents, grouped into LED/backlight/sound/work-mode state.
// Reads belong to last_request only; a sent read cannot establish a setting.
const FAMILY: &str = "CASE WHEN intent IN (x'01060000',x'01060100',x'010A0000') THEN 0 WHEN intent IN (x'0109C500',x'0109C601',x'0109C602',x'0109C603') THEN 1 WHEN intent IN (x'0109C100',x'0109C201',x'0109C202') THEN 2 WHEN intent IN (x'0109C700',x'0109C801',x'0109C802') THEN 3 END";
const WRITTEN: &str = "status='written' AND intent NOT IN (x'0109C500',x'0109C100',x'0109C700')";

pub(crate) fn install_indexes(tx: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    // Index-only maintenance is compatible with the existing row format. Like
    // the LED fence index, install transactionally without a schema bump.
    tx.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS command_keypad_request_state ON command_journal(boot_id,revision,device_id,({FAMILY}),updated_ms DESC,command_id DESC) WHERE ({FAMILY}) IS NOT NULL;
         CREATE INDEX IF NOT EXISTS command_keypad_written_state ON command_journal(boot_id,revision,device_id,({FAMILY}),updated_ms DESC,command_id DESC) WHERE ({FAMILY}) IS NOT NULL AND {WRITTEN};"
    ))?;
    Ok(())
}
fn query_sql(written: bool) -> String {
    format!(
        "SELECT command_id,device_id,status,intent,transport_epoch,control_epoch,updated_ms FROM command_journal WHERE boot_id=?1 AND revision=?2 AND device_id=?3 AND ({FAMILY}) IS NOT NULL AND ({FAMILY})=?4 {} ORDER BY updated_ms DESC,command_id DESC LIMIT 1",
        if written {
            format!("AND {WRITTEN}")
        } else {
            String::new()
        }
    )
}
fn record(row: &Row<'_>) -> rusqlite::Result<Record> {
    let integer = |column| {
        u64::try_from(row.get::<_, i64>(column)?).map_err(|_| rusqlite::Error::InvalidQuery)
    };
    Ok(Record {
        command: integer(0)?,
        device: row.get(1)?,
        status: row.get(2)?,
        intent: row.get(3)?,
        transport_epoch: integer(4)?,
        control_epoch: integer(5)?,
        updated_ms: integer(6)?,
    })
}
impl Store {
    /// At most eight records per requested endpoint, independent of unrelated
    /// traffic and failed/request-only history. Scope never expands to siblings.
    pub fn keypad_state_records(
        &self,
        boot: &str,
        revision: u64,
        ids: &[DeviceId],
    ) -> Result<Vec<Record>, Error> {
        if ids.len() > 128 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err(Error::InvalidAudit);
        }
        let revision = sql_revision(revision)?;
        let request_sql = query_sql(false);
        let written_sql = query_sql(true);
        let mut request = self.connection.prepare_cached(&request_sql)?;
        let mut written = self.connection.prepare_cached(&written_sql)?;
        let mut records = vec![];
        let mut seen = BTreeSet::new();
        for id in ids {
            for family in 0..4 {
                for query in [&mut request, &mut written] {
                    if let Some(value) = query
                        .query_row(params![boot, revision, id.as_str(), family], record)
                        .optional()?
                        && seen.insert(value.command)
                    {
                        records.push(value);
                    }
                }
            }
        }
        Ok(records)
    }
}

#[cfg(test)]
#[path = "keypad_history_tests.rs"]
mod tests;
