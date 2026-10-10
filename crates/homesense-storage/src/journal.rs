//! Durable command audit, never a replay queue. Runtime owns queue reservation
//! and must not dispatch before acceptance commits.
use crate::{Error, Store, sql_revision};
use homesense_core::{
    Command, LevelTransition, MotorMotion,
    queue::{Cancellation, Origin, QueuedCommand},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Accepted,
    Writing,
    Written,
    Observed,
    Cancelled,
    Failed,
    Uncertain,
}
impl Status {
    fn text(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Writing => "writing",
            Self::Written => "written",
            Self::Observed => "observed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
    fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "accepted" => Ok(Self::Accepted),
            "writing" => Ok(Self::Writing),
            "written" => Ok(Self::Written),
            "observed" => Ok(Self::Observed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            "uncertain" => Ok(Self::Uncertain),
            _ => Err(Error::InvalidAudit),
        }
    }
}
fn boot(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        Err(Error::InvalidAudit)
    } else {
        Ok(())
    }
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
/// Version 1 audit encoding: version, action, value, reserved. Not bus bytes.
fn intent(command: Command) -> [u8; 4] {
    let (tag, value) = match command {
        Command::Query => (0, 0),
        Command::Switch(v) => (1, u8::from(v)),
        Command::SetLevel { level, transition } => (
            if transition == LevelTransition::Immediate {
                2
            } else {
                3
            },
            level.get(),
        ),
        Command::PauseRamp => (4, 0),
        Command::Motor(m) => (
            5,
            match m {
                MotorMotion::Up => 0,
                MotorMotion::Down => 1,
                MotorMotion::Stop => 2,
            },
        ),
        Command::PanelLed(v) => (6, u8::from(v)),
    };
    [1, tag, value, 0]
}
impl Store {
    /// All commands are committed together against the currently active revision.
    /// Duplicate IDs fail the whole transaction; this is not request idempotency.
    pub fn accept_commands(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        now_ms: u64,
    ) -> Result<(), Error> {
        self.accept_queue_batch(boot_id, commands, &[], now_ms)
    }
    /// Persist accepted commands and queue coalescing/STOP cancellations together.
    pub fn accept_queue_batch(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if cancellations.len() > 4096 {
            return Err(Error::InvalidAudit);
        }
        if commands.is_empty() || commands.len() > 128 {
            return Err(Error::InvalidAudit);
        }
        let now = integer(now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        for command in commands {
            let revision = sql_revision(command.epochs.configuration)?;
            if active != Some(revision) {
                return Err(Error::Conflict);
            }
            if command.id == 0
                || command.device_id.as_str().len() > 256
                || command.epochs.transport == 0
                || command.epochs.control == 0
                || command.deadline_ms <= now_ms
            {
                return Err(Error::InvalidAudit);
            }
            let origin = match command.origin {
                Origin::Http => "http",
                Origin::Hap => "hap",
                Origin::Panel => "panel",
                Origin::Rule => "rule",
                Origin::Background => "background",
            };
            tx.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,run_id,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'accepted')",params![boot_id,integer(command.id)?,revision,command.device_id.as_str(),origin,intent(command.command).as_slice(),integer(command.epochs.transport)?,integer(command.epochs.control)?,integer(command.deadline_ms)?,now,command.run_id.map(integer).transpose()?])?;
        }
        for cancellation in cancellations {
            let command = &cancellation.command;
            let count = tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason=?2 WHERE boot_id=?3 AND command_id=?4 AND status='accepted' AND revision=?5 AND device_id=?6 AND intent=?7 AND updated_ms<=?1", params![now,format!("{:?}",cancellation.reason),boot_id,integer(command.id)?,sql_revision(command.epochs.configuration)?,command.device_id.as_str(),intent(command.command).as_slice()])?;
            if count != 1 {
                return Err(Error::Conflict);
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn command_status(&self, boot_id: &str, id: u64) -> Result<Option<Status>, Error> {
        boot(boot_id)?;
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT status FROM command_journal WHERE boot_id=?1 AND command_id=?2",
                params![boot_id, integer(id)?],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| Status::parse(&s)).transpose()
    }
    pub fn transition_command(
        &mut self,
        boot_id: &str,
        id: u64,
        from: Status,
        to: Status,
        now_ms: u64,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if !matches!(
            (from, to),
            (
                Status::Accepted,
                Status::Writing | Status::Cancelled | Status::Failed
            ) | (
                Status::Writing,
                Status::Written | Status::Failed | Status::Uncertain
            ) | (
                Status::Written,
                Status::Observed | Status::Failed | Status::Uncertain
            )
        ) {
            return Err(Error::InvalidAudit);
        }
        let count = self.connection.execute("UPDATE command_journal SET status=?1,updated_ms=?2 WHERE boot_id=?3 AND command_id=?4 AND status=?5 AND updated_ms<=?2",params![to.text(),integer(now_ms)?,boot_id,integer(id)?,from.text()])?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Call during exclusive startup with a fresh boot ID. Returns only an audit
    /// count; no recovered command is returned for transport replay.
    pub fn recover_commands(&mut self, current_boot: &str) -> Result<usize, Error> {
        boot(current_boot)?;
        Ok(self.connection.execute("UPDATE command_journal SET status='uncertain',recovered_by=?1 WHERE boot_id<>?1 AND status IN ('accepted','writing','written')",[current_boot])?)
    }
}
