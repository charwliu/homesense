//! A rule admission receipt is not evidence of a successful physical action.
use crate::{Error, sql_revision};
use homesense_core::queue::{Origin, QueuedCommand};
use rusqlite::{OptionalExtension, Transaction, params};
/// One exact source label per resulting physical command. NULL remains valid
/// for legacy callers/records; labels are never inferred from command kinds.
pub fn validate_sources(sources: Option<&[String]>, commands: usize) -> Result<(), Error> {
    if let Some(sources) = sources
        && (commands > 128 || sources.len() != commands
            || sources.iter().any(|s| s.is_empty() || s.len() > 8192))
    {
        return Err(Error::InvalidAudit);
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct Admission {
    pub revision: u64,
    pub correlation: u128,
    pub event: u64,
    pub parent: u64,
    pub rule: String,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert(
    tx: &Transaction<'_>,
    boot: &str,
    audit: &Admission,
    commands: &[QueuedCommand],
    sources: Option<&[String]>,
    now: u64,
    active: Option<i64>,
) -> Result<(), Error> {
    validate_sources(sources, commands.len())?;
    let revision = sql_revision(audit.revision)?;
    if active != Some(revision) {
        return Err(Error::Conflict);
    }
    if audit.correlation == 0
        || audit.event == 0
        || audit.parent >= audit.event
        || audit.rule.is_empty()
        || audit.rule.len() > 256
        || audit.rule.chars().any(char::is_control)
        || commands
            .iter()
            .any(|c| c.origin != Origin::Rule || c.epochs.configuration != audit.revision)
    {
        return Err(Error::InvalidAudit);
    }
    let event = i64::try_from(audit.event).map_err(|_| Error::InvalidAudit)?;
    let parent = i64::try_from(audit.parent).map_err(|_| Error::InvalidAudit)?;
    let now = i64::try_from(now).map_err(|_| Error::InvalidAudit)?;
    let correlation = format!("{:032x}", audit.correlation);
    if parent != 0 {
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM rule_admission WHERE boot_id=?1 AND correlation=?2 AND event_id=?3 AND revision=?4)",params![boot,correlation,parent,revision],|r|r.get(0))?;
        if !exists {
            return Err(Error::InvalidAudit);
        }
    }
    tx.execute("INSERT INTO rule_admission(boot_id,correlation,event_id,parent_id,revision,rule_id,admitted_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![boot,correlation,event,parent,revision,audit.rule,now])?;
    for (index, command) in commands.iter().enumerate() {
        tx.execute(
            "INSERT INTO rule_command(boot_id,correlation,event_id,command_id,source) VALUES(?1,?2,?3,?4,?5)",
            params![
                boot,
                correlation,
                event,
                i64::try_from(command.id).map_err(|_| Error::InvalidAudit)?,
                sources.map(|labels| labels[index].as_str())
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub id: u64,
    pub device: String,
    pub status: crate::journal::Status,
    /// Exact accepted node label; None denotes an untraced historical caller.
    pub source: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Record {
    pub admission: Admission,
    pub admitted_ms: u64,
    pub commands: Vec<CommandResult>,
}
#[derive(Debug, Clone)]
pub struct Outcome {
    pub revision: u64,
    pub finished_ms: u64,
    /// None means dispatch completed, not that any physical target was reached.
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Page {
    pub outcome: Option<Outcome>,
    pub records: Vec<Record>,
    pub next_after: Option<u64>,
}
impl crate::Store {
    /// Read one consistent, bounded page. This does not replay or acknowledge
    /// commands; Written is still not a physical-state confirmation.
    pub fn rule_chain(
        &mut self,
        boot: &str,
        correlation: u128,
        after: u64,
        limit: usize,
    ) -> Result<Page, Error> {
        if boot.is_empty() || boot.len() > 128 || correlation == 0 || !(1..=64).contains(&limit) {
            return Err(Error::InvalidAudit);
        }
        let after = i64::try_from(after).map_err(|_| Error::InvalidAudit)?;
        let tx = self.connection.transaction()?;
        let key = format!("{correlation:032x}");
        let rows = {
            let mut statement=tx.prepare("SELECT event_id,parent_id,revision,rule_id,admitted_ms FROM rule_admission WHERE boot_id=?1 AND correlation=?2 AND event_id>?3 ORDER BY event_id LIMIT ?4")?;
            statement
                .query_map(params![boot, key, after, limit as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let unsigned = |value: i64| u64::try_from(value).map_err(|_| Error::InvalidAudit);
        let mut records = vec![];
        for (event, parent, revision, rule, time) in rows {
            let mut statement=tx.prepare("SELECT j.command_id,j.device_id,j.status,r.source FROM rule_command r JOIN command_journal j ON j.boot_id=r.boot_id AND j.command_id=r.command_id WHERE r.boot_id=?1 AND r.correlation=?2 AND r.event_id=?3 ORDER BY j.command_id LIMIT 129")?;
            let values = statement
                .query_map(params![boot, key, event], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if values.len() > 128 {
                return Err(Error::InvalidAudit);
            }
            let commands = values
                .into_iter()
                .map(|(id, device, status, source)| {
                    if source.as_ref().is_some_and(|s| s.is_empty() || s.len() > 8192) {
                        return Err(Error::InvalidAudit);
                    }
                    Ok(CommandResult {
                        id: unsigned(id)?,
                        device,
                        status: crate::journal::Status::parse(&status)?,
                        source,
                    })
                })
                .collect::<Result<_, Error>>()?;
            records.push(Record {
                admission: Admission {
                    revision: unsigned(revision)?,
                    correlation,
                    event: unsigned(event)?,
                    parent: unsigned(parent)?,
                    rule,
                },
                admitted_ms: unsigned(time)?,
                commands,
            });
        }
        let next_after = if records.len() == limit {
            records.last().map(|r| r.admission.event)
        } else {
            None
        };
        let outcome=tx.query_row("SELECT revision,finished_ms,error FROM rule_chain_outcome WHERE boot_id=?1 AND correlation=?2",params![boot,key],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?))).optional()?.map(|(revision,time,error)|Ok::<_,Error>(Outcome{revision:unsigned(revision)?,finished_ms:unsigned(time)?,error})).transpose()?;
        tx.commit()?;
        Ok(Page {
            outcome,
            records,
            next_after,
        })
    }
}

impl crate::Store {
    pub fn finish_rule_chain(
        &mut self,
        boot: &str,
        revision: u64,
        correlation: u128,
        now: u64,
        error: Option<&str>,
    ) -> Result<(), Error> {
        if boot.is_empty()
            || boot.len() > 128
            || correlation == 0
            || error.is_some_and(|e| e.len() > 8192)
        {
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
        tx.execute("INSERT INTO rule_chain_outcome(boot_id,correlation,revision,finished_ms,error) VALUES(?1,?2,?3,?4,?5)",params![boot,format!("{correlation:032x}"),revision,i64::try_from(now).map_err(|_|Error::InvalidAudit)?,error])?;
        tx.commit()?;
        Ok(())
    }
}
