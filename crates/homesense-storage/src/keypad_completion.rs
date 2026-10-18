//! Atomic receipt for a key-specific physical completion. This records queue
//! acceptance, not LED delivery or a replay plan. Source authorization and the
//! live feedback lease remain the Admission owner's responsibility.
use crate::{Error, Store, sql_revision, variables::Update};
use homesense_core::{
    Command, DeviceId,
    queue::{Epochs, QueuedCommand},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};

pub struct Commit {
    pub update: Update,
    pub policy: String,
    pub sequence: u64,
    pub endpoint: DeviceId,
    pub request_id: String,
    pub epochs: Epochs,
    /// Next command ID captured before the original gesture's admission.
    pub command_fence: u64,
    /// Actual complete writer callbacks retained by the original source.
    pub written: BTreeMap<u64, u64>,
    pub observations: BTreeMap<DeviceId, u64>,
    pub after_device: DeviceId,
    pub after_written: u64,
    pub original_deadline_ms: u64,
    pub send_before_ms: u64,
}
impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn keypad_completion_feedback_settled(
        &self,
        boot: &str,
        device: &DeviceId,
        at: Option<u64>,
        epoch: u64,
        fence: u64,
        owned: &BTreeMap<u64, u64>,
    ) -> Result<bool, Error> {
        feedback_settled(&self.connection, boot, device, at, epoch, fence, owned)
    }
    pub fn ensure_keypad_completion_audit(&self) -> Result<(), Error> {
        self.ensure_component_mode_audit()?;
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS keypad_completion_audit (
            boot TEXT NOT NULL,sequence INTEGER NOT NULL CHECK(sequence>0),revision INTEGER NOT NULL,
            endpoint TEXT NOT NULL,request_id TEXT NOT NULL,policy TEXT NOT NULL,controller TEXT NOT NULL,
            target_name TEXT NOT NULL,target_version INTEGER NOT NULL,transport_epoch INTEGER NOT NULL,
            control_epoch INTEGER NOT NULL,at_ms INTEGER NOT NULL,original_deadline_ms INTEGER NOT NULL,
            send_before_ms INTEGER NOT NULL,command_fence INTEGER NOT NULL,tickets TEXT NOT NULL,
            observations TEXT NOT NULL,led_commands TEXT NOT NULL,PRIMARY KEY(boot,sequence))")?;
        Ok(())
    }

    /// Check queued controls as well as manual overrides. The committing
    /// transaction repeats this check; a read here is not commit authority.
    pub fn keypad_completion_dependencies_current(
        &self,
        boot: &str,
        revision: u64,
        command_fence: u64,
        owned: &BTreeSet<u64>,
        devices: &BTreeSet<DeviceId>,
    ) -> Result<bool, Error> {
        dependencies_current(
            &self.connection,
            boot,
            revision,
            command_fence,
            owned,
            devices,
        )
    }
}
fn number(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn actuator(bytes: &[u8]) -> bool {
    matches!(bytes, [1, 1..=3 | 7..=8, _, 0])
}
#[allow(clippy::too_many_arguments)]
fn feedback_settled(
    db: &Connection,
    boot: &str,
    device: &DeviceId,
    at: Option<u64>,
    epoch: u64,
    fence: u64,
    owned: &BTreeMap<u64, u64>,
) -> Result<bool, Error> {
    let Some(at) = at else {
        return Ok(false);
    };
    let mut query=db.prepare("SELECT j.command_id,j.status,j.updated_ms,j.intent FROM command_journal j WHERE j.boot_id=?1 AND (j.device_id=?2 OR EXISTS(SELECT 1 FROM module_off_effect m WHERE m.boot_id=j.boot_id AND m.command_id=j.command_id AND m.device_id=?2)) AND (j.status IN ('accepted','writing') OR (j.status IN ('written','observed','failed','uncertain') AND j.transport_epoch>=?3 AND j.updated_ms>=?4))")?;
    let rows = query.query_map(
        params![boot, device.as_str(), number(epoch)?, number(at)?],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        },
    )?;
    for row in rows {
        let (id, status, updated, intent) = row?;
        if !actuator(&intent) {
            continue;
        }
        let id = u64::try_from(id).map_err(|_| Error::InvalidAudit)?;
        // An older Observed receipt is settled at its actual observation
        // boundary. It is not relabelled as one of this gesture's writes.
        if status == "observed"
            && (owned.get(&id).is_some_and(|written| *written < at)
                || (id < fence && updated <= number(at)?))
        {
            continue;
        }
        return Ok(false);
    }
    Ok(true)
}
type CommandReceipt = (i64, i64, i64, String, Vec<u8>, String, i64, i64);

