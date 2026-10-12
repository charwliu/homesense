use homesense_core::ExecutionContext;
use homesensed::daily_backup::{self,Status};
#[tokio::test]
async fn daily_snapshot_is_unique_verified_and_worker_stops_without_replay() {
    let root=std::env::temp_dir().join(format!("daily-backup-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let source=root.join("source.sqlite");
    let mut store=homesense_storage::Store::open(&source).unwrap();store.publish_automation(None,include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,ExecutionContext::Simulation,&[]).unwrap();
    assert!(daily_backup::once(&source,ExecutionContext::Simulation,10).unwrap());
    let before=std::fs::read(root.join("daily-backups/source.sqlite/daily-10.sqlite")).unwrap();
    assert!(!daily_backup::once(&source,ExecutionContext::Simulation,10).unwrap());assert_eq!(std::fs::read(root.join("daily-backups/source.sqlite/daily-10.sqlite")).unwrap(),before);
    assert!(daily_backup::once(&source,ExecutionContext::Simulation,11).unwrap());
    std::fs::write(root.join("daily-backups/source.sqlite/daily-12.sqlite"),b"broken").unwrap();assert!(daily_backup::once(&source,ExecutionContext::Simulation,12).is_err());assert_eq!(std::fs::read(root.join("daily-backups/source.sqlite/daily-12.sqlite")).unwrap(),b"broken");
    let (stop,receive)=tokio::sync::watch::channel(false);let (status,mut reports)=tokio::sync::watch::channel(Status::Idle);
    let worker=tokio::spawn(daily_backup::run(source.clone(),ExecutionContext::Simulation,receive,status));
    tokio::time::timeout(std::time::Duration::from_secs(5),reports.wait_for(|s|matches!(s,Status::Complete{created:true,..}))).await.unwrap().unwrap();
    stop.send(true).unwrap();worker.await.unwrap();
    let count=std::fs::read_dir(root.join("daily-backups/source.sqlite")).unwrap().count();assert_eq!(count,2); // Current snapshot plus preserved corrupt file; expired valid files are pruned.
    let (stop,receive)=tokio::sync::watch::channel(true);let (status,_)=tokio::sync::watch::channel(Status::Idle);daily_backup::run(source,ExecutionContext::Simulation,receive,status).await;drop(stop);
    assert_eq!(std::fs::read_dir(root.join("daily-backups/source.sqlite")).unwrap().count(),count);
    let (stop,receive)=tokio::sync::watch::channel(false);let (status,mut reports)=tokio::sync::watch::channel(Status::Idle);
    let worker=tokio::spawn(daily_backup::run(root.join("missing.sqlite"),ExecutionContext::Simulation,receive,status));
    tokio::time::timeout(std::time::Duration::from_secs(5),reports.wait_for(|s|matches!(s,Status::Failed))).await.unwrap().unwrap();stop.send(true).unwrap();worker.await.unwrap();assert!(!root.join("missing.sqlite").exists());
    assert_eq!(store.active().unwrap().unwrap().revision,1);drop(store);std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn supervisor_preserves_service_result_even_when_backup_fails() {
    let root=std::env::temp_dir().join(format!("daily-supervisor-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
    let result=daily_backup::supervise(root.join("missing.sqlite"),ExecutionContext::Simulation,async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;Err("synthetic service failure".into())
    }).await;
    assert_eq!(result,Err("synthetic service failure".into()));assert!(!root.join("missing.sqlite").exists());std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retention_removes_only_verified_expired_daily_files() {
    let root=std::env::temp_dir().join(format!("daily-retention-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let source=root.join("source.sqlite");
    let mut store=homesense_storage::Store::open(&source).unwrap();
    store.publish_automation(None,include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,ExecutionContext::Simulation,&[]).unwrap();
    daily_backup::once(&source,ExecutionContext::Simulation,10).unwrap();
    daily_backup::once(&source,ExecutionContext::Simulation,11).unwrap();
    let directory=root.join("daily-backups/source.sqlite");
    std::fs::write(directory.join("daily-9.sqlite"),b"corrupt").unwrap();
    std::fs::copy(directory.join("daily-10.sqlite"),directory.join("daily-010.sqlite")).unwrap();
    std::fs::copy(directory.join("daily-10.sqlite"),directory.join("daily-99.sqlite")).unwrap();
    daily_backup::once(&source,ExecutionContext::Simulation,40).unwrap();
    assert!(!directory.join("daily-10.sqlite").exists());
    for name in ["daily-11.sqlite","daily-40.sqlite","daily-9.sqlite","daily-010.sqlite","daily-99.sqlite"] {assert!(directory.join(name).exists(),"{name}");}
    assert!(!daily_backup::once(&source,ExecutionContext::Simulation,40).unwrap());
    drop(store);std::fs::remove_dir_all(root).unwrap();
}
