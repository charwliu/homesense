//! Durable at-most-once relative scene requests. Reserved phases are never
//! replay instructions. The runtime validates capabilities and fresh feedback.
use crate::{Error, Store, sql_revision};
use homesense_core::{DeviceId, Percent, queue::Epochs};
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub controller: String,
    pub key: String,
    pub scene: String,
    pub group: String,
    pub variable: String,
    pub variable_version: u64,
    pub boot: String,
    pub epochs: Epochs,
    pub deadline_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    Observed,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub request: Request,
    pub phase: u64,
    pub fence: u64,
    pub status: Status,
    pub commands: Vec<u64>,
    pub initial_commands: Vec<u64>,
    pub selected: Option<i64>,
    pub updated_ms: u64,
}
fn number(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
fn valid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 250 && !s.chars().any(char::is_control)
}
impl Store {
    pub fn native_written_query_devices(
        &self,
        request: &Request,
        ids: &[u64],
    ) -> Result<Option<std::collections::BTreeSet<DeviceId>>, Error> {
        if ids.is_empty() || ids.len() > 128 {
            return Ok(None);
        }
        let mut devices = std::collections::BTreeSet::new();
        for id in ids {
            let device:Option<String>=self.connection.query_row("SELECT device_id FROM command_journal WHERE boot_id=?1 AND command_id=?2 AND revision=?3 AND transport_epoch=?4 AND control_epoch=?5 AND status='written' AND intent=x'01000000'",params![request.boot,number(*id)?,sql_revision(request.epochs.configuration)?,number(request.epochs.transport)?,number(request.epochs.control)?],|r|r.get(0)).optional()?;
            let Some(device) = device else {
                return Ok(None);
            };
            if !devices.insert(DeviceId::new(device).map_err(|_| Error::InvalidAudit)?) {
                return Ok(None);
            }
        }
        Ok(Some(devices))
    }
    pub fn native_scene_request(
        &self,
        controller: &str,
        key: &str,
    ) -> Result<Option<Record>, Error> {
        Ok(self.connection.query_row("SELECT scene_id,group_id,variable_name,variable_version,boot_id,revision,transport_epoch,control_epoch,deadline_ms,phase,manual_fence,status,commands,selected,updated_ms,initial_commands FROM native_scene_request WHERE controller=?1 AND request_key=?2",params![controller,key],|r|{
            let n=|i| ->rusqlite::Result<u64>{u64::try_from(r.get::<_,i64>(i)?).map_err(|_|rusqlite::Error::InvalidQuery)};
            let status:String=r.get(11)?;let commands:String=r.get(12)?;
            Ok(Record{request:Request{controller:controller.into(),key:key.into(),scene:r.get(0)?,group:r.get(1)?,variable:r.get(2)?,variable_version:n(3)?,boot:r.get(4)?,epochs:Epochs{configuration:n(5)?,transport:n(6)?,control:n(7)?},deadline_ms:n(8)?},phase:n(9)?,fence:n(10)?,status:match status.as_str(){"pending"=>Status::Pending,"observed"=>Status::Observed,"unknown"=>Status::Unknown,_=>return Err(rusqlite::Error::InvalidQuery)},commands:if commands.is_empty(){vec![]}else{commands.split(',').map(|n|n.parse::<u64>().map_err(|_|rusqlite::Error::InvalidQuery)).collect::<rusqlite::Result<Vec<_>>>()?},initial_commands:{let s:String=r.get(15)?;if s.is_empty(){vec![]}else{s.split(',').map(|n|n.parse::<u64>().map_err(|_|rusqlite::Error::InvalidQuery)).collect::<rusqlite::Result<Vec<_>>>()?}},selected:r.get(13)?,updated_ms:n(14)?})
        }).optional()?)
    }
    /// Reserve before the first physical command. A later rejection intentionally
    /// leaves the key consumed; there is no rollback permission to retry Toggle.
    pub fn reserve_native_scene(&mut self, request: &Request, now: u64) -> Result<(), Error> {
        if ![
            &request.controller,
            &request.key,
            &request.scene,
            &request.group,
            &request.variable,
            &request.boot,
        ]
        .into_iter()
        .all(|s| valid(s))
            || request.key.len() > 128
            || !request
                .key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || request.epochs.transport == 0
            || request.epochs.control == 0
            || request.deadline_ms <= now
        {
            return Err(Error::InvalidAudit);
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision = sql_revision(request.epochs.configuration)?;
        let active: i64 = tx.query_row(
            "SELECT revision FROM active_configuration WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let version: i64 = tx.query_row(
            "SELECT version FROM logical_variable WHERE revision=?1 AND name=?2",
            params![revision, request.variable],
            |r| r.get(0),
        )?;
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_scene_request WHERE boot_id=?1 AND revision=?2 AND transport_epoch=?3 AND control_epoch=?4 AND group_id=?5 AND status='pending')",params![request.boot,revision,number(request.epochs.transport)?,number(request.epochs.control)?,request.group],|r|r.get(0))?;
        let count: i64 = tx.query_row("SELECT count(*) FROM native_scene_request", [], |r| {
            r.get(0)
        })?;
        if active != revision
            || version != number(request.variable_version)?
            || busy
            || count >= 100000
        {
            return Err(Error::Conflict);
        }
        tx.execute("INSERT INTO native_scene_request(controller,request_key,scene_id,group_id,variable_name,variable_version,boot_id,revision,transport_epoch,control_epoch,deadline_ms,phase,manual_fence,status,commands,initial_commands,selected,updated_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,0,'pending','','',NULL,?12)",params![request.controller,request.key,request.scene,request.group,request.variable,version,request.boot,revision,number(request.epochs.transport)?,number(request.epochs.control)?,number(request.deadline_ms)?,number(now)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn native_scene_pending(
        &self,
        boot: &str,
        epochs: Epochs,
        group: &str,
    ) -> Result<bool, Error> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM native_scene_request WHERE boot_id=?1 AND revision=?2 AND transport_epoch=?3 AND control_epoch=?4 AND group_id=?5 AND status='pending')",params![boot,sql_revision(epochs.configuration)?,number(epochs.transport)?,number(epochs.control)?,group],|r|r.get(0))?)
    }
    pub fn native_scene_failure_after(
        &self,
        boot: &str,
        epochs: Epochs,
        group: &str,
    ) -> Result<Option<u64>, Error> {
        let n:Option<i64>=self.connection.query_row("SELECT max(updated_ms) FROM native_scene_request WHERE boot_id=?1 AND revision=?2 AND transport_epoch=?3 AND control_epoch=?4 AND group_id=?5 AND status='unknown'",params![boot,sql_revision(epochs.configuration)?,number(epochs.transport)?,number(epochs.control)?,group],|r|r.get(0))?;
        n.map(|n| u64::try_from(n).map_err(|_| Error::InvalidAudit))
            .transpose()
    }
    /// Advance before queue admission. Even an abandoned/failed phase remains
    /// consumed so a lost response cannot issue the same physical frame twice.
    pub fn consume_native_phase(
        &mut self,
        request: &Request,
        phase: u64,
        now: u64,
    ) -> Result<(), Error> {
        let changed=self.connection.execute("UPDATE native_scene_request SET phase=phase+1,updated_ms=?4 WHERE controller=?1 AND request_key=?2 AND phase=?3 AND status='pending' AND boot_id=?5 AND revision=?6 AND transport_epoch=?7 AND control_epoch=?8 AND variable_version=(SELECT version FROM logical_variable WHERE revision=?6 AND name=variable_name) AND deadline_ms>?4",params![request.controller,request.key,number(phase)?,number(now)?,request.boot,sql_revision(request.epochs.configuration)?,number(request.epochs.transport)?,number(request.epochs.control)?])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn native_phase_accepted(
        &mut self,
        request: &Request,
        phase: u64,
        ids: &[u64],
        fence: u64,
        now: u64,
    ) -> Result<(), Error> {
        if ids.len() > 128 || ids.contains(&0) {
            return Err(Error::InvalidAudit);
        }
        let ids = ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let changed=self.connection.execute("UPDATE native_scene_request SET commands=?4,initial_commands=CASE WHEN phase=1 THEN ?4 ELSE initial_commands END,manual_fence=?5,updated_ms=?6 WHERE controller=?1 AND request_key=?2 AND phase=?3 AND status='pending'",params![request.controller,request.key,number(phase)?,ids,number(fence)?,number(now)?])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn fail_native_scene(
        &mut self,
        controller: &str,
        key: &str,
        now: u64,
    ) -> Result<(), Error> {
        self.connection.execute("UPDATE native_scene_request SET status='unknown',selected=NULL,updated_ms=?3 WHERE controller=?1 AND request_key=?2 AND status='pending'",params![controller,key,number(now)?])?;
        Ok(())
    }
    /// Recovery is audit-only: no command or request is returned to replay.
    pub fn expire_native_scenes(
        &mut self,
        boot: &str,
        epochs: Epochs,
        now: u64,
    ) -> Result<(), Error> {
        self.connection.execute("UPDATE native_scene_request SET status='unknown',selected=NULL,updated_ms=CASE WHEN boot_id=?1 THEN ?5 ELSE updated_ms END WHERE status='pending' AND (boot_id!=?1 OR revision!=?2 OR transport_epoch!=?3 OR control_epoch!=?4 OR deadline_ms<=?5)",params![boot,sql_revision(epochs.configuration)?,number(epochs.transport)?,number(epochs.control)?,number(now)?])?;
        Ok(())
    }
    /// Only confirmed post-query values become native dimmer settings. The
    /// existing journal pointer prevents overwriting a newer absolute setting.
    pub fn finish_native_scene(
        &mut self,
        request: &Request,
        phase: u64,
        selected: Option<i64>,
        levels: &[(DeviceId, u64, Percent)],
        now: u64,
    ) -> Result<(), Error> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for (device, toggle, level) in levels {
            let row:i64=tx.query_row("SELECT rowid FROM command_journal WHERE boot_id=?1 AND command_id=?2 AND device_id=?3 AND revision=?4 AND transport_epoch=?5 AND status='written' AND intent=x'01070000'",params![request.boot,number(*toggle)?,device.as_str(),sql_revision(request.epochs.configuration)?,number(request.epochs.transport)?],|r|r.get(0))?;
            tx.execute("INSERT INTO light_setting(revision,device_id,level,journal_row) VALUES(?1,?2,?3,?4) ON CONFLICT(revision,device_id) DO UPDATE SET level=excluded.level,journal_row=excluded.journal_row WHERE light_setting.journal_row<excluded.journal_row",params![sql_revision(request.epochs.configuration)?,device.as_str(),level.get(),row])?;
        }
        let changed=tx.execute("UPDATE native_scene_request SET status='observed',selected=?4,updated_ms=?5 WHERE controller=?1 AND request_key=?2 AND phase=?3 AND status='pending' AND variable_version=(SELECT version FROM logical_variable WHERE revision=?6 AND name=variable_name)",params![request.controller,request.key,number(phase)?,selected,number(now)?,sql_revision(request.epochs.configuration)?])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        tx.commit()?;
        Ok(())
    }
}
