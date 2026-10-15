//! Bounded physical-binding command history, separate from observed feedback.
use crate::{Error, Store, sql_revision};
use homesense_core::DeviceId;
use rusqlite::params_from_iter;
const LATEST_LED_REQUEST_SQL: &str = "SELECT command_id,device_id,status,intent,transport_epoch,control_epoch,updated_ms FROM command_journal WHERE boot_id=?1 AND revision=?2 AND device_id=?3 AND transport_epoch=?4 AND control_epoch=?5 AND intent IN (x'01060000',x'01060100',x'010A0000') ORDER BY command_id DESC LIMIT 1";
#[derive(Debug)]
pub struct Record {
    pub command: u64,
    pub device: String,
    pub status: String,
    pub intent: Vec<u8>,
    pub transport_epoch: u64,
    pub control_epoch: u64,
    pub updated_ms: u64,
}
impl Store {
    /// Exact queued read-ticket receipts; the trusted adapter supplies owned IDs.
    pub fn module_query_records(
        &self,
        boot: &str,
        revision: u64,
        commands: &[u64],
    ) -> Result<Vec<Record>, Error> {
        if commands.is_empty() || commands.len() > 16 {
            return Err(Error::InvalidAudit);
        }
        let mut rows = Vec::new();
        for command in commands {
            let mut found = self.recent_device_commands(
                boot,
                revision,
                &[
                    homesense_core::DeviceId::new(format!("module-query-ticket:{command}"))
                        .map_err(|_| Error::InvalidAudit)?,
                ],
                1,
            )?;
            if let Some(row) = found.pop() {
                rows.push(row);
            }
        }
        Ok(rows)
    }
    pub fn recent_device_commands(
        &self,
        boot: &str,
        revision: u64,
        ids: &[DeviceId],
        limit: usize,
    ) -> Result<Vec<Record>, Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        if ids.len() > 128 || !(1..=64).contains(&limit) {
            return Err(Error::InvalidAudit);
        }
        let placeholders = (0..ids.len())
            .map(|i| format!("?{}", i + 4))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT j.command_id,COALESCE(e.device_id,j.device_id),j.status,j.intent,j.transport_epoch,j.control_epoch,j.updated_ms FROM command_journal j LEFT JOIN module_off_effect e ON e.boot_id=j.boot_id AND e.command_id=j.command_id WHERE j.boot_id=?1 AND j.revision=?2 AND COALESCE(e.device_id,j.device_id) IN ({placeholders}) ORDER BY j.updated_ms DESC,j.rowid DESC,e.channel ASC LIMIT ?3"
        );
        let mut values = vec![
            rusqlite::types::Value::Text(boot.into()),
            rusqlite::types::Value::Integer(sql_revision(revision)?),
            rusqlite::types::Value::Integer(limit as i64),
        ];
        values.extend(
            ids.iter()
                .map(|id| rusqlite::types::Value::Text(id.as_str().into())),
        );
        let mut query = self.connection.prepare(&sql)?;
        let rows = query.query_map(params_from_iter(values), |r| {
            Ok(Record {
                command: u64::try_from(r.get::<_, i64>(0)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                device: r.get(1)?,
                status: r.get(2)?,
                intent: r.get(3)?,
                transport_epoch: u64::try_from(r.get::<_, i64>(4)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                control_epoch: u64::try_from(r.get::<_, i64>(5)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                updated_ms: u64::try_from(r.get::<_, i64>(6)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

impl Store {
    /// Exact current-generation LED request fence, including accepted work.
    /// This is audit metadata; only the actor's complete write is Written.
    pub fn latest_led_request(
        &self,
        boot: &str,
        revision: u64,
        id: &DeviceId,
        epochs: homesense_core::queue::Epochs,
    ) -> Result<Option<Record>, Error> {
        use rusqlite::OptionalExtension;
        let mut query = self.connection.prepare(LATEST_LED_REQUEST_SQL)?;
        Ok(query
            .query_row(
                rusqlite::params![
                    boot,
                    sql_revision(revision)?,
                    id.as_str(),
                    sql_revision(epochs.transport)?,
                    sql_revision(epochs.control)?
                ],
                |r| {
                    Ok(Record {
                        command: u64::try_from(r.get::<_, i64>(0)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        device: r.get(1)?,
                        status: r.get(2)?,
                        intent: r.get(3)?,
                        transport_epoch: u64::try_from(r.get::<_, i64>(4)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        control_epoch: u64::try_from(r.get::<_, i64>(5)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        updated_ms: u64::try_from(r.get::<_, i64>(6)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
#[path = "led_request_index_tests.rs"]
mod led_request_index_tests;
