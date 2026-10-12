//! Last confirmed nonzero brightness, scoped to immutable configuration revision.
//! This is a restore preference, never a current-state observation.
use crate::{Error, Store, sql_revision};
use homesense_core::{DeviceId, Percent};
use rusqlite::params;
use std::collections::BTreeMap;
impl Store {
    /// Durable HomeSense settings, distinct from measured output and restore
    /// preferences. Only completed writes count; cancelled/queued/uncertain
    /// targets cannot become settings. Row order spans process boots.
    pub fn recorded_dimmer_levels(
        &self,
        revision: u64,
        devices: &[DeviceId],
    ) -> Result<BTreeMap<DeviceId, Percent>, Error> {
        if devices.len() > 4096 { return Err(Error::InvalidPayload); }
        use rusqlite::OptionalExtension;
        let mut query = self.connection.prepare("SELECT level FROM light_setting WHERE revision=?1 AND device_id=?2")?;
        let mut result = BTreeMap::new();
        for id in devices {
            let level: Option<u8> = query.query_row(params![sql_revision(revision)?,id.as_str()], |row| row.get(0)).optional()?;
            if let Some(level) = level { result.insert(id.clone(), Percent::new(level).map_err(|_|Error::InvalidAudit)?); }
        }
        Ok(result)
    }
    /// The runtime must supply only fresh, confirmed physical feedback.
    /// A zero observation intentionally preserves the previous nonzero value.
    pub fn remember_light_levels(
        &mut self,
        revision: u64,
        levels: &[(DeviceId, Percent)],
    ) -> Result<(), Error> {
        let revision = sql_revision(revision)?;
        let tx = self.connection.transaction()?;
        let active: i64 = tx.query_row(
            "SELECT revision FROM active_configuration WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if active != revision {
            return Err(Error::Conflict);
        }
        for (id, level) in levels {
            if level.get() == 0 {
                continue;
            }
            tx.execute("INSERT INTO light_restore(revision,device_id,level) VALUES(?1,?2,?3) ON CONFLICT(revision,device_id) DO UPDATE SET level=excluded.level WHERE light_restore.level!=excluded.level", params![revision,id.as_str(),level.get()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn light_restore_levels(
        &self,
        revision: u64,
    ) -> Result<BTreeMap<DeviceId, Percent>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT device_id,level FROM light_restore WHERE revision=?1")?;
        let records = statement.query_map([sql_revision(revision)?], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u8>(1)?))
        })?;
        let mut levels = BTreeMap::new();
        for record in records {
            let (id, level) = record?;
            if level == 0 {
                return Err(Error::InvalidPayload);
            }
            levels.insert(
                DeviceId::new(id).map_err(|_| Error::InvalidPayload)?,
                Percent::new(level).map_err(|_| Error::InvalidPayload)?,
            );
        }
        Ok(levels)
    }
}
