use homesense_core::{DeviceId, ExecutionContext, Percent};
use homesense_storage::Store;
#[test]
fn restore_survives_restart_ignores_zero_and_does_not_cross_revision() {
    let path = std::env::temp_dir().join(format!(
        "homesense-restore-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = Store::open(&path).unwrap();
    store
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation.json"),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let id = DeviceId::new("simulation:light").unwrap();
    store
        .remember_light_levels(1, &[(id.clone(), Percent::new(30).unwrap())])
        .unwrap();
    store
        .remember_light_levels(1, &[(id.clone(), Percent::new(0).unwrap())])
        .unwrap();
    assert!(
        store
            .remember_light_levels(2, &[(id.clone(), Percent::new(80).unwrap())])
            .is_err()
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.light_restore_levels(1).unwrap()[&id].get(), 30);
    assert!(store.light_restore_levels(2).unwrap().is_empty());
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
