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
