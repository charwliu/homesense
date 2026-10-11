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
