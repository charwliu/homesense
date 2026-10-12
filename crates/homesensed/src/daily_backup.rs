//! Daily online snapshots. No activation, replay, or gateway access.
use homesense_core::ExecutionContext;
use std::{path::{Path,PathBuf},time::Duration};
#[derive(Debug,Clone,PartialEq,Eq)]
pub enum Status {Idle,Complete{utc_day:u64,created:bool},Failed}
/// One UTC-day snapshot; an existing snapshot must pass inspection before reuse.
pub fn once(source:&Path,context:ExecutionContext,utc_day:u64)->Result<bool,String> {
    let directory=source.parent().ok_or("database parent missing")?.join("daily-backups");
    let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
    #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
    builder.create(&directory).map_err(|e|e.to_string())?;
    if !std::fs::symlink_metadata(&directory).map_err(|e|e.to_string())?.is_dir(){return Err("daily backup directory must not be a symlink".into());}
    // Multiple HomeSense databases can share a state directory; never reuse
    // another database's daily file merely because the UTC day matches.
    let directory=directory.join(source.file_name().ok_or("database filename missing")?);
    builder.create(&directory).map_err(|e|e.to_string())?;
    if !std::fs::symlink_metadata(&directory).map_err(|e|e.to_string())?.is_dir(){return Err("daily source directory must not be a symlink".into());}
    let destination=directory.join(format!("daily-{utc_day}.sqlite"));
    let created=match std::fs::symlink_metadata(&destination) {
        Ok(_)=>{homesense_storage::backup::inspect(&destination,context)?;Ok(false)},
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{
            homesense_storage::backup::create(source,&destination,Duration::from_secs(10))?;
            homesense_storage::backup::inspect(&destination,context)?;Ok(true)
        },
        Err(e)=>Err(e.to_string()),
    }?;
    prune(&directory,context,utc_day)?;
    Ok(created)
}
/// Retain the current UTC day and preceding 29 days. Only canonical,
/// independently verified snapshots are eligible; unknown files remain intact.
fn prune(directory:&Path,context:ExecutionContext,day:u64)->Result<(),String> {
    let entries=homesense_storage::backup::catalog(directory)?;
    for entry in entries {
        let Some(raw)=entry.name.strip_prefix("daily-").and_then(|v|v.strip_suffix(".sqlite")) else {continue;};
        let Ok(saved)=raw.parse::<u64>() else {continue;};
        if !entry.regular_file || saved.to_string()!=raw || day.saturating_sub(saved)<30 || saved>day {continue;}
        let path=directory.join(&entry.name);
        // Never delete corrupt, foreign-schema or sidecar-dependent files.
        if homesense_storage::backup::inspect(&path,context).is_ok() {
            std::fs::remove_file(path).map_err(|e|e.to_string())?;
        }
    }
    Ok(())
}
/// Start immediately, then check once per minute. Successful days never repeat
/// within a process. Failures retry next minute; blocking SQLite work is awaited
/// before stop completes, so no abandoned background writer is left running.
pub async fn run(source:PathBuf,context:ExecutionContext,mut stop:tokio::sync::watch::Receiver<bool>,status:tokio::sync::watch::Sender<Status>) {
    let mut timer=tokio::time::interval(Duration::from_secs(60));timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut completed=None;
    loop {
        if *stop.borrow(){return;}
        tokio::select! {
            changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){return;}},
            _=timer.tick()=>{
                let day=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|v|v.as_secs()/86400);
                let Some(day)=day else {status.send_replace(Status::Failed);continue;};
                if completed.is_some_and(|old|day<=old){continue;}
                let source=source.clone();
                match tokio::task::spawn_blocking(move||once(&source,context,day)).await {
                    Ok(Ok(created))=>{completed=Some(day);status.send_replace(Status::Complete{utc_day:day,created});},
                    _=>{status.send_replace(Status::Failed);},
                }
            }
        }
    }
}

/// Own both futures in the service scope; normal and error exits stop and join
/// maintenance. Backup failure is reported without turning off device control.
pub async fn supervise<F>(source:PathBuf,context:ExecutionContext,service:F)->Result<(),String>
where F:std::future::Future<Output=Result<(),String>> {
    let (stop,receive)=tokio::sync::watch::channel(false);
    let (status,mut reports)=tokio::sync::watch::channel(Status::Idle);
    let maintenance=run(source,context,receive,status);
    let mut service=Box::pin(service);
    tokio::pin!(maintenance);
    loop {
        tokio::select! {
            result=&mut service=>{let _=stop.send(true);maintenance.await;return result;},
            _=&mut maintenance=>return Err("daily backup worker stopped unexpectedly".into()),
            changed=reports.changed()=>{
                if changed.is_ok() && matches!(*reports.borrow_and_update(),Status::Failed){eprintln!("HomeSense daily backup failed; device service continues; retry in one minute");}
            }
        }
    }
}
