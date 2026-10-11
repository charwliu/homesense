//! SQLite configuration revisions and atomic active-pointer publication.
//! A single runtime owner coordinates this store with the arbitration barrier.
//! Configuration schema validation precedes this opaque payload storage layer.
pub mod journal;
pub mod light_restore;
use homesense_core::queue::CancelReason;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, time::Duration};
const APPLICATION_ID: i64 = 0x48534E53;
const MAX_PAYLOAD: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Database(rusqlite::Error),
    Configuration(homesense_config::Error),
    ForeignDatabase,
    UnsupportedSchema(i64),
    InvalidRevision,
    InvalidPayload,
    InvalidAudit,
    Conflict,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    pub revision: u64,
    pub payload: Vec<u8>,
}
#[derive(Debug)]
pub struct CancellationAudit<'a> {
    pub boot_id: &'a str,
    pub command_id: u64,
    pub reason: CancelReason,
}

pub struct Store {
    connection: Connection,
}
fn sql_revision(value: u64) -> Result<i64, Error> {
    if value == 0 {
        return Err(Error::InvalidRevision);
    }
    i64::try_from(value).map_err(|_| Error::InvalidRevision)
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let app: i64 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if app != 0 && app != APPLICATION_ID {
            return Err(Error::ForeignDatabase);
        }
        if version != 0 && (!(1..=4).contains(&version) || app != APPLICATION_ID) {
            return Err(Error::UnsupportedSchema(version));
        }
        if version == 0 {
            let tables: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err(Error::ForeignDatabase);
            }
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch("CREATE TABLE configuration_revision (
                revision INTEGER PRIMARY KEY CHECK(revision > 0), payload BLOB NOT NULL CHECK(length(payload) > 0 AND length(payload) <= 4194304));
                CREATE TABLE active_configuration (singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL REFERENCES configuration_revision(revision));
                CREATE TABLE publication_cancellation (revision INTEGER NOT NULL REFERENCES configuration_revision(revision), boot_id TEXT NOT NULL, command_id INTEGER NOT NULL CHECK(command_id>0), reason TEXT NOT NULL, PRIMARY KEY(revision,boot_id,command_id));
                CREATE TRIGGER immutable_configuration_update BEFORE UPDATE ON configuration_revision BEGIN SELECT RAISE(ABORT, 'immutable configuration'); END;
                CREATE TRIGGER immutable_configuration_delete BEFORE DELETE ON configuration_revision BEGIN SELECT RAISE(ABORT, 'immutable configuration'); END;")?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 1 {
            tx.execute_batch("CREATE TABLE command_journal (
                boot_id TEXT NOT NULL, command_id INTEGER NOT NULL CHECK(command_id>0),
                revision INTEGER NOT NULL REFERENCES configuration_revision(revision),
                device_id TEXT NOT NULL, origin TEXT NOT NULL, intent BLOB NOT NULL CHECK(length(intent)=4),
                transport_epoch INTEGER NOT NULL, control_epoch INTEGER NOT NULL,
                deadline_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, run_id INTEGER,
                status TEXT NOT NULL CHECK(status IN ('accepted','writing','written','observed','cancelled','failed','uncertain')),
                recovered_by TEXT, PRIMARY KEY(boot_id,command_id));")?;
            tx.pragma_update(None, "user_version", 2)?;
        }
        if version <= 2 {
            tx.execute_batch("ALTER TABLE command_journal ADD COLUMN cancellation_reason TEXT;")?;
            tx.pragma_update(None, "user_version", 3)?;
        }
        if version <= 3 {
            tx.execute_batch("CREATE TABLE light_restore (revision INTEGER NOT NULL REFERENCES configuration_revision(revision), device_id TEXT NOT NULL, level INTEGER NOT NULL CHECK(level BETWEEN 1 AND 100), PRIMARY KEY(revision,device_id));")?;
            tx.pragma_update(None, "user_version", 4)?;
        }
        tx.commit()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }
    /// Validate before opening the publication transaction. The payload revision
    /// is authoritative, preventing a mismatch between SQL and configuration IDs.
    pub fn publish_configuration(
        &mut self,
        expected: Option<u64>,
        payload: &[u8],
        context: homesense_core::ExecutionContext,
        cancellations: &[CancellationAudit<'_>],
    ) -> Result<homesense_config::Configuration, Error> {
        let config = homesense_config::Configuration::parse(payload, context)
            .map_err(Error::Configuration)?;
        self.publish(expected, config.revision(), payload, cancellations)?;
        Ok(config)
    }
    /// Startup path: revalidate stored data for the requested execution context.
    pub fn active_configuration(
        &self,
        context: homesense_core::ExecutionContext,
    ) -> Result<Option<homesense_config::Configuration>, Error> {
        let Some(stored) = self.active()? else {
            return Ok(None);
        };
        let config = homesense_config::Configuration::parse(&stored.payload, context)
            .map_err(Error::Configuration)?;
        if config.revision() != stored.revision {
            return Err(Error::InvalidRevision);
        }
        Ok(Some(config))
    }
    pub fn active(&self) -> Result<Option<Configuration>, Error> {
        Ok(self.connection.query_row("SELECT r.revision,r.payload FROM active_configuration a JOIN configuration_revision r ON a.revision=r.revision WHERE a.singleton=1", [], |r| Ok(Configuration { revision: r.get::<_, i64>(0)? as u64, payload: r.get(1)? })).optional()?)
    }
    pub fn revision(&self, revision: u64) -> Result<Option<Configuration>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT revision,payload FROM configuration_revision WHERE revision=?1",
                [sql_revision(revision)?],
                |r| {
                    Ok(Configuration {
                        revision: r.get::<_, i64>(0)? as u64,
                        payload: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }
    /// Compare-and-swap publication, including cancellation audit in ONE transaction.
    /// On any database error the caller must resolve the durable active pointer;
    /// it must not assume rollback or resume cancelled commands.
    pub fn publish(
        &mut self,
        expected: Option<u64>,
        revision: u64,
        payload: &[u8],
        cancellations: &[CancellationAudit<'_>],
    ) -> Result<(), Error> {
        let revision = sql_revision(revision)?;
        let expected = expected.map(sql_revision).transpose()?;
        if payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(Error::InvalidPayload);
        }
        if cancellations.len() > 65536
            || cancellations.iter().any(|c| {
                c.boot_id.trim().is_empty()
                    || c.boot_id.len() > 128
                    || c.command_id == 0
                    || c.command_id > i64::MAX as u64
            })
        {
            return Err(Error::InvalidAudit);
        }
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
        if active != expected {
            return Err(Error::Conflict);
        }
        let latest: Option<i64> = tx.query_row(
            "SELECT max(revision) FROM configuration_revision",
            [],
            |r| r.get(0),
        )?;
        if latest.is_some_and(|v| revision <= v) {
            return Err(Error::InvalidRevision);
        }
        tx.execute(
            "INSERT INTO configuration_revision(revision,payload) VALUES (?1,?2)",
            params![revision, payload],
        )?;
        for cancellation in cancellations {
            tx.execute("INSERT INTO publication_cancellation(revision,boot_id,command_id,reason) VALUES (?1,?2,?3,?4)", params![revision,cancellation.boot_id,cancellation.command_id as i64,format!("{:?}", cancellation.reason)])?;
        }
        tx.execute("INSERT INTO active_configuration(singleton,revision) VALUES (1,?1) ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision", [revision])?;
        tx.commit()?;
        Ok(())
    }
    pub fn cancellation_count(&self, revision: u64) -> Result<u64, Error> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM publication_cancellation WHERE revision=?1",
            [sql_revision(revision)?],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}
