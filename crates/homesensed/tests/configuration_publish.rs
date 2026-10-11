use homesensed::configuration_publish::publish;
#[test]
fn publication_requires_stopped_runtime_and_expected_revision_preserving_identity() {
    let directory =
        std::env::temp_dir().join(format!("homesense-publish-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(directory.join("identity")).unwrap();
    std::fs::write(directory.join("identity/sentinel"), b"identity unchanged").unwrap();
    let path = directory.join("runtime.sqlite");
    let mut store = homesense_storage::Store::open(&path).unwrap();
    let document = include_bytes!("../../../config/live/study-switch.json");
    store
        .publish_configuration(None, document, homesense_core::ExecutionContext::Live, &[])
        .unwrap();
    let mut value: hap::serde_json::Value = hap::serde_json::from_slice(document).unwrap();
    value["revision"] = 2.into();
    let next = hap::serde_json::to_vec(&value).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("runtime.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(publish(&next, &directory, 1).is_err());
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    lock.unlock().unwrap();
    drop(lock);
    assert!(publish(&next, &directory, 9).is_err());
    assert_eq!(publish(&next, &directory, 1).unwrap(), 2);
    assert_eq!(store.active().unwrap().unwrap().payload, next);
    assert!(publish(document, &directory, 2).is_err());
    assert_eq!(
        std::fs::read(directory.join("identity/sentinel")).unwrap(),
        b"identity unchanged"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn automation_cli_publishes_both_documents_only_after_runtime_stops() {
    use homesense_core::ExecutionContext::Live;
    let directory =
        std::env::temp_dir().join(format!("homesense-bundle-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(directory.join("identity")).unwrap();
    std::fs::write(directory.join("identity/sentinel"), b"keep pairing").unwrap();
    let path = directory.join("runtime.sqlite");
    let mut store = homesense_storage::Store::open(&path).unwrap();
    let initial = include_bytes!("../../../config/live/study-switch.json");
    store
        .publish_configuration(None, initial, Live, &[])
        .unwrap();
    let mut next: hap::serde_json::Value = hap::serde_json::from_slice(initial).unwrap();
    next["revision"] = 2.into();
    let device_path = directory.join("devices.json");
    let rules_path = directory.join("rules.json");
    std::fs::write(&device_path, hap::serde_json::to_vec(&next).unwrap()).unwrap();
    let rules = br#"{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}"#;
    std::fs::write(&rules_path, rules).unwrap();
    let run = |expected: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_homesensed"))
            .arg("publish-automation")
            .arg(&device_path)
            .arg(&rules_path)
            .arg(&directory)
            .arg(expected)
            .output()
            .unwrap()
    };
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("runtime.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(!run("1").status.success());
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    lock.unlock().unwrap();
    drop(lock);
    assert!(!run("9").status.success());
    std::fs::write(&rules_path, b"{}").unwrap();
    assert!(!run("1").status.success());
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    std::fs::write(&rules_path, rules).unwrap();
    let output = run("1");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(store);
    let store = homesense_storage::Store::open(&path).unwrap();
    let bundle = store.active_bundle(Live).unwrap().unwrap();
    assert_eq!(bundle.configuration.revision(), 2);
    assert_eq!(bundle.automation.unwrap().revision, 2);
    assert_eq!(
        std::fs::read(directory.join("identity/sentinel")).unwrap(),
        b"keep pairing"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}
