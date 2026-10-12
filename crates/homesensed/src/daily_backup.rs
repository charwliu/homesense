//! Daily online snapshots. No activation, replay, or gateway access.
use homesense_core::ExecutionContext;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Complete { utc_day: u64, created: bool },
    Failed,
}
/// Public diagnostic categories contain no paths, OS error text or source data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Clock,
    Directory,
    SnapshotCreation,
    SnapshotVerification,
    Retention,
    Worker,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unavailable,
    Waiting,
    Checking,
    Complete,
    Failed,
    Stopped,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Report {
    pub phase: Phase,
    pub attempted_utc_day: Option<u64>,
    pub last_success_utc_day: Option<u64>,
    pub created: Option<bool>,
    pub failure: Option<Failure>,
}
/// One supervisor owns one monitor. Reading it never opens storage or probes a
/// gateway. A report is the last maintenance outcome, not service liveness.
#[derive(Clone, Default)]
pub struct Monitor(Arc<RwLock<Report>>);
impl Monitor {
    pub fn report(&self) -> Report {
        self.0.read().map(|r| r.clone()).unwrap_or_default()
    }
    fn update(&self, apply: impl FnOnce(&mut Report)) {
        if let Ok(mut report) = self.0.write() {
            apply(&mut report);
        }
    }
}
type AttemptError = (Failure, String);
fn categorized(category: Failure) -> impl FnOnce(String) -> AttemptError {
    move |error| (category, error)
}
/// One UTC-day snapshot; an existing snapshot must pass inspection before reuse.
pub fn once(source: &Path, context: ExecutionContext, utc_day: u64) -> Result<bool, String> {
    attempt(source, context, utc_day).map_err(|(_, error)| error)
}
fn attempt(source: &Path, context: ExecutionContext, utc_day: u64) -> Result<bool, AttemptError> {
    let prepare = || -> Result<PathBuf, String> {
        let directory = source
            .parent()
            .ok_or("database parent missing")?
            .join("daily-backups");
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory).map_err(|e| e.to_string())?;
        if !std::fs::symlink_metadata(&directory)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("daily backup directory must not be a symlink".into());
        }
        let directory = directory.join(source.file_name().ok_or("database filename missing")?);
        builder.create(&directory).map_err(|e| e.to_string())?;
        if !std::fs::symlink_metadata(&directory)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("daily source directory must not be a symlink".into());
        }
        Ok(directory)
    };
    // Multiple databases share a state directory, but never a daily file.
    let directory = prepare().map_err(categorized(Failure::Directory))?;
    let destination = directory.join(format!("daily-{utc_day}.sqlite"));
    let created = match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            homesense_storage::backup::inspect(&destination, context)
                .map_err(categorized(Failure::SnapshotVerification))?;
            false
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            homesense_storage::backup::create(source, &destination, Duration::from_secs(10))
                .map_err(categorized(Failure::SnapshotCreation))?;
            homesense_storage::backup::inspect(&destination, context)
                .map_err(categorized(Failure::SnapshotVerification))?;
            true
        }
        Err(e) => return Err((Failure::SnapshotVerification, e.to_string())),
    };
    prune(&directory, context, utc_day).map_err(categorized(Failure::Retention))?;
    Ok(created)
}
/// Retain the current UTC day and preceding 29 days. Only canonical,
/// independently verified snapshots are eligible; unknown files remain intact.
fn prune(directory: &Path, context: ExecutionContext, day: u64) -> Result<(), String> {
    let entries = homesense_storage::backup::catalog(directory)?;
    for entry in entries {
        let Some(raw) = entry
            .name
            .strip_prefix("daily-")
            .and_then(|v| v.strip_suffix(".sqlite"))
        else {
            continue;
        };
        let Ok(saved) = raw.parse::<u64>() else {
            continue;
        };
        if !entry.regular_file
            || saved.to_string() != raw
            || day.saturating_sub(saved) < 30
            || saved > day
        {
            continue;
        }
        let path = directory.join(&entry.name);
        // Never delete corrupt, foreign-schema or sidecar-dependent files.
        if homesense_storage::backup::inspect(&path, context).is_ok() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
/// Start immediately, then check once per minute. Successful days never repeat
/// within a process. Failures retry next minute; blocking SQLite work is awaited
/// before stop completes, so no abandoned background writer is left running.
pub async fn run(
    source: PathBuf,
    context: ExecutionContext,
    stop: tokio::sync::watch::Receiver<bool>,
    status: tokio::sync::watch::Sender<Status>,
) {
    run_monitored(source, context, stop, status, Monitor::default()).await;
}
pub async fn run_monitored(
    source: PathBuf,
    context: ExecutionContext,
    mut stop: tokio::sync::watch::Receiver<bool>,
    status: tokio::sync::watch::Sender<Status>,
    monitor: Monitor,
) {
    monitor.update(|report| report.phase = Phase::Waiting);
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut completed = None;
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){break;}},
            _=timer.tick()=>{
                let day=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|v|v.as_secs()/86400);
                let Some(day)=day else {
                    monitor.update(|report|{report.phase=Phase::Failed;report.attempted_utc_day=None;report.created=None;report.failure=Some(Failure::Clock);});
                    status.send_replace(Status::Failed);continue;
                };
                if completed.is_some_and(|old|day<=old){continue;}
                monitor.update(|report|{report.phase=Phase::Checking;report.attempted_utc_day=Some(day);report.created=None;report.failure=None;});
                let source=source.clone();
                match tokio::task::spawn_blocking(move||attempt(&source,context,day)).await {
                    Ok(Ok(created))=>{
                        completed=Some(day);
                        monitor.update(|report|{report.phase=Phase::Complete;report.last_success_utc_day=Some(day);report.created=Some(created);});
                        status.send_replace(Status::Complete{utc_day:day,created});
                    },
                    result=>{
                        let failure=match result {Ok(Err((failure,_)))=>failure,_=>Failure::Worker};
                        monitor.update(|report|{report.phase=Phase::Failed;report.failure=Some(failure);});
                        status.send_replace(Status::Failed);
                    },
                }
            }
        }
    }
    monitor.update(|report| report.phase = Phase::Stopped);
}

/// Own both futures in the service scope; normal and error exits stop and join
/// maintenance. Backup failure is reported without turning off device control.
pub async fn supervise<F>(
    source: PathBuf,
    context: ExecutionContext,
    service: F,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
{
    supervise_monitored(source, context, service, Monitor::default()).await
}
pub async fn supervise_monitored<F>(
    source: PathBuf,
    context: ExecutionContext,
    service: F,
    monitor: Monitor,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
{
    let (stop, receive) = tokio::sync::watch::channel(false);
    let (status, mut reports) = tokio::sync::watch::channel(Status::Idle);
    let maintenance = run_monitored(source, context, receive, status, monitor.clone());
    let mut service = Box::pin(service);
    tokio::pin!(maintenance);
    loop {
        tokio::select! {
            result=&mut service=>{let _=stop.send(true);maintenance.await;return result;},
            _=&mut maintenance=>return Err("daily backup worker stopped unexpectedly".into()),
            changed=reports.changed()=>{
                if changed.is_ok() && matches!(*reports.borrow_and_update(),Status::Failed){
                    eprintln!("HomeSense daily backup failed ({:?}); device service continues; retry in one minute",monitor.report().failure);
                }
            }
        }
    }
}
