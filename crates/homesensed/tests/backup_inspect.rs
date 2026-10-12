#[test]
fn backup_inspection_cli_is_read_only_and_requires_explicit_simulation() {
    let root=std::env::temp_dir().join(format!("homesense-backup-cli-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
    let source=root.join("source.sqlite");let backup=root.join("snapshot.sqlite");
    let mut store=homesense_storage::Store::open(&source).unwrap();
    store.publish_automation(None,include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,homesense_core::ExecutionContext::Simulation,&[]).unwrap();
    homesense_storage::backup::create(&source,&backup,std::time::Duration::from_secs(2)).unwrap();
    let bytes=std::fs::read(&backup).unwrap();
    let output=std::process::Command::new(env!("CARGO_BIN_EXE_homesensed")).arg("inspect-backup").arg(&backup).arg("--simulation").output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let report:serde_json::Value=serde_json::from_slice(&output.stdout).unwrap();assert_eq!(report["active_revision"],"1");assert_eq!(report["integrity"],"ok");assert_eq!(report.as_object().unwrap().len(),6);
    let output=std::process::Command::new(env!("CARGO_BIN_EXE_homesensed")).arg("inspect-backup").arg(&backup).output().unwrap();assert!(!output.status.success());assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&backup).unwrap(),bytes);drop(store);std::fs::remove_dir_all(root).unwrap();
}
