//! Private persistent credential digests. No browser sessions or plaintext secrets.
use crate::web_auth::Grant;
use rusqlite::{Connection, params};
use std::path::Path;
use uuid::Uuid;

pub struct Record {
    pub id: Uuid,
    pub digest: [u8;32],
    pub grant: Grant,
}
pub struct Credentials(Connection);
impl Credentials {
    pub fn open(path:&Path)->Result<Self,String> {
        if !path.exists() {
            let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
            options.open(path).map_err(|e|e.to_string())?;
        }
        let metadata=std::fs::symlink_metadata(path).map_err(|e|e.to_string())?;
        if !metadata.file_type().is_file(){return Err("credential database must be a regular file".into());}
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {return Err("credential database must be private".into());}
        }
        let db=Connection::open(path).map_err(|e|e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(1)).map_err(|e|e.to_string())?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS web_credential (
            id TEXT PRIMARY KEY, digest BLOB NOT NULL UNIQUE CHECK(length(digest)=32),
            grant_json TEXT NOT NULL
          );").map_err(|e|e.to_string())?;
        let result=Self(db);result.list()?;Ok(result)
    }
    pub fn save(&mut self,record:&Record)->Result<(),String> {
        validate(&record.grant)?;
        let encoded=serde_json::to_string(&record.grant).map_err(|e|e.to_string())?;
        let tx=self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let count:i64=tx.query_row("SELECT count(*) FROM web_credential WHERE id<>?1",[record.id.to_string()],|r|r.get(0)).map_err(|e|e.to_string())?;
        if count>=128 {return Err("credential capacity reached".into());}
        tx.execute("INSERT INTO web_credential(id,digest,grant_json) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET digest=excluded.digest,grant_json=excluded.grant_json",params![record.id.to_string(),record.digest.as_slice(),encoded]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e|e.to_string())
    }
    pub fn revoke(&mut self,id:Uuid)->Result<(),String> {
        self.0.execute("DELETE FROM web_credential WHERE id=?1",[id.to_string()]).map_err(|e|e.to_string())?;Ok(())
    }
    pub fn list(&self)->Result<Vec<Record>,String> {
        let mut stmt=self.0.prepare("SELECT id,digest,grant_json FROM web_credential ORDER BY id LIMIT 129").map_err(|e|e.to_string())?;
        let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,String>(2)?))).map_err(|e|e.to_string())?;
        let mut result=Vec::new();
        for row in rows {
            let (id,digest,grant)=row.map_err(|e|e.to_string())?;
            let grant:Grant=serde_json::from_str(&grant).map_err(|_|"invalid stored grant")?;validate(&grant)?;
            result.push(Record{id:Uuid::parse_str(&id).map_err(|_|"invalid stored credential id")?,digest:digest.try_into().map_err(|_|"invalid stored digest")?,grant});
        }
        if result.len()>128 {return Err("credential capacity exceeded".into());}Ok(result)
    }
}
fn validate(grant:&Grant)->Result<(),String> {
    if grant.scenes.len()>4095 || grant.devices.len()>256 || grant.scenes.iter().chain(&grant.devices).any(|s|s.is_empty() || s.len()>256 || s.chars().any(char::is_control)) {return Err("invalid grant scope".into());}Ok(())
}
