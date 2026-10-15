//! Durable at-most-once Web request fence. Reserved/unknown outcomes are never
//! replayed, including after restart; they require inspection of runtime history.
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    New,
    Existing(Option<String>),
    Conflict,
}
pub struct Receipts {
    db: Connection,
}
impl Receipts {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut options=std::fs::OpenOptions::new();options.write(true).create(true).truncate(false);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        options.open(path).map_err(|e|e.to_string())?;
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS web_receipt (principal TEXT NOT NULL, request_key TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT, PRIMARY KEY(principal,request_key));").map_err(|e|e.to_string())?;
        Ok(Self { db })
    }
    pub fn claim(
        &mut self,
        principal: &str,
        key: &str,
        fingerprint: &str,
    ) -> Result<Claim, String> {
        if principal.len() > 64
            || key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || fingerprint.len() > 1024
        {
            return Err("invalid request identity".into());
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let existing:Option<(String,Option<String>)>=tx.query_row("SELECT fingerprint,response FROM web_receipt WHERE principal=?1 AND request_key=?2",params![principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        if let Some((old, response)) = existing {
            return Ok(if old == fingerprint {
                Claim::Existing(response)
            } else {
                Claim::Conflict
            });
        }
        let count: i64 = tx
            .query_row("SELECT count(*) FROM web_receipt", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count >= 100_000 {
            return Err("receipt capacity reached".into());
        }
        tx.execute(
            "INSERT INTO web_receipt(principal,request_key,fingerprint) VALUES (?1,?2,?3)",
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
        self.db.query_row("SELECT fingerprint,response FROM web_receipt WHERE principal=?1 AND request_key=?2",params![principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())
    }
    pub fn finish(&mut self, principal: &str, key: &str, response: &str) -> Result<(), String> {
        if response.len() > 8192 {
            return Err("receipt too large".into());
        }
        let changed=self.db.execute("UPDATE web_receipt SET response=?3 WHERE principal=?1 AND request_key=?2 AND response IS NULL",params![principal,key,response]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("receipt already completed or missing".into());
        }
        Ok(())
    }
}
