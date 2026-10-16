//! SQLite configuration revisions and atomic active-pointer publication.
//! A single runtime owner coordinates this store with the arbitration barrier.
//! Configuration schema validation precedes this opaque payload storage layer.
pub mod automation;
pub mod configuration_history;
pub mod calendar;
pub mod module_off;
pub mod journal;
pub mod feedback_inference;
pub mod light_restore;
pub mod rule_audit;
pub mod scene_run;
pub mod scene_history;
pub mod variables;
pub mod scene_observation;
pub mod scene_initialization;
pub mod native_scene;
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
    active_bundle_cache: std::cell::RefCell<Option<automation::CachedBundle>>,
}
fn sql_revision(value: u64) -> Result<i64, Error> {
    if value == 0 {
        return Err(Error::InvalidRevision);
    }
    i64::try_from(value).map_err(|_| Error::InvalidRevision)
}
pub const SCHEMA_VERSION:i64=27;
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let app: i64 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if app != 0 && app != APPLICATION_ID {
            return Err(Error::ForeignDatabase);
        }
        if version != 0 && (!(1..=SCHEMA_VERSION).contains(&version) || app != APPLICATION_ID) {
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
        if version <= 4 {
            tx.execute_batch("CREATE TABLE study_mode(revision INTEGER PRIMARY KEY REFERENCES configuration_revision(revision), value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 3));")?;
            tx.pragma_update(None, "user_version", 5)?;
        }
        if version <= 5 {
            tx.execute_batch("CREATE TABLE logical_variable (revision INTEGER NOT NULL REFERENCES configuration_revision(revision), name TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('bool','integer')), value INTEGER, version INTEGER NOT NULL DEFAULT 0 CHECK(version>=0), PRIMARY KEY(revision,name), CHECK(kind!='bool' OR value IS NULL OR value IN (0,1)));")?;
            tx.pragma_update(None, "user_version", 6)?;
        }
        if version <= 6 {
            tx.execute_batch("CREATE TABLE calendar_watermark (revision INTEGER PRIMARY KEY REFERENCES configuration_revision(revision), unix_ms INTEGER NOT NULL CHECK(unix_ms>=946684800000));")?;
            tx.pragma_update(None, "user_version", 7)?;
        }
        if version <= 7 {
            tx.execute_batch("CREATE TABLE rule_admission (boot_id TEXT NOT NULL, correlation TEXT NOT NULL, event_id INTEGER NOT NULL CHECK(event_id>0), parent_id INTEGER NOT NULL CHECK(parent_id>=0 AND parent_id<event_id), revision INTEGER NOT NULL REFERENCES configuration_revision(revision), rule_id TEXT NOT NULL, admitted_ms INTEGER NOT NULL, PRIMARY KEY(boot_id,correlation,event_id));
            CREATE TABLE rule_command (boot_id TEXT NOT NULL, correlation TEXT NOT NULL, event_id INTEGER NOT NULL, command_id INTEGER NOT NULL, PRIMARY KEY(boot_id,command_id), FOREIGN KEY(boot_id,correlation,event_id) REFERENCES rule_admission(boot_id,correlation,event_id), FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id));")?;
            tx.pragma_update(None, "user_version", 8)?;
        }
        if version <= 8 {
            tx.execute_batch("CREATE TABLE rule_chain_outcome (boot_id TEXT NOT NULL, correlation TEXT NOT NULL, revision INTEGER NOT NULL REFERENCES configuration_revision(revision), finished_ms INTEGER NOT NULL, error TEXT, PRIMARY KEY(boot_id,correlation));")?;
            tx.pragma_update(None, "user_version", 9)?;
        }
        if version <= 9 {
            tx.execute_batch("CREATE TABLE scene_run (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, revision INTEGER NOT NULL REFERENCES configuration_revision(revision), definition TEXT, started_ms INTEGER NOT NULL CHECK(started_ms>=0), updated_ms INTEGER NOT NULL CHECK(updated_ms>=started_ms), status TEXT NOT NULL CHECK(status IN ('pending','running','commands_written','partial','cancelled','failed','interrupted')), version INTEGER NOT NULL CHECK(version>=0), PRIMARY KEY(boot_id,run_id));")?;
            tx.pragma_update(None, "user_version", 10)?;
        }
        if version <= 10 {
            tx.execute_batch("CREATE TABLE scene_step (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, step_index INTEGER NOT NULL CHECK(step_index>=0 AND step_index<4096), source TEXT NOT NULL, PRIMARY KEY(boot_id,run_id,step_index), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));
            CREATE TABLE scene_command (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, step_index INTEGER NOT NULL, command_id INTEGER NOT NULL, PRIMARY KEY(boot_id,command_id), FOREIGN KEY(boot_id,run_id,step_index) REFERENCES scene_step(boot_id,run_id,step_index), FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id));")?;
            tx.pragma_update(None, "user_version", 11)?;
        }
        if version <= 11 {
            tx.execute_batch("CREATE TABLE automation_revision (revision INTEGER PRIMARY KEY REFERENCES configuration_revision(revision), payload BLOB NOT NULL);
            CREATE TRIGGER immutable_automation_update BEFORE UPDATE ON automation_revision BEGIN SELECT RAISE(ABORT, 'immutable automation'); END;
            CREATE TRIGGER immutable_automation_delete BEFORE DELETE ON automation_revision BEGIN SELECT RAISE(ABORT, 'immutable automation'); END;")?;
            tx.pragma_update(None, "user_version", 12)?;
        }
        if version <= 12 {
            tx.execute_batch("CREATE TABLE scene_cause (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, correlation TEXT NOT NULL, event_id INTEGER NOT NULL, action_index INTEGER NOT NULL CHECK(action_index>=0 AND action_index<65536), source TEXT NOT NULL, PRIMARY KEY(boot_id,run_id), UNIQUE(boot_id,correlation,event_id,action_index), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id), FOREIGN KEY(boot_id,correlation,event_id) REFERENCES rule_admission(boot_id,correlation,event_id));
            CREATE TRIGGER immutable_scene_cause_update BEFORE UPDATE ON scene_cause BEGIN SELECT RAISE(ABORT, 'immutable scene cause'); END;")?;
            tx.pragma_update(None, "user_version", 13)?;
        }
        if version <= 13 {
            tx.execute_batch("CREATE TABLE scene_sample (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, step_index INTEGER NOT NULL, sample_index INTEGER NOT NULL CHECK(sample_index>=0 AND sample_index<1024), first_command INTEGER NOT NULL, PRIMARY KEY(boot_id,run_id,step_index,sample_index), FOREIGN KEY(boot_id,run_id,step_index) REFERENCES scene_step(boot_id,run_id,step_index), FOREIGN KEY(boot_id,first_command) REFERENCES command_journal(boot_id,command_id));")?;
            tx.pragma_update(None, "user_version", 14)?;
        }
        if version <= 14 {
            tx.execute_batch("CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement')), PRIMARY KEY(boot_id,run_id,version), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));")?;
            tx.pragma_update(None, "user_version", 15)?;
        }
        if version <= 15 {
            tx.execute_batch("ALTER TABLE scene_cancellation RENAME TO scene_cancellation_old;
                CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement','cancel_requested')), PRIMARY KEY(boot_id,run_id,version), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));
                INSERT INTO scene_cancellation SELECT * FROM scene_cancellation_old;
                DROP TABLE scene_cancellation_old;")?;
            tx.pragma_update(None, "user_version", 16)?;
        }
        if version <= 16 {
            tx.execute_batch("CREATE TABLE scene_expiry (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, step_index INTEGER NOT NULL CHECK(step_index>=0 AND step_index<4096), version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, source TEXT NOT NULL, PRIMARY KEY(boot_id,run_id,step_index), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));")?;
            tx.pragma_update(None, "user_version", 17)?;
        }
        if version <= 17 {
            tx.execute_batch("CREATE TABLE feedback_inference (
                boot_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence>0),
                revision INTEGER NOT NULL, transport_epoch INTEGER NOT NULL, control_epoch INTEGER NOT NULL,
                correlation TEXT NOT NULL, parent_event INTEGER NOT NULL CHECK(parent_event>0),
                command_id INTEGER NOT NULL, device_id TEXT NOT NULL,
                observation_kind TEXT NOT NULL CHECK(observation_kind IN ('switch','level')),
                observation_value INTEGER NOT NULL, written_ms INTEGER NOT NULL,
                observed_ms INTEGER NOT NULL, window_ms INTEGER NOT NULL CHECK(window_ms>0 AND window_ms<=60000),
                attribution TEXT NOT NULL CHECK(attribution='inferred'),
                PRIMARY KEY(boot_id,sequence),
                FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id),
                FOREIGN KEY(boot_id,correlation,parent_event) REFERENCES rule_admission(boot_id,correlation,event_id));
                CREATE TRIGGER immutable_feedback_inference_update BEFORE UPDATE ON feedback_inference BEGIN SELECT RAISE(ABORT, 'immutable inference'); END;")?;
            tx.pragma_update(None, "user_version", 18)?;
        }
        if version <= 18 {
            tx.execute_batch("ALTER TABLE scene_cancellation RENAME TO scene_cancellation_v18;
                CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement','cancel_requested','service_stopped','configuration_published','transport_failure','preparation_failed')), PRIMARY KEY(boot_id,run_id,version), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));
                INSERT INTO scene_cancellation SELECT * FROM scene_cancellation_v18;
                DROP TABLE scene_cancellation_v18;")?;
            tx.pragma_update(None, "user_version", 19)?;
        }
        if version <= 19 {
            tx.execute_batch("ALTER TABLE scene_cancellation RENAME TO scene_cancellation_v19;
                CREATE TABLE scene_cancellation (boot_id TEXT NOT NULL, run_id TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), device_id TEXT NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('manual_override','scene_replacement','cancel_requested','service_stopped','configuration_published','transport_failure','preparation_failed','publication_barrier')), PRIMARY KEY(boot_id,run_id,version), FOREIGN KEY(boot_id,run_id) REFERENCES scene_run(boot_id,run_id));
                INSERT INTO scene_cancellation SELECT * FROM scene_cancellation_v19;
                DROP TABLE scene_cancellation_v19;")?;
            tx.pragma_update(None, "user_version", 20)?;
        }
        if version <= 20 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS light_setting (revision INTEGER NOT NULL REFERENCES configuration_revision(revision),device_id TEXT NOT NULL,level INTEGER NOT NULL CHECK(level BETWEEN 0 AND 100),journal_row INTEGER NOT NULL CHECK(journal_row>0),PRIMARY KEY(revision,device_id));
                INSERT INTO light_setting(revision,device_id,level,journal_row)
                SELECT j.revision,j.device_id,((instr('0123456789ABCDEF',substr(hex(j.intent),5,1))-1)*16+instr('0123456789ABCDEF',substr(hex(j.intent),6,1))-1),j.rowid FROM command_journal j
                WHERE j.status IN ('written','observed') AND substr(j.intent,1,2) IN (x'0102',x'0103')
                  AND j.rowid=(SELECT max(k.rowid) FROM command_journal k WHERE k.revision=j.revision AND k.device_id=j.device_id AND k.status IN ('written','observed') AND substr(k.intent,1,2) IN (x'0102',x'0103'))
                ON CONFLICT(revision,device_id) DO NOTHING;")?;
            tx.pragma_update(None, "user_version", 21)?;
        }
        if version <= 21 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS scene_observation (
                revision INTEGER NOT NULL, name TEXT NOT NULL, variable_version INTEGER NOT NULL CHECK(variable_version>0),
                authority TEXT NOT NULL CHECK(authority='inferred'), group_id TEXT NOT NULL,
                boot_id TEXT NOT NULL CHECK(length(boot_id)=32), transport_epoch INTEGER NOT NULL CHECK(transport_epoch>0),
                control_epoch INTEGER NOT NULL CHECK(control_epoch>0), stream_generation INTEGER NOT NULL CHECK(stream_generation>0),
                event_sequence INTEGER NOT NULL CHECK(event_sequence>0), device_id TEXT NOT NULL,
                observed_ms INTEGER NOT NULL CHECK(observed_ms>=0), basis TEXT NOT NULL CHECK(basis IN ('unknown_initial','select_member','toggle_off')),
                value INTEGER CHECK(value IS NULL OR value>=0),
                PRIMARY KEY(boot_id,revision,transport_epoch,control_epoch,stream_generation,event_sequence),
                UNIQUE(revision,name,variable_version),
                FOREIGN KEY(revision,name) REFERENCES logical_variable(revision,name));
                CREATE TRIGGER IF NOT EXISTS immutable_scene_observation_update BEFORE UPDATE ON scene_observation BEGIN SELECT RAISE(ABORT, 'immutable scene observation'); END;
                CREATE TRIGGER IF NOT EXISTS immutable_scene_observation_delete BEFORE DELETE ON scene_observation BEGIN SELECT RAISE(ABORT, 'immutable scene observation'); END;")?;
            tx.pragma_update(None, "user_version", 22)?;
        }
        if version <= 22 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS scene_initialization (
                revision INTEGER NOT NULL, name TEXT NOT NULL, variable_version INTEGER NOT NULL CHECK(variable_version=1),
                group_id TEXT NOT NULL, boot_id TEXT NOT NULL CHECK(length(boot_id)=32),
                transport_epoch INTEGER NOT NULL CHECK(transport_epoch>0), control_epoch INTEGER NOT NULL CHECK(control_epoch>0),
                observed_ms INTEGER NOT NULL CHECK(observed_ms>=0), initialized_ms INTEGER NOT NULL CHECK(initialized_ms>=observed_ms),
                value INTEGER NOT NULL CHECK(value>=0), PRIMARY KEY(revision,name),
                FOREIGN KEY(revision,name) REFERENCES logical_variable(revision,name));
                CREATE TRIGGER IF NOT EXISTS immutable_scene_initialization_update BEFORE UPDATE ON scene_initialization BEGIN SELECT RAISE(ABORT, 'immutable scene initialization'); END;
                CREATE TRIGGER IF NOT EXISTS immutable_scene_initialization_delete BEFORE DELETE ON scene_initialization BEGIN SELECT RAISE(ABORT, 'immutable scene initialization'); END;")?;
            tx.pragma_update(None, "user_version", 23)?;
        }
        if version <= 23 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS native_scene_request (
                controller TEXT NOT NULL, request_key TEXT NOT NULL, scene_id TEXT NOT NULL, group_id TEXT NOT NULL,
                variable_name TEXT NOT NULL, variable_version INTEGER NOT NULL CHECK(variable_version>=0), boot_id TEXT NOT NULL,
                revision INTEGER NOT NULL REFERENCES configuration_revision(revision), transport_epoch INTEGER NOT NULL CHECK(transport_epoch>0), control_epoch INTEGER NOT NULL CHECK(control_epoch>0),
                deadline_ms INTEGER NOT NULL CHECK(deadline_ms>=0),phase INTEGER NOT NULL CHECK(phase>=0),manual_fence INTEGER NOT NULL CHECK(manual_fence>=0),
                status TEXT NOT NULL CHECK(status IN ('pending','observed','unknown')),commands TEXT NOT NULL,initial_commands TEXT NOT NULL,selected INTEGER CHECK(selected IS NULL OR selected>=0),updated_ms INTEGER NOT NULL CHECK(updated_ms>=0),
                PRIMARY KEY(controller,request_key),FOREIGN KEY(revision,variable_name) REFERENCES logical_variable(revision,name));")?;
            tx.execute_batch("CREATE INDEX IF NOT EXISTS native_scope ON native_scene_request(boot_id,revision,transport_epoch,control_epoch,group_id,status); CREATE INDEX IF NOT EXISTS native_expiry ON native_scene_request(status,deadline_ms);")?;
            tx.pragma_update(None, "user_version", 24)?;
        }
        if version <= 24 {
            // Existing commands retain NULL: a migration cannot reconstruct
            // source nodes that were discarded by an older runtime.
            tx.execute_batch("ALTER TABLE rule_command ADD COLUMN source TEXT CHECK(source IS NULL OR length(CAST(source AS BLOB)) BETWEEN 1 AND 8192);")?;
            tx.pragma_update(None, "user_version", 25)?;
        }
        if version <= 25 {
            tx.execute_batch("CREATE TABLE module_off_effect (
                boot_id TEXT NOT NULL, command_id INTEGER NOT NULL,
                module_id TEXT NOT NULL, gateway_id TEXT NOT NULL,
                address INTEGER NOT NULL CHECK(address BETWEEN 1 AND 255), profile TEXT NOT NULL,
                channel INTEGER NOT NULL CHECK(channel BETWEEN 1 AND 16), device_id TEXT NOT NULL,
                PRIMARY KEY(boot_id,command_id,channel),
                FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id));
                CREATE INDEX module_off_device ON module_off_effect(boot_id,device_id,command_id);
                CREATE TRIGGER module_off_effect_update BEFORE UPDATE ON module_off_effect BEGIN SELECT RAISE(ABORT,'immutable module scope'); END;
                CREATE TRIGGER module_off_effect_delete BEFORE DELETE ON module_off_effect BEGIN SELECT RAISE(ABORT,'immutable module scope'); END;")?;
            tx.pragma_update(None,"user_version",26)?;
        }
        if version <= 26 {
            gateway_audit::migrate(&tx)?;
            tx.pragma_update(None, "user_version", 27)?;
        }
        // The LED index itself changes no row format or user_version.
        // Install transactionally for new databases and every supported old
        // schema after migration. The mandatory scope migration above separately
        // advances schema27, which old schema26 programs must reject.
        tx.execute_batch("CREATE INDEX IF NOT EXISTS command_led_request_scope ON command_journal(boot_id,revision,device_id,transport_epoch,control_epoch,command_id DESC) WHERE intent IN (x'01060000',x'01060100',x'010A0000')")?;
        tx.commit()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection, active_bundle_cache: Default::default() })
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
        Ok(self.active_bundle(context)?.map(|bundle| bundle.configuration))
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
        self.publish_inner(expected, revision, payload, cancellations, None)
    }
    fn publish_inner(
        &mut self,
        expected: Option<u64>,
        revision: u64,
        payload: &[u8],
        cancellations: &[CancellationAudit<'_>],
        automation: Option<(&[u8], &homesense_config::rule_document::Document)>,
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
        if let Some(active_revision)=active {
            let previous:Vec<u8>=tx.query_row("SELECT payload FROM configuration_revision WHERE revision=?1",[active_revision],|r|r.get(0))?;
            if homesense_config::document_schema_version(&previous)==Some(2) && homesense_config::document_schema_version(payload)!=Some(2){return Err(Error::InvalidPayload);}
        }
        // Legacy device-only publication must not silently discard automation.
        if automation.is_none() && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM automation_revision WHERE revision=?1)",
            [expected], |r| r.get::<_, bool>(0),
        )? {
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
        if let Some((payload, document)) = automation {
            automation::insert(&tx, expected, revision, payload, document)?;
        }
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

pub mod backup;

pub mod module_history;

/// Durable local wire/dependency scopes; receipts are never replay plans.
pub mod gateway_audit;
