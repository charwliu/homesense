//! SQLite online backup, including committed WAL pages. Never copy live DB files.
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
/// Create a new private standalone snapshot. Existing destinations are never
/// overwritten. The caller must run this blocking operation off async executors.
pub fn create(source: &Path, destination: &Path, timeout: Duration) -> Result<(), String> {
    if timeout.is_zero() || timeout > Duration::from_secs(30) {
        return Err("backup timeout must be in (0,30s]".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(destination).map_err(|e| e.to_string())?;
    let result = (|| {
        let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        let mut target = Connection::open(destination).map_err(|e| e.to_string())?;
        source
            .busy_timeout(Duration::ZERO)
            .map_err(|e| e.to_string())?;
        target
            .busy_timeout(Duration::ZERO)
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        {
            let backup = Backup::new(&source, &mut target).map_err(|e| e.to_string())?;
            loop {
                if Instant::now() >= deadline {
                    return Err("backup deadline exceeded".into());
                }
                match backup.step(64).map_err(|e| e.to_string())? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    _ => return Err("unsupported backup outcome".into()),
                }
            }
        }
        target
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|e| e.to_string())?;
        let integrity: String = target
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if integrity != "ok" {
            return Err("backup integrity check failed".into());
        }
        target.close().map_err(|(_, e)| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if let Some(parent) = destination.parent() {
            std::fs::File::open(parent)
                .and_then(|dir| dir.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

/// Read-only verification of a standalone HomeSense backup. No migration,
/// activation, key material, device names or executable plans are returned.
#[derive(Debug,PartialEq,Eq)]
pub struct Inspection {
    pub bytes:u64,
    pub schema:i64,
    pub active_revision:Option<u64>,
    pub configuration_versions:u64,
    pub automation_versions:u64,
}
pub fn inspect(path:&Path,context:homesense_core::ExecutionContext)->Result<Inspection,String> {
    use std::io::Read;
    let metadata=std::fs::symlink_metadata(path).map_err(|e|e.to_string())?;
    if !metadata.is_file() || metadata.len()>1024*1024*1024 {return Err("expected a regular standalone backup up to 1 GiB".into());}
    let mut header=[0u8;20];std::fs::File::open(path).and_then(|mut f|f.read_exact(&mut header)).map_err(|e|e.to_string())?;
    if &header[..16]!=b"SQLite format 3\0" || header[18]!=1 || header[19]!=1 {return Err("backup must be a standalone rollback-journal SQLite file".into());}
    for suffix in ["-wal","-shm","-journal"] {
        let mut sidecar=path.as_os_str().to_owned();sidecar.push(suffix);
        match std::fs::symlink_metadata(Path::new(&sidecar)) {Ok(_)=>return Err("backup has a sidecar; use an online snapshot".into()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.to_string())}
    }
    let connection=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e|e.to_string())?;
    connection.busy_timeout(Duration::ZERO).map_err(|e|e.to_string())?;
    let app:i64=connection.pragma_query_value(None,"application_id",|r|r.get(0)).map_err(|e|e.to_string())?;
    let schema:i64=connection.pragma_query_value(None,"user_version",|r|r.get(0)).map_err(|e|e.to_string())?;
    if app!=crate::APPLICATION_ID || schema!=18 {return Err("foreign or unsupported backup schema".into());}
    let tx=connection.unchecked_transaction().map_err(|e|e.to_string())?;
    let integrity:String=tx.query_row("PRAGMA quick_check",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if integrity!="ok" {return Err("backup integrity check failed".into());}
    let foreign_keys:i64=tx.query_row("SELECT count(*) FROM pragma_foreign_key_check",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if foreign_keys!=0{return Err("backup foreign key check failed".into());}
    let versions:i64=tx.query_row("SELECT count(*) FROM configuration_revision",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    let automations:i64=tx.query_row("SELECT count(*) FROM automation_revision",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e|e.to_string())?;
    let store=crate::Store{connection};
    let active=store.active_bundle(context).map_err(|e|e.to_string())?;
    Ok(Inspection{bytes:metadata.len(),schema,active_revision:active.map(|b|b.configuration.revision()),configuration_versions:versions as u64,automation_versions:automations as u64})
}
