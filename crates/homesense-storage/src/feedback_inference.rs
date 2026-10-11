//! Durable candidate attribution. A matching target never proves its sender.
use crate::{Error, Store, sql_revision};
use homesense_core::{DeviceId, Observation, queue::Epochs};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

#[derive(Debug, Clone)]
pub struct Inference {
    pub sequence: u64,
    pub epochs: Epochs,
    pub correlation: u128,
    pub parent_event: u64,
    pub command_id: u64,
    pub device: DeviceId,
    pub observed: Observation,
    pub observed_ms: u64,
    pub window_ms: u64,
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
impl Store {
    /// Claim an observation once, before continuation. Validates durable write
    /// and original rule/scene ownership; never updates command or device state.
    /// Duplicate claims fail, so retrying cannot admit the same event twice.
    pub fn record_feedback_inference(
        &mut self,
        boot: &str,
        inference: &Inference,
    ) -> Result<(), Error> {
        let i = inference;
        if boot.is_empty()
            || boot.len() > 128
            || i.sequence == 0
            || i.correlation == 0
            || i.parent_event == 0
            || i.command_id == 0
            || i.window_ms == 0
            || i.window_ms > 60_000
        {
            return Err(Error::InvalidAudit);
        }
        let (kind, value) = match i.observed {
            Observation::Switch(v) => ("switch", u8::from(v)),
            Observation::Level(v) => ("level", v.get()),
            Observation::Button { .. } => return Err(Error::InvalidAudit),
        };
        let revision = sql_revision(i.epochs.configuration)?;
        let sequence = integer(i.sequence)?;
        let parent = integer(i.parent_event)?;
        let command = integer(i.command_id)?;
        let transport = integer(i.epochs.transport)?;
        let control = integer(i.epochs.control)?;
        let observed = integer(i.observed_ms)?;
        let correlation = format!("{:032x}", i.correlation);
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
        if active != Some(revision) {
            return Err(Error::Conflict);
        }
        let write: Option<(Vec<u8>, i64)> = tx.query_row(
            "SELECT intent,updated_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2 AND revision=?3 AND transport_epoch=?4 AND control_epoch=?5 AND device_id=?6 AND origin='rule' AND status='written'",
            params![boot,command,revision,transport,control,i.device.as_str()],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((intent, written)) = write else {
            return Err(Error::InvalidAudit);
        };
        let target_matches = match (kind, intent.as_slice()) {
            ("switch", [1, 1, target, 0]) => *target == value,
            ("level", [1, 2 | 3, target, 0]) => *target == value,
            _ => false,
        };
        if !target_matches
            || written < 0
            || observed < written
            || observed - written >= i.window_ms as i64
        {
            return Err(Error::InvalidAudit);
        }
        // Expanded immediate commands belong to rule_command. Delayed and fade
        // commands belong through scene_command -> scene_cause.
        let owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rule_command WHERE boot_id=?1 AND command_id=?2 AND correlation=?3 AND event_id=?4)
             OR EXISTS(SELECT 1 FROM scene_command c JOIN scene_cause s ON s.boot_id=c.boot_id AND s.run_id=c.run_id WHERE c.boot_id=?1 AND c.command_id=?2 AND s.correlation=?3 AND s.event_id=?4)",
            params![boot,command,correlation,parent], |r| r.get(0),
        )?;
        let terminal: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rule_chain_outcome WHERE boot_id=?1 AND correlation=?2)",
            params![boot, correlation],
            |r| r.get(0),
        )?;
        if !owned || terminal {
            return Err(Error::InvalidAudit);
        }
        tx.execute("INSERT INTO feedback_inference(boot_id,sequence,revision,transport_epoch,control_epoch,correlation,parent_event,command_id,device_id,observation_kind,observation_value,written_ms,observed_ms,window_ms,attribution) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'inferred')",
            params![boot,sequence,revision,transport,control,correlation,parent,command,i.device.as_str(),kind,i64::from(value),written,observed,i.window_ms as i64])?;
        tx.commit()?;
        Ok(())
    }
}
