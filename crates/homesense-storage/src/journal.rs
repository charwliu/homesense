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
    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
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
        self.accept_queue_batch_with_study(boot_id, commands, cancellations, now_ms, None)
    }
    pub fn study_mode(&self, revision: u64) -> Result<Option<u8>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM study_mode WHERE revision=?1",
                [sql_revision(revision)?],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn accept_queue_batch_with_study(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_variables(boot_id, commands, cancellations, now_ms, study, &[])
    }
    /// Logical variable CAS and command acceptance share the same transaction.
    pub fn accept_queue_batch_with_variables(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_rule_audit(
            boot_id,
            commands,
            cancellations,
            now_ms,
            study,
            variables,
            None,
        )
    }
    // One transaction owns the queue, variable CAS and optional causal audit.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_rule_audit(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
        audit: Option<&crate::rule_audit::Admission>,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_audits(
            boot_id,
            commands,
            cancellations,
            now_ms,
            study,
            variables,
            audit,
            None,
        )
    }
    /// All causal links commit with the command rows, never after admission.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_audits(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
        audit: Option<&crate::rule_audit::Admission>,
        scene: Option<&crate::scene_run::StepLink>,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if variables.len() > 1024 || (study.is_some() && commands.is_empty()) {
            return Err(Error::InvalidAudit);
        }
        if cancellations.len() > 4096 {
            return Err(Error::InvalidAudit);
        }
        if (commands.is_empty() && variables.is_empty() && audit.is_none()) || commands.len() > 128
        {
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
        if let Some((expected, next)) = study {
            if next > 3
                || expected.is_some_and(|v| v > 3)
                || commands
                    .iter()
                    .any(|c| c.origin != Origin::Rule || c.run_id != Some(commands[0].id))
            {
                return Err(Error::InvalidAudit);
            }
            let revision = sql_revision(commands[0].epochs.configuration)?;
            let current: Option<u8> = tx
                .query_row(
                    "SELECT value FROM study_mode WHERE revision=?1",
                    [revision],
                    |r| r.get(0),
                )
                .optional()?;
            if current != expected {
                return Err(Error::Conflict);
            }
            tx.execute("INSERT INTO study_mode(revision,value) VALUES(?1,?2) ON CONFLICT(revision) DO UPDATE SET value=excluded.value",params![revision,next])?;
        }
        if let Some(scene) = scene {
            crate::scene_run::insert_step(&tx, boot_id, scene, commands, active)?;
        }
        if let Some(audit) = audit {
            crate::rule_audit::insert(&tx, boot_id, audit, commands, now_ms, active)?;
        }
        crate::variables::apply(&tx, variables, active)?;
        cancel_in_transaction(&tx, boot_id, cancellations, now)?;
        tx.commit()?;
        Ok(())
    }
    /// Persist a publication/run barrier before attempting a configuration commit.
    /// Every snapshot must still be Accepted; any mismatch rolls back the batch.
    /// This never cancels Writing/Written commands or changes the active revision.
    pub fn cancel_queued(
        &mut self,
        boot_id: &str,
        cancellations: &[Cancellation],
        now_ms: u64,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if cancellations.len() > 4096 {
            return Err(Error::InvalidAudit);
        }
        let now = integer(now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        cancel_in_transaction(&tx, boot_id, cancellations, now)?;
        tx.commit()?;
        Ok(())
    }
    /// Close this boot's pending transport work atomically. Written commands
    /// remain written: loss of feedback is not proof their local write failed.
    pub fn fail_transport(&mut self, boot_id: &str, now_ms: u64) -> Result<(), Error> {
        boot(boot_id)?;
        let now = integer(now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let future: i64 = tx.query_row(
            "SELECT count(*) FROM command_journal WHERE boot_id=?1 AND status IN ('accepted','writing') AND updated_ms>?2",
            params![boot_id, now], |r| r.get(0))?;
        if future != 0 {
            return Err(Error::Conflict);
        }
        tx.execute("UPDATE command_journal SET status='cancelled',cancellation_reason='TransportFailed',updated_ms=?2 WHERE boot_id=?1 AND status='accepted'", params![boot_id, now])?;
        tx.execute("UPDATE command_journal SET status='uncertain',updated_ms=?2 WHERE boot_id=?1 AND status='writing'", params![boot_id, now])?;
        tx.commit()?;
        Ok(())
    }
    /// A feedback-based rule may not read through pending output or use a
    /// sample at/before the latest completed/ambiguous write. Cancelled output
    /// did not reach the transport and does not invalidate a prior observation.
    pub fn feedback_is_settled(
        &self,
        boot_id: &str,
        device: &homesense_core::DeviceId,
        observed_ms: Option<u64>,
    ) -> Result<bool, Error> {
        boot(boot_id)?;
        let sample = observed_ms.map(integer).transpose()?;
        let blocked:bool=self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND (status IN ('accepted','writing') OR (status IN ('written','observed','failed','uncertain') AND (?3 IS NULL OR updated_ms>=?3))))",
            params![boot_id,device.as_str(),sample],|row|row.get(0))?;
        Ok(!blocked)
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

fn cancel_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    boot_id: &str,
    cancellations: &[Cancellation],
    now: i64,
) -> Result<(), Error> {
    for cancellation in cancellations {
        let command = &cancellation.command;
        let count = tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason=?2 WHERE boot_id=?3 AND command_id=?4 AND status='accepted' AND revision=?5 AND device_id=?6 AND intent=?7 AND updated_ms<=?1 AND transport_epoch=?8 AND control_epoch=?9 AND deadline_ms=?10 AND run_id IS ?11",
            params![now, format!("{:?}", cancellation.reason), boot_id, integer(command.id)?,
                sql_revision(command.epochs.configuration)?, command.device_id.as_str(),
                intent(command.command).as_slice(), integer(command.epochs.transport)?,
                integer(command.epochs.control)?, integer(command.deadline_ms)?, command.run_id.map(integer).transpose()?])?;
        if count != 1 {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
