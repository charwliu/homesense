//! Durable at-most-once ledger. Expiry preserves complete records and fences.
//! A reserved index blocks legacy CREATE TABLE initialization before any handler.
use crate::web_receipt_policy::Policy;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    New,
    Existing(Option<String>),
    Conflict,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedClaim {
    pub claim: Claim,
    pub expired: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub fingerprint: String,
    pub response: Option<String>,
    pub created_ms: Option<u64>,
    pub expires_ms: Option<u64>,
    pub expired: bool,
    pub legacy: bool,
}
/// Canonical scope is constructed by trusted routing, never request JSON/header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    AwayOff,
    Management(crate::web_management_receipts::Operation),
    SceneActions,
    SceneCancellations,
    DeviceCommands,
    ModuleCommands,
    ModuleQueries,
    SceneRelativeActions,
    ConfigurationActivate(uuid::Uuid),
}
impl Endpoint {
    pub fn control(path: &str) -> Result<Self, String> {
        Ok(match path {
            "/api/v1/away-off" => Self::AwayOff,
            "/api/v1/scene-actions" => Self::SceneActions,
            "/api/v1/scene-cancellations" => Self::SceneCancellations,
            "/api/v1/device-commands" => Self::DeviceCommands,
            "/api/v1/module-commands" => Self::ModuleCommands,
            "/api/v1/module-queries" => Self::ModuleQueries,
            "/api/v1/scene-relative-actions" => Self::SceneRelativeActions,
            _ => return Err("unknown canonical receipt endpoint".into()),
        })
    }
    fn key(&self) -> String {
        match self {
            Self::AwayOff => "/api/v1/away-off".into(),
            Self::Management(op) => op.key(),
            Self::SceneActions => "/api/v1/scene-actions".into(),
            Self::SceneCancellations => "/api/v1/scene-cancellations".into(),
            Self::DeviceCommands => "/api/v1/device-commands".into(),
            Self::ModuleCommands => "/api/v1/module-commands".into(),
            Self::ModuleQueries => "/api/v1/module-queries".into(),
            Self::SceneRelativeActions => "/api/v1/scene-relative-actions".into(),
            Self::ConfigurationActivate(id) => format!("/api/v1/config/activations/{id}"),
        }
    }
}
type StoredScoped = (String, Option<String>, Option<i64>, Option<i64>);
pub struct Receipts {
    db: Connection,
    policy: Policy,
    clock: Arc<dyn Fn() -> Result<u64, String> + Send + Sync>,
}
fn utc_ms() -> Result<u64, String> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|_| "receipt clock overflow".into())
}
fn identity(principal: &str, key: &str, fingerprint: Option<&str>) -> Result<(), String> {
    if principal.is_empty()
        || principal.len() > 64
        || principal.chars().any(char::is_control)
        || key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || fingerprint.is_some_and(|f| f.is_empty() || f.len() > 1024)
    {
        return Err("invalid request identity".into());
    }
    Ok(())
}
fn migrate(db: &mut Connection) -> Result<(), String> {
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version == 2 || version == 3 {
        let expected:Vec<(String,String)>=db.prepare("SELECT name,type FROM sqlite_master WHERE name IN ('web_receipt','web_receipt_legacy','web_receipt_scoped','web_receipt_meta') ORDER BY name").map_err(|e|e.to_string())?.query_map([],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?.collect::<Result<_,_>>().map_err(|e|e.to_string())?;
        let kind = if version == 2 { "view" } else { "index" };
        if expected
            != vec![
                ("web_receipt".into(), kind.into()),
                ("web_receipt_legacy".into(), "table".into()),
                ("web_receipt_meta".into(), "table".into()),
                ("web_receipt_scoped".into(), "table".into()),
            ]
        {
            return Err("receipt format incomplete".into());
        }
        if version == 2 {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            tx.execute_batch("DROP VIEW web_receipt;CREATE INDEX web_receipt ON web_receipt_scoped(principal,endpoint,request_key);PRAGMA user_version=3;").map_err(|e|e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    if version != 0 {
        return Err("unsupported receipt format; older ledger cannot resume writes".into());
    }
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let occupied:i64=tx.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('web_receipt_legacy','web_receipt_scoped','web_receipt_meta')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if occupied != 0 {
        return Err("ambiguous receipt migration".into());
    }
    let old: Option<String> = tx
        .query_row(
            "SELECT type FROM sqlite_master WHERE name='web_receipt'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(kind) = old {
        let cols: Vec<String> = tx
            .prepare("PRAGMA table_info(web_receipt)")
            .map_err(|e| e.to_string())?
            .query_map([], |r| r.get(1))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if kind != "table" || cols != ["principal", "request_key", "fingerprint", "response"] {
            return Err("unsupported legacy receipt schema".into());
        }
        tx.execute_batch("ALTER TABLE web_receipt RENAME TO web_receipt_legacy;")
            .map_err(|e| e.to_string())?;
    } else {
        tx.execute_batch("CREATE TABLE web_receipt_legacy(principal TEXT NOT NULL,request_key TEXT NOT NULL,fingerprint TEXT NOT NULL,response TEXT,PRIMARY KEY(principal,request_key));").map_err(|e|e.to_string())?;
    }
    tx.execute_batch("CREATE TABLE web_receipt_scoped(principal TEXT NOT NULL,endpoint TEXT NOT NULL,request_key TEXT NOT NULL,fingerprint TEXT NOT NULL,response TEXT,created_ms INTEGER NOT NULL,expires_ms INTEGER,expired_ms INTEGER,PRIMARY KEY(principal,endpoint,request_key));CREATE TABLE web_receipt_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),last_seen_ms INTEGER);INSERT INTO web_receipt_meta VALUES(1,NULL);CREATE INDEX web_receipt ON web_receipt_scoped(principal,endpoint,request_key);PRAGMA user_version=3;").map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
impl Receipts {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with_policy(path, Policy::default())
    }
    pub fn open_configured(path: &Path) -> Result<Self, String> {
        Self::open_with_policy(
            path,
            Policy::load(path.parent().ok_or("receipt parent missing")?)?,
        )
    }
    pub fn open_with_policy(path: &Path, policy: Policy) -> Result<Self, String> {
        Self::open_with_clock(path, policy, Arc::new(utc_ms))
    }
    /// Trusted test/server clock injection; never sourced from a Web request.
    pub fn open_with_clock(
        path: &Path,
        policy: Policy,
        clock: Arc<dyn Fn() -> Result<u64, String> + Send + Sync>,
    ) -> Result<Self, String> {
        policy.validate()?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map_err(|e| e.to_string())?;
        let mut db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;")
            .map_err(|e| e.to_string())?;
        migrate(&mut db)?;
        Ok(Self { db, policy, clock })
    }
    pub fn record_count(&self) -> Result<u64, String> {
        self.db.query_row("SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)",[],|r|r.get::<_,i64>(0)).map(|n|n as u64).map_err(|e|e.to_string())
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn lookup_scoped(
        &self,
        principal: &str,
        endpoint: &Endpoint,
        key: &str,
    ) -> Result<Option<Record>, String> {
        identity(principal, key, None)?;
        let now = i64::try_from((self.clock)()?).map_err(|_| "receipt clock overflow")?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        tx.execute("UPDATE web_receipt_meta SET last_seen_ms=CASE WHEN last_seen_ms IS NULL OR last_seen_ms<?1 THEN ?1 ELSE last_seen_ms END WHERE singleton=1",[now]).map_err(|e|e.to_string())?;
        let high: i64 = tx
            .query_row(
                "SELECT last_seen_ms FROM web_receipt_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let legacy:Option<(String,Option<String>)>=tx.query_row("SELECT fingerprint,response FROM web_receipt_legacy WHERE principal=?1 AND request_key=?2",params![principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        if let Some((fingerprint, response)) = legacy {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(Some(Record {
                fingerprint,
                response,
                created_ms: None,
                expires_ms: None,
                expired: false,
                legacy: true,
            }));
        }
        tx.execute("UPDATE web_receipt_scoped SET expired_ms=?4 WHERE principal=?1 AND endpoint=?2 AND request_key=?3 AND expired_ms IS NULL AND expires_ms IS NOT NULL AND expires_ms<=?4",params![principal,endpoint.key(),key,high]).map_err(|e|e.to_string())?;
        let row=tx.query_row("SELECT fingerprint,response,created_ms,expires_ms,expired_ms FROM web_receipt_scoped WHERE principal=?1 AND endpoint=?2 AND request_key=?3",params![principal,endpoint.key(),key],|r|Ok(Record{fingerprint:r.get(0)?,response:r.get(1)?,created_ms:Some(r.get::<_,i64>(2)? as u64),expires_ms:r.get::<_,Option<i64>>(3)?.map(|n|n as u64),expired:r.get::<_,Option<i64>>(4)?.is_some(),legacy:false})).optional().map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(row)
    }
    pub fn claim_scoped(
        &mut self,
        principal: &str,
        endpoint: &Endpoint,
        key: &str,
        fingerprint: &str,
    ) -> Result<ScopedClaim, String> {
        identity(principal, key, Some(fingerprint))?;
        let now = (self.clock)()?;
        let now_sql = i64::try_from(now).map_err(|_| "receipt clock overflow")?;
        let endpoint = endpoint.key();
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let legacy:Option<(String,Option<String>)>=tx.query_row("SELECT fingerprint,response FROM web_receipt_legacy WHERE principal=?1 AND request_key=?2",params![principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        if let Some((old, response)) = legacy {
            return Ok(ScopedClaim {
                claim: if old == fingerprint {
                    Claim::Existing(response)
                } else {
                    Claim::Conflict
                },
                expired: false,
            });
        }
        let existing:Option<StoredScoped>=tx.query_row("SELECT fingerprint,response,expires_ms,expired_ms FROM web_receipt_scoped WHERE principal=?1 AND endpoint=?2 AND request_key=?3",params![principal,endpoint,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
        if let Some((old, response, expires, expired_at)) = existing {
            tx.execute("UPDATE web_receipt_meta SET last_seen_ms=CASE WHEN last_seen_ms IS NULL OR last_seen_ms<?1 THEN ?1 ELSE last_seen_ms END WHERE singleton=1",[now_sql]).map_err(|e|e.to_string())?;
            let high: i64 = tx
                .query_row(
                    "SELECT last_seen_ms FROM web_receipt_meta WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let expired = expired_at.is_some() || expires.is_some_and(|n| high >= n);
            if expired && expired_at.is_none() {
                tx.execute("UPDATE web_receipt_scoped SET expired_ms=?4 WHERE principal=?1 AND endpoint=?2 AND request_key=?3",params![principal,endpoint,key,high]).map_err(|e|e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(ScopedClaim {
                claim: if old == fingerprint {
                    Claim::Existing(response)
                } else {
                    Claim::Conflict
                },
                expired,
            });
        }
        let previous: Option<i64> = tx
            .query_row(
                "SELECT last_seen_ms FROM web_receipt_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if previous.is_some_and(|n| now_sql < n) {
            return Err("receipt clock moved backwards; new admission refused".into());
        }
        let count: i64 = tx
            .query_row("SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count >= i64::from(self.policy.max_records) {
            return Err("receipt capacity reached; expired fences remain retained".into());
        }
        let expires = self.policy.expiry_ms(now)?.map(|n| n as i64);
        tx.execute("INSERT INTO web_receipt_scoped(principal,endpoint,request_key,fingerprint,created_ms,expires_ms)VALUES(?1,?2,?3,?4,?5,?6)",params![principal,endpoint,key,fingerprint,now_sql,expires]).map_err(|e|e.to_string())?;
        tx.execute(
            "UPDATE web_receipt_meta SET last_seen_ms=?1 WHERE singleton=1",
            [now_sql],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(ScopedClaim {
            claim: Claim::New,
            expired: false,
        })
    }
    pub fn finish_scoped(
        &mut self,
        principal: &str,
        endpoint: &Endpoint,
        key: &str,
        response: &str,
    ) -> Result<(), String> {
        identity(principal, key, None)?;
        if response.len()
            > if matches!(endpoint, Endpoint::Management(_)) {
                68 * 1024
            } else {
                8192
            }
        {
            return Err("receipt too large".into());
        }
        let n=self.db.execute("UPDATE web_receipt_scoped SET response=?4 WHERE principal=?1 AND endpoint=?2 AND request_key=?3 AND response IS NULL",params![principal,endpoint.key(),key,response]).map_err(|e|e.to_string())?;
        if n != 1 {
            return Err("receipt already completed, legacy or missing".into());
        }
        Ok(())
    }
    /// Compatibility API for existing callers. Such records remain global shadows
    /// with unknown endpoint/time; actual HTTP handlers exclusively use scoped APIs.
    pub fn claim(
        &mut self,
        principal: &str,
        key: &str,
        fingerprint: &str,
    ) -> Result<Claim, String> {
        identity(principal, key, Some(fingerprint))?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let rows = {
            let mut stmt=tx.prepare("SELECT fingerprint,response FROM web_receipt_legacy WHERE principal=?1 AND request_key=?2 UNION ALL SELECT fingerprint,response FROM web_receipt_scoped WHERE principal=?1 AND request_key=?2 LIMIT 2").map_err(|e|e.to_string())?;
            stmt.query_map(params![principal, key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        };
        if rows.len() > 1 {
            return Err("ambiguous unscoped receipt claim".into());
        }
        let old = rows.into_iter().next();
        if let Some((old, response)) = old {
            return Ok(if old == fingerprint {
                Claim::Existing(response)
            } else {
                Claim::Conflict
            });
        }
        let count: i64 = tx
            .query_row("SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count >= i64::from(self.policy.max_records) {
            return Err("receipt capacity reached".into());
        }
        tx.execute(
            "INSERT INTO web_receipt_legacy(principal,request_key,fingerprint)VALUES(?1,?2,?3)",
            params![principal, key, fingerprint],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Claim::New)
    }
    pub fn lookup(
        &self,
        principal: &str,
        key: &str,
    ) -> Result<Option<(String, Option<String>)>, String> {
        identity(principal, key, None)?;
        let mut stmt=self.db.prepare("SELECT fingerprint,response FROM web_receipt_legacy WHERE principal=?1 AND request_key=?2 UNION ALL SELECT fingerprint,response FROM web_receipt_scoped WHERE principal=?1 AND request_key=?2").map_err(|e|e.to_string())?;
        let rows: Vec<_> = stmt
            .query_map(params![principal, key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if rows.len() > 1 {
            return Err("ambiguous unscoped receipt lookup".into());
        }
        Ok(rows.into_iter().next())
    }
    pub fn finish(&mut self, principal: &str, key: &str, response: &str) -> Result<(), String> {
        identity(principal, key, None)?;
        if response.len() > 8192 {
            return Err("receipt too large".into());
        }
        let legacy:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM web_receipt_legacy WHERE principal=?1 AND request_key=?2)",params![principal,key],|r|r.get(0)).map_err(|e|e.to_string())?;
        if legacy {
            let n=self.db.execute("UPDATE web_receipt_legacy SET response=?3 WHERE principal=?1 AND request_key=?2 AND response IS NULL",params![principal,key,response]).map_err(|e|e.to_string())?;
            if n == 1 {
                return Ok(());
            }
        }
        Err("unscoped finish cannot modify scoped receipt".into())
    }
}
