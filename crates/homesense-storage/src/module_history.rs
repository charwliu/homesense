//! Bounded physical-binding command history, separate from observed feedback.
use crate::{Error, Store, sql_revision};
use homesense_core::DeviceId;
use rusqlite::params_from_iter;
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
            "SELECT command_id,device_id,status,intent,transport_epoch,control_epoch,updated_ms FROM command_journal WHERE boot_id=?1 AND revision=?2 AND device_id IN ({placeholders}) ORDER BY updated_ms DESC,rowid DESC LIMIT ?3"
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
