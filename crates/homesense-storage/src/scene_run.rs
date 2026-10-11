//! Durable scene lifecycle, distinct from physical target confirmation.
//! Interrupted runs are never runnable plans and contain no replay payload.
use crate::{Error, Store, sql_revision};
use rusqlite::{OptionalExtension, params};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    CommandsWritten,
    Partial,
    Cancelled,
    Failed,
    Interrupted,
}
impl Status {
    fn text(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::CommandsWritten => "commands_written",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
    fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "commands_written" => Self::CommandsWritten,
            "partial" => Self::Partial,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            "interrupted" => Self::Interrupted,
            _ => return Err(Error::InvalidAudit),
        })
    }
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}
#[derive(Debug, Clone)]
pub struct Record {
    pub revision: u64,
    pub definition: Option<String>,
    pub started_ms: u64,
    pub updated_ms: u64,
    pub version: u64,
    pub status: Status,
}
fn key(s: &str) -> Result<(), Error> {
    if s.is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
        Err(Error::InvalidAudit)
    } else {
        Ok(())
    }
}
/// A durable pending receipt, never a persisted replay plan.
#[derive(Debug, Clone)]
pub struct Start {
    pub action_index: usize,
    pub source: String,
    pub run: String,
    pub revision: u64,
    pub definition: String,
}
pub(crate) fn insert_start(tx:&rusqlite::Transaction<'_>,boot:&str,start:&Start,now:u64,active:Option<i64>)->Result<(),Error> {
    key(boot)?;key(&start.run)?;
    if start.definition.is_empty() || start.definition.len()>256 || start.definition.chars().any(char::is_control) {return Err(Error::InvalidAudit);}
    let revision=sql_revision(start.revision)?;
    if active!=Some(revision) {return Err(Error::Conflict);}
    let now=i64::try_from(now).map_err(|_|Error::InvalidAudit)?;
    tx.execute("INSERT INTO scene_run(boot_id,run_id,revision,definition,started_ms,updated_ms,status,version) VALUES(?1,?2,?3,?4,?5,?5,'pending',0)",params![boot,start.run,revision,start.definition,now])?;
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub correlation: u128,
    pub event: u64,
    pub action_index: usize,
    pub source: String,
}
pub(crate) fn insert_cause(tx:&rusqlite::Transaction<'_>,boot:&str,start:&Start,audit:&crate::rule_audit::Admission)->Result<(),Error> {
    if start.action_index>=65536 || start.source.is_empty() || start.source.len()>8192 {return Err(Error::InvalidAudit);}
    tx.execute("INSERT INTO scene_cause(boot_id,run_id,correlation,event_id,action_index,source) VALUES(?1,?2,?3,?4,?5,?6)",params![boot,start.run,format!("{:032x}",audit.correlation),i64::try_from(audit.event).map_err(|_|Error::InvalidAudit)?,start.action_index as i64,start.source])?;
    Ok(())
}
impl Store {
    pub fn scene_cause(&self,boot:&str,run:&str)->Result<Option<Cause>,Error> {
        key(boot)?;key(run)?;
        let row=self.connection.query_row("SELECT correlation,event_id,action_index,source FROM scene_cause WHERE boot_id=?1 AND run_id=?2",params![boot,run],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).optional()?;
        row.map(|(correlation,event,index,source)|Ok(Cause{correlation:u128::from_str_radix(&correlation,16).map_err(|_|Error::InvalidAudit)?,event:u64::try_from(event).map_err(|_|Error::InvalidAudit)?,action_index:usize::try_from(index).map_err(|_|Error::InvalidAudit)?,source})).transpose()
    }
}
impl Store {
    pub fn begin_scene_run(
        &mut self,
        boot: &str,
        run: &str,
        revision: u64,
        definition: Option<&str>,
        now: u64,
    ) -> Result<(), Error> {
        key(boot)?;
        key(run)?;
        if definition
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(Error::InvalidAudit);
        }
        let revision = sql_revision(revision)?;
        let now = i64::try_from(now).map_err(|_| Error::InvalidAudit)?;
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
        tx.execute("INSERT INTO scene_run(boot_id,run_id,revision,definition,started_ms,updated_ms,status,version) VALUES(?1,?2,?3,?4,?5,?5,'pending',0)", params![boot,run,revision,definition,now])?;
        tx.commit()?;
        Ok(())
    }
    pub fn scene_run(&self, boot: &str, run: &str) -> Result<Option<Record>, Error> {
        key(boot)?;
        key(run)?;
        let row = self.connection.query_row("SELECT revision,definition,started_ms,updated_ms,version,status FROM scene_run WHERE boot_id=?1 AND run_id=?2", params![boot,run], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?))).optional()?;
        row.map(
            |(revision, definition, started_ms, updated_ms, version, status)| {
                Ok(Record {
                    revision: u64::try_from(revision).map_err(|_| Error::InvalidAudit)?,
                    definition,
                    started_ms: u64::try_from(started_ms).map_err(|_| Error::InvalidAudit)?,
                    updated_ms: u64::try_from(updated_ms).map_err(|_| Error::InvalidAudit)?,
                    version: u64::try_from(version).map_err(|_| Error::InvalidAudit)?,
                    status: Status::parse(&status)?,
                })
            },
        )
        .transpose()
    }
    /// A terminal snapshot cannot be changed. Late completion from an older
    /// configuration may settle its own receipt, but cannot create a new run.
    pub fn checkpoint_scene_run(
        &mut self,
        boot: &str,
        run: &str,
        expected: u64,
        status: Status,
        now: u64,
    ) -> Result<u64, Error> {
        key(boot)?;
        key(run)?;
        if status == Status::Pending || status == Status::Interrupted {
            return Err(Error::InvalidAudit);
        }
        let expected = i64::try_from(expected).map_err(|_| Error::InvalidAudit)?;
        let next = expected.checked_add(1).ok_or(Error::InvalidAudit)?;
        let now = i64::try_from(now).map_err(|_| Error::InvalidAudit)?;
        let changed = self.connection.execute("UPDATE scene_run SET status=?1,updated_ms=?2,version=?3 WHERE boot_id=?4 AND run_id=?5 AND version=?6 AND status IN ('pending','running') AND updated_ms<=?2", params![status.text(),now,next,boot,run,expected])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(next as u64)
    }
    /// Called only by the exclusive startup owner. Keep old boot-relative times;
    /// a new process clock cannot date an earlier process's interruption.
    pub fn recover_scene_runs(&mut self, current_boot: &str) -> Result<usize, Error> {
        key(current_boot)?;
        Ok(self.connection.execute("UPDATE scene_run SET status='interrupted',version=version+1 WHERE boot_id<>?1 AND status IN ('pending','running')", [current_boot])?)
    }
}

