//! Version-bound inferred selections. No command, scene receipt or LED output.
//! Callers validate reviewed key ownership and the live event scope before commit.
use crate::{Error, Store, sql_revision};
use homesense_core::{
    DeviceId,
    queue::Epochs,
    rule::{Lifetime, Value, ValueType},
    scene_observer::{Authority, Basis, Inference, Scope, Selection},
};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Effective value; a different boot/epoch/stream makes an inference unknown.
    pub stored: crate::variables::Stored,
    /// Retained even when invalidated, so unknown never loses its source label.
    pub evidence: Option<Inference>,
    /// Whether that evidence belongs to the caller's current complete scope.
    pub evidence_current: bool,
    /// Historical local initialization origin, bound to the current version.
    pub initialization: Option<crate::scene_initialization::Origin>,
    /// Ephemeral current-boot projection attached by the runtime only. Stored
    /// historical values are never promoted to physical truth by this module.
    pub physical: Option<PhysicalProjection>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalProjection {
    pub boot_id: u128,
    pub epochs: Epochs,
    pub observed_ms: Option<u64>,
    pub history_value: Option<Value>,
}
impl Snapshot {
    pub fn selection(&self) -> Selection {
        Selection {
            value: match self.stored.value {
                Some(Value::Integer(value)) => Some(value),
                _ => None,
            },
            version: self.stored.version,
            authority: self
                .evidence
                .as_ref()
                .map_or(Authority::Recorded, |v| Authority::Inferred(v.scope)),
        }
    }
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
fn snapshot(
    connection: &Connection,
    revision: u64,
    name: &str,
    scope: Option<Scope>,
) -> Result<Option<Snapshot>, Error> {
    let row: Option<(String, Option<i64>, i64)> = connection
        .query_row(
            "SELECT kind,value,version FROM logical_variable WHERE revision=?1 AND name=?2",
            params![sql_revision(revision)?, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((kind, mut value, version)) = row else {
        return Ok(None);
    };
    if kind != "integer" {
        return Err(Error::InvalidAudit);
    }
    let evidence = connection.query_row(
        "SELECT group_id,boot_id,transport_epoch,control_epoch,stream_generation,event_sequence,device_id,observed_ms,basis,value FROM scene_observation WHERE revision=?1 AND name=?2 AND variable_version=?3 AND authority='inferred'",
        params![sql_revision(revision)?, name, version],
        |r| {
            let decode = |column: usize| -> rusqlite::Result<u64> {
                u64::try_from(r.get::<_, i64>(column)?).map_err(|_| rusqlite::Error::InvalidQuery)
            };
            let boot: String = r.get(1)?;
            let device: String = r.get(6)?;
            let basis: String = r.get(8)?;
            Ok(Inference {
                group: r.get(0)?, expected_version: u64::try_from(version-1).map_err(|_| rusqlite::Error::InvalidQuery)?,
                value: r.get(9)?,
                basis: match basis.as_str() { "unknown_initial"=>Basis::UnknownInitial, "select_member"=>Basis::SelectMember, "toggle_off"=>Basis::ToggleOff, _=>return Err(rusqlite::Error::InvalidQuery) },
                scope: Scope {
                    boot_id: u128::from_str_radix(&boot,16).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    epochs: Epochs {configuration:revision,transport:decode(2)?,control:decode(3)?},
                    stream_generation:decode(4)?,
                },
                sequence:decode(5)?, device:DeviceId::new(device).map_err(|_|rusqlite::Error::InvalidQuery)?, observed_ms:decode(7)?,
            })
        },
    ).optional()?;
    if let Some(evidence) = &evidence {
        if value != evidence.value {
            return Err(Error::InvalidAudit);
        }
        if scope != Some(evidence.scope) {
            value = None;
        }
    }
    let evidence_current = evidence.as_ref().is_some_and(|e| Some(e.scope) == scope);
    let initialization = crate::scene_initialization::origin(connection, revision, name, version)?;
    if initialization.as_ref().is_some_and(|origin| evidence.is_some() || value != Some(origin.value)) {
        return Err(Error::InvalidAudit);
    }
    Ok(Some(Snapshot {
        physical: None,
        initialization,
        evidence_current,
        stored: crate::variables::Stored {
            lifetime: Lifetime::Persistent,
            kind: ValueType::Integer,
            value: value.map(Value::Integer),
            version: u64::try_from(version).map_err(|_| Error::InvalidAudit)?,
        },
        evidence,
    }))
}
impl Store {
    /// A coherent read transaction for selection plus provenance. `None` scope
    /// is useful for read-only inspection: every inferred value becomes unknown.
    pub fn scene_selection(
        &self,
        revision: u64,
        name: &str,
        scope: Option<Scope>,
    ) -> Result<Option<Snapshot>, Error> {
        let tx = self.connection.unchecked_transaction()?;
        let result = snapshot(&tx, revision, name, scope)?;
        tx.commit()?;
        Ok(result)
    }

    /// CAS the actual variable and append immutable source evidence together.
    /// A stale/duplicate event or CAS conflict must be discarded, never replanned
    /// against a newer manual selection. This function does not authorize keys;
    /// the admission actor must validate the mapping, freshness and current scope.
    pub fn record_scene_inference(
        &mut self,
        revision: u64,
        name: &str,
        inference: &Inference,
    ) -> Result<(), Error> {
        self.record_scene_inference_inner(revision, name, inference, false)
    }

    /// Commit a reviewed external toggle result. The admission owner validates
    /// the independent candidate transition, mapping and fresh stream first.
    /// Local variables (including LED clears) are only CAS guards: they are not
    /// the external controller's prior state. Still stored as inferred evidence.
    pub fn record_external_scene_inference(
        &mut self,
        revision: u64,
        name: &str,
        inference: &Inference,
    ) -> Result<(), Error> {
        self.record_scene_inference_inner(revision, name, inference, true)
    }

    fn record_scene_inference_inner(
        &mut self,
        revision: u64,
        name: &str,
        inference: &Inference,
        external: bool,
    ) -> Result<(), Error> {
        let scope = inference.scope;
        if scope.boot_id == 0
            || scope.epochs.configuration != revision
            || scope.epochs.transport == 0
            || scope.epochs.control == 0
            || scope.stream_generation == 0
            || inference.sequence == 0
            || inference.group.trim().is_empty()
            || inference.group.len() > 256
            || inference.group.chars().any(char::is_control)
            || name.trim().is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
        {
            return Err(Error::InvalidAudit);
        }
        let revision_sql = sql_revision(revision)?;
        let transport = integer(scope.epochs.transport)?;
        let control = integer(scope.epochs.control)?;
        let generation = integer(scope.stream_generation)?;
        let sequence = integer(inference.sequence)?;
        let observed = integer(inference.observed_ms)?;
        let version = integer(inference.expected_version)?
            .checked_add(1)
            .ok_or(Error::InvalidAudit)?;
        let boot = format!("{:032x}", scope.boot_id);
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
        if active != Some(revision_sql) {
            return Err(Error::Conflict);
        }
        let previous = snapshot(&tx, revision, name, Some(scope))?.ok_or(Error::InvalidAudit)?;
        if previous.stored.version != inference.expected_version {
            return Err(Error::Conflict);
        }
        let valid = if external {
            match (inference.basis, inference.value) {
                (Basis::UnknownInitial, None) | (Basis::ToggleOff, Some(0)) => true,
                (Basis::SelectMember, Some(next)) => next > 0,
                _ => false,
            }
        } else {
            match (inference.basis, previous.selection().value, inference.value) {
                (Basis::UnknownInitial, None, None) => true,
                (Basis::SelectMember, Some(old), Some(next)) => old >= 0 && next > 0 && old != next,
                (Basis::ToggleOff, Some(old), Some(0)) => old > 0,
                _ => false,
            }
        };
        if !valid {
            return Err(Error::InvalidAudit);
        }
        let latest:Option<i64> = tx.query_row(
            "SELECT max(event_sequence) FROM scene_observation WHERE boot_id=?1 AND revision=?2 AND transport_epoch=?3 AND control_epoch=?4 AND stream_generation=?5",
            params![boot,revision_sql,transport,control,generation],|r|r.get(0),
        )?;
        if latest.is_some_and(|latest| sequence <= latest) {
            return Err(Error::Conflict);
        }
        crate::variables::apply(
            &tx,
            &[crate::variables::Update {
                revision,
                name: name.into(),
                expected_version: inference.expected_version,
                value: inference.value.map(Value::Integer),
            }],
            active,
        )?;
        tx.execute(
            "INSERT INTO scene_observation(revision,name,variable_version,authority,group_id,boot_id,transport_epoch,control_epoch,stream_generation,event_sequence,device_id,observed_ms,basis,value) VALUES(?1,?2,?3,'inferred',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![revision_sql,name,version,inference.group,boot,transport,control,generation,sequence,inference.device.as_str(),observed,match inference.basis {Basis::UnknownInitial=>"unknown_initial",Basis::SelectMember=>"select_member",Basis::ToggleOff=>"toggle_off"},inference.value],
        )?;
        tx.commit()?;
        Ok(())
    }
}
