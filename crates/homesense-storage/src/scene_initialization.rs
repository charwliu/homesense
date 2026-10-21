//! One-time local selection derived from reviewed, complete physical feedback.
//! The admission owner validates policy, scope and feedback before this CAS.
use crate::{Error, Store, sql_revision};
use homesense_core::{queue::Epochs, rule::Value};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub group: String,
    pub boot_id: u128,
    pub epochs: Epochs,
    pub observed_ms: u64,
    pub initialized_ms: u64,
    pub value: i64,
}
pub(crate) fn origin(
    connection: &Connection,
    revision: u64,
    name: &str,
    version: i64,
) -> Result<Option<Origin>, Error> {
    Ok(connection.prepare_cached("SELECT group_id,boot_id,transport_epoch,control_epoch,observed_ms,initialized_ms,value FROM scene_initialization WHERE revision=?1 AND name=?2 AND variable_version=?3")?.query_row(
        params![sql_revision(revision)?, name, version],
        |row| {
            let integer = |i| -> rusqlite::Result<u64> {u64::try_from(row.get::<_,i64>(i)?).map_err(|_|rusqlite::Error::InvalidQuery)};
            let boot: String = row.get(1)?;
            Ok(Origin {group:row.get(0)?,boot_id:u128::from_str_radix(&boot,16).map_err(|_|rusqlite::Error::InvalidQuery)?,epochs:Epochs{configuration:revision,transport:integer(2)?,control:integer(3)?},observed_ms:integer(4)?,initialized_ms:integer(5)?,value:row.get(6)?})
        },
    ).optional()?)
}
impl Store {
    /// Does not dispatch commands, run scenes, or alter an observer baseline.
    /// Only a never-written raw NULL variable (version 0) can be initialized.
    pub fn initialize_scene_selection(
        &mut self,
        name: &str,
        expected_version: u64,
        source: &Origin,
    ) -> Result<(), Error> {
        let valid =
            |s: &str| !s.trim().is_empty() && s.len() <= 250 && !s.chars().any(char::is_control);
        if expected_version != 0
            || !valid(name)
            || !valid(&source.group)
            || source.boot_id == 0
            || source.epochs.transport == 0
            || source.epochs.control == 0
            || source.observed_ms > source.initialized_ms
            || source.value < 0
        {
            return Err(Error::InvalidAudit);
        }
        let revision = sql_revision(source.epochs.configuration)?;
        let integer = |value: u64| i64::try_from(value).map_err(|_| Error::InvalidAudit);
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if active != Some(revision) {
            return Err(Error::Conflict);
        }
        let uninitialized:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM logical_variable v WHERE revision=?1 AND name=?2 AND kind='integer' AND value IS NULL AND version=?3 AND NOT EXISTS(SELECT 1 FROM scene_observation o WHERE o.revision=v.revision AND o.name=v.name AND o.variable_version=v.version) AND NOT EXISTS(SELECT 1 FROM scene_initialization i WHERE i.revision=v.revision AND i.name=v.name))",params![revision,name,integer(expected_version)?],|row|row.get(0))?;
        if !uninitialized {
            return Err(Error::Conflict);
        }
        crate::variables::apply(
            &tx,
            &[crate::variables::Update {
                revision: source.epochs.configuration,
                name: name.into(),
                expected_version,
                value: Some(Value::Integer(source.value)),
            }],
            active,
        )?;
        tx.execute("INSERT INTO scene_initialization(revision,name,variable_version,group_id,boot_id,transport_epoch,control_epoch,observed_ms,initialized_ms,value) VALUES(?1,?2,1,?3,?4,?5,?6,?7,?8,?9)",params![revision,name,source.group,format!("{:032x}",source.boot_id),integer(source.epochs.transport)?,integer(source.epochs.control)?,integer(source.observed_ms)?,integer(source.initialized_ms)?,source.value])?;
        tx.commit()?;
        Ok(())
    }
    pub fn has_pending_scene(&self, boot: &str, revision: u64) -> Result<bool, Error> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM scene_run WHERE boot_id=?1 AND revision=?2 AND status IN ('pending','running'))",params![boot,sql_revision(revision)?],|row|row.get(0))?)
    }
}