#[derive(Debug, Clone)]
pub struct StepLink {
    pub run: String,
    pub index: usize,
    pub source: String,
}
pub(crate) fn insert_step(
    tx: &rusqlite::Transaction<'_>,
    boot: &str,
    link: &StepLink,
    commands: &[homesense_core::queue::QueuedCommand],
    active: Option<i64>,
) -> Result<(), Error> {
    key(&link.run)?;
    if link.index >= 4096
        || link.source.is_empty()
        || link.source.len() > 8192
        || commands.is_empty()
    {
        return Err(Error::InvalidAudit);
    }
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision,status FROM scene_run WHERE boot_id=?1 AND run_id=?2",
            params![boot, link.run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((revision, status)) = row else {
        return Err(Error::InvalidAudit);
    };
    if active != Some(revision)
        || !matches!(status.as_str(), "pending" | "running")
        || commands.iter().any(|c| {
            c.origin != homesense_core::queue::Origin::Rule
                || i64::try_from(c.epochs.configuration).ok() != Some(revision)
        })
    {
        return Err(Error::InvalidAudit);
    }
    tx.execute(
        "INSERT INTO scene_step(boot_id,run_id,step_index,source) VALUES(?1,?2,?3,?4)",
        params![boot, link.run, link.index as i64, link.source],
    )?;
    for command in commands {
        tx.execute(
            "INSERT INTO scene_command(boot_id,run_id,step_index,command_id) VALUES(?1,?2,?3,?4)",
            params![
                boot,
                link.run,
                link.index as i64,
                i64::try_from(command.id).map_err(|_| Error::InvalidAudit)?
            ],
        )?;
    }
    Ok(())
}
#[derive(Debug)]
pub struct CommandRecord {
    pub step: usize,
    pub source: String,
    pub command_id: u64,
    pub device: String,
    pub status: crate::journal::Status,
}
impl Store {
    /// Commands are ordered by durable admission ID; reading does not resume them.
    pub fn scene_commands(
        &self,
        boot: &str,
        run: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<CommandRecord>, Error> {
        key(boot)?;
        key(run)?;
        if !(1..=128).contains(&limit) {
            return Err(Error::InvalidAudit);
        }
        let after = i64::try_from(after).map_err(|_| Error::InvalidAudit)?;
        let mut statement = self.connection.prepare("SELECT c.step_index,s.source,c.command_id,j.device_id,j.status FROM scene_command c JOIN scene_step s ON s.boot_id=c.boot_id AND s.run_id=c.run_id AND s.step_index=c.step_index JOIN command_journal j ON j.boot_id=c.boot_id AND j.command_id=c.command_id WHERE c.boot_id=?1 AND c.run_id=?2 AND c.command_id>?3 ORDER BY c.command_id LIMIT ?4")?;
        let rows = statement
            .query_map(params![boot, run, after, limit as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(step, source, id, device, status)| {
                Ok(CommandRecord {
                    step: usize::try_from(step).map_err(|_| Error::InvalidAudit)?,
                    source,
                    command_id: u64::try_from(id).map_err(|_| Error::InvalidAudit)?,
                    device,
                    status: crate::journal::Status::parse(&status)?,
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum Query {
    Latest {
        definition: String,
    },
    Run {
        boot: String,
        run: String,
        after: u64,
        limit: usize,
    },
}
impl Query {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Latest { definition } => {
                if definition.is_empty()
                    || definition.len() > 256
                    || definition.chars().any(char::is_control)
                {
                    return Err(Error::InvalidAudit);
                }
            }
            Self::Run {
                boot,
                run,
                after,
                limit,
            } => {
                key(boot)?;
                key(run)?;
                if *after > i64::MAX as u64 || !(1..=128).contains(limit) {
                    return Err(Error::InvalidAudit);
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Page {
    pub boot: String,
    pub run: String,
    pub record: Record,
    pub commands: Vec<CommandRecord>,
    pub next_after: Option<u64>,
}
impl Store {
    /// Latest means insertion order, not incomparable clocks from different
    /// process boots. One read transaction covers both receipt and command rows.
    pub fn query_scene(&self, revision: u64, query: &Query) -> Result<Option<Page>, Error> {
        query.validate()?;
        let revision = sql_revision(revision)?;
        let tx = self.connection.unchecked_transaction()?;
        let identity = match query {
            Query::Latest {definition} => {
                tx.query_row("SELECT boot_id,run_id FROM scene_run WHERE revision=?1 AND definition=?2 ORDER BY rowid DESC LIMIT 1",params![revision,definition],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,0,128))).optional()?
            }
            Query::Run {boot,run,after,limit} => Some((boot.clone(),run.clone(),*after,*limit)),
        };
        let result = if let Some((boot, run, after, limit)) = identity {
            if let Some(record) = self.scene_run(&boot, &run)? {
                let commands = self.scene_commands(&boot, &run, after, limit)?;
                let next_after = if commands.len() == limit {
                    commands.last().map(|c| c.command_id)
                } else {
                    None
                };
                Some(Page {
                    boot,
                    run,
                    record,
                    commands,
                    next_after,
                })
            } else {
                None
            }
        } else {
            None
        };
        tx.commit()?;
        Ok(result)
    }
}
