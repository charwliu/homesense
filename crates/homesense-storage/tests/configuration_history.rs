use homesense_core::ExecutionContext;
use homesense_storage::Store;
#[test]
fn history_is_bounded_stable_and_exports_matching_documents_without_activation() {
    let root = std::env::temp_dir().join(format!(
        "homesense-history-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("state.sqlite");
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .configuration_history(None, 2)
            .unwrap()
            .active_revision,
        None
    );
    assert!(
        store
            .configuration_history(None, 2)
            .unwrap()
            .revisions
            .is_empty()
    );
    let device = |revision| {
        String::from_utf8(include_bytes!("../../../config/simulation.json").to_vec())
            .unwrap()
            .replace("\"revision\": 1", &format!("\"revision\": {revision}"))
    };
    let rules = |revision| {
        format!(
            "{{\"schema_version\":1,\"configuration_revision\":{revision},\"variables\":[],\"rules\":[]}}"
        )
    };
    store
        .publish_configuration(
            None,
            device(1).as_bytes(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .publish_automation(
            Some(1),
            device(3).as_bytes(),
            rules(3).as_bytes(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .publish_automation(
            Some(3),
            device(9).as_bytes(),
            rules(9).as_bytes(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let first = store.configuration_history(None, 2).unwrap();
    assert_eq!(first.active_revision, Some(9));
    assert_eq!(
        first
            .revisions
            .iter()
            .map(|r| r.revision)
            .collect::<Vec<_>>(),
        vec![9, 3]
    );
    assert_eq!(first.next_before, Some(3));
    assert_eq!(first.revisions[0].device_bytes, device(9).len() as u64);
    assert_eq!(
        first.revisions[0].automation_bytes,
        Some(rules(9).len() as u64)
    );
    // Concurrent later publication cannot duplicate or displace entries behind a cursor.
    store
        .publish_automation(
            Some(9),
            device(10).as_bytes(),
            rules(10).as_bytes(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let second = store.configuration_history(first.next_before, 2).unwrap();
    assert_eq!(second.active_revision, Some(10));
    assert_eq!(second.revisions.len(), 1);
    assert_eq!(second.revisions[0].revision, 1);
    assert_eq!(second.revisions[0].automation_bytes, None);
    assert_eq!(second.next_before, None);
    for revision in [1, 3, 9, 10] {
        let export = store
            .export_configuration(revision, ExecutionContext::Simulation)
            .unwrap()
            .unwrap();
        assert_eq!(export.devices, device(revision).into_bytes());
        assert_eq!(
            export.automation,
            if revision == 1 {
                None
            } else {
                Some(rules(revision).into_bytes())
            }
        );
    }
    assert!(
        store
            .export_configuration(2, ExecutionContext::Simulation)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .export_configuration(1, ExecutionContext::Live)
            .is_err()
    );
    for limit in [0, 101, usize::MAX] {
        assert!(store.configuration_history(None, limit).is_err());
    }
    for before in [0, u64::MAX] {
        assert!(store.configuration_history(Some(before), 1).is_err());
        assert!(
            store
                .export_configuration(before, ExecutionContext::Simulation)
                .is_err()
        );
    }
    assert_eq!(store.active().unwrap().unwrap().revision, 10);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .configuration_history(None, 100)
            .unwrap()
            .revisions
            .len(),
        4
    );
    assert_eq!(store.active().unwrap().unwrap().revision, 10);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
