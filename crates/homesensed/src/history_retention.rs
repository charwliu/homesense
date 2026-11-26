//! Periodic bounded history archival. Separate from 30-day rotating backups:
//! archival files are never automatically expired or used as replay plans.
use std::{path::PathBuf, time::Duration};

pub async fn run(source: PathBuf, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){break;}},
            _=timer.tick()=>{
                let source=source.clone();
                let outcome=tokio::task::spawn_blocking(move||{
                    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|"clock invalid".to_string())?;
                    let now=i64::try_from(now.as_millis()).map_err(|_|"clock overflow".to_string())?;
                    let directory=source.parent().ok_or("database parent missing")?.join("history-archives").join(source.file_name().ok_or("database filename missing")?);
                    homesense_storage::history_retention::maintain(&source,&directory,now)
                }).await;
                match outcome {
                    Ok(Ok(report)) if report.archived>0 || report.clock_reset=>log::info!("history_retention scanned={} archived={} deleted={} clock_reset={}",report.scanned,report.archived,report.deleted,report.clock_reset),
                    Ok(Ok(_))=>{},
                    // Paths, row contents and credentials stay out of diagnostics.
                    _=>log::warn!("history_retention deferred; no unverified history deletion; retry in one minute"),
                }
            }
        }
    }
}
/// Stop and join the blocking pass before shutdown returns. Failure of this
/// maintenance task never disables control or bypasses a database constraint.
pub async fn supervise<F>(source: PathBuf, service: F) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
{
    let (stop, receive) = tokio::sync::watch::channel(false);
    let maintenance = run(source, receive);
    tokio::pin!(maintenance);
    tokio::pin!(service);
    tokio::select! {
        result=&mut service=>{let _=stop.send(true);maintenance.await;result},
        _=&mut maintenance=>Err("history retention worker stopped unexpectedly".into()),
    }
}