fn dependencies_current(
    db: &Connection,
    boot: &str,
    revision: u64,
    command_fence: u64,
    owned: &BTreeSet<u64>,
    devices: &BTreeSet<DeviceId>,
) -> Result<bool, Error> {
    if command_fence == 0 || devices.is_empty() || devices.len() > 64 || owned.len() > 512 {
        return Err(Error::InvalidAudit);
    }
    // Whole-module Off also affects nonrepresentative journal devices.
    let mut query = db.prepare("SELECT j.command_id,j.intent FROM command_journal j WHERE j.boot_id=?1 AND j.revision=?2 AND j.command_id>=?3
        AND (j.device_id=?4 OR EXISTS(SELECT 1 FROM module_off_effect m WHERE m.boot_id=j.boot_id AND m.command_id=j.command_id AND m.device_id=?4))")?;
    for device in devices {
        let rows = query.query_map(
            params![
                boot,
                number(revision)?,
                number(command_fence)?,
                device.as_str()
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        for row in rows {
            let (id, intent) = row?;
            let id = u64::try_from(id).map_err(|_| Error::InvalidAudit)?;
            if actuator(&intent) && !owned.contains(&id) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Called inside the same IMMEDIATE transaction as variable CAS and queue
/// insertion. Failure here rolls both back, including their component audit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert(
    tx: &Transaction<'_>,
    boot: &str,
    controller: &str,
    commands: &[QueuedCommand],
    variables: &[Update],
    completion: &Commit,
    now: u64,
    active: Option<i64>,
) -> Result<(), Error> {
    let u = &completion.update;
    if active != Some(sql_revision(u.revision)?) || completion.epochs.configuration != u.revision {
        return Err(Error::Conflict);
    }
    if completion.sequence == 0
        || completion.command_fence == 0
        || !text(controller, 128)
        || !text(&completion.policy, 256)
        || !text(completion.endpoint.as_str(), 256)
        || completion.request_id.is_empty()
        || completion.request_id.len() > 128
        || !completion
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        || completion.epochs.transport == 0
        || completion.epochs.control == 0
        || u.value.is_none()
        || completion.written.is_empty()
        || completion.written.len() > 512
        || completion.observations.is_empty()
        || completion.observations.len() > 64
        || commands.is_empty()
        || commands.len() > 16
        || variables.len() != 1
        || variables[0].revision != u.revision
        || variables[0].name != u.name
        || variables[0].expected_version != u.expected_version
        || variables[0].value != u.value
        || now >= completion.send_before_ms
        || completion.send_before_ms > completion.original_deadline_ms
        || completion.send_before_ms.saturating_sub(now) > 2_000
        || completion.observations.values().any(|at| *at > now)
        || completion
            .observations
            .get(&completion.after_device)
            .is_none_or(|at| *at <= completion.after_written)
    {
        return Err(Error::InvalidAudit);
    }
    let mut outputs = BTreeSet::new();
    for command in commands {
        if !matches!(command.command, Command::PanelLed(_))
            || command.epochs != completion.epochs
            || command.deadline_ms > completion.send_before_ms
            || command.device_id == completion.endpoint
            || !outputs.insert(&command.device_id)
            || completion.written.contains_key(&command.id)
        {
            return Err(Error::InvalidAudit);
        }
    }
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM keypad_completion_audit WHERE boot=?1 AND sequence=?2)",
        params![boot, number(completion.sequence)?],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(Error::Conflict);
    }
    let mut trigger_written = false;
    for (id, written) in &completion.written {
        let row:Option<CommandReceipt>=tx.query_row(
            "SELECT revision,transport_epoch,control_epoch,device_id,intent,status,updated_ms,deadline_ms
             FROM command_journal WHERE boot_id=?1 AND command_id=?2",
            params![boot,number(*id)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).optional()?;
        let Some((revision, transport, control, device, intent, status, updated, deadline)) = row
        else {
            return Err(Error::Conflict);
        };
        if *id < completion.command_fence
            || *written > now
            || number(*written)? >= deadline
            || revision != number(completion.epochs.configuration)?
            || transport != number(completion.epochs.transport)?
            || control != number(completion.epochs.control)?
            || (status != "written" && status != "observed")
            || (status == "written" && updated != number(*written)?)
            || (status == "observed" && (!actuator(&intent) || updated < number(*written)?))
        {
            return Err(Error::Conflict);
        }
        if actuator(&intent) && intent.as_slice() != [1, 8, 0, 0] {
            if completion
                .observations
                .get(&DeviceId::new(&device).map_err(|_| Error::InvalidAudit)?)
                .is_some_and(|at| *at <= *written)
            {
                return Err(Error::Conflict);
            }
            if device == completion.after_device.as_str() && *written == completion.after_written {
                trigger_written = true;
            }
        }
    }
    if !trigger_written {
        return Err(Error::InvalidAudit);
    }
    for (device, at) in &completion.observations {
        if !feedback_settled(
            tx,
            boot,
            device,
            Some(*at),
            completion.epochs.transport,
            completion.command_fence,
            &completion.written,
        )? {
            return Err(Error::Conflict);
        }
    }
    if !dependencies_current(
        tx,
        boot,
        u.revision,
        completion.command_fence,
        &completion.written.keys().copied().collect(),
        &completion.observations.keys().cloned().collect(),
    )? {
        return Err(Error::Conflict);
    }
    let version = u
        .expected_version
        .checked_add(1)
        .ok_or(Error::InvalidAudit)?;
    let tickets = completion
        .written
        .keys()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    tx.execute("INSERT INTO component_mode_audit VALUES(?1,?2,?3,?4,?5,?6,?7,'local_written_with_feedback_inference',?8)",
        params![number(u.revision)?,u.name,number(version)?,completion.policy,boot,controller,number(now)?,tickets])?;
    let observations = completion
        .observations
        .iter()
        .map(|(id, at)| (id.as_str(), *at))
        .collect::<BTreeMap<_, _>>();
    tx.execute("INSERT INTO keypad_completion_audit VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![boot,number(completion.sequence)?,number(u.revision)?,completion.endpoint.as_str(),completion.request_id,completion.policy,
            controller,u.name,number(version)?,number(completion.epochs.transport)?,number(completion.epochs.control)?,number(now)?,
            number(completion.original_deadline_ms)?,number(completion.send_before_ms)?,number(completion.command_fence)?,tickets,
            serde_json::to_string(&observations).map_err(|_|Error::InvalidAudit)?,commands.iter().map(|command|command.id.to_string()).collect::<Vec<_>>().join(",")])?;
    Ok(())
}
