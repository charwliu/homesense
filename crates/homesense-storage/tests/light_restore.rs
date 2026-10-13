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

#[test]
fn recorded_settings_include_off_and_ignore_unwritten_targets_across_boots() {
    use homesense_core::{
        Command, LevelTransition,
        queue::{Epochs, Origin, QueuedCommand},
    };
    use homesense_storage::journal::Status;
    let path = std::env::temp_dir().join(format!(
        "recorded-level-{}.sqlite",
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
    let command = |n, value| QueuedCommand {
        id: n,
        device_id: id.clone(),
        command: Command::SetLevel {
            level: Percent::new(value).unwrap(),
            transition: LevelTransition::Ramp,
        },
        origin: Origin::Hap,
        epochs: Epochs {
            configuration: 1,
            transport: 1,
            control: 1,
        },
        deadline_ms: 1000,
        run_id: None,
    };
    store
        .accept_commands("boot1", &[command(1, 50), command(2, 30)], 1)
        .unwrap();
    assert!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap()
            .is_empty()
    );
    store
        .transition_command("boot1", 1, Status::Accepted, Status::Writing, 2)
        .unwrap();
    store
        .transition_command("boot1", 1, Status::Writing, Status::Written, 3)
        .unwrap();
    store
        .transition_command("boot1", 2, Status::Accepted, Status::Cancelled, 4)
        .unwrap();
    assert_eq!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap()[&id]
            .get(),
        50
    );
    // Schema 20 migration captures only writes whose completion is known.
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("DROP TABLE light_setting; ALTER TABLE rule_command DROP COLUMN source; PRAGMA user_version=20;")
        .unwrap();
    drop(sql);
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap()[&id]
            .get(),
        50
    );

    store.accept_commands("boot2", &[command(1, 0)], 0).unwrap();
    store
        .transition_command("boot2", 1, Status::Accepted, Status::Writing, 1)
        .unwrap();
    // Settings and Written audit commit together or neither commits.
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_setting BEFORE UPDATE ON light_setting BEGIN SELECT RAISE(ABORT,'synthetic setting failure'); END;").unwrap();
    assert!(
        store
            .transition_command("boot2", 1, Status::Writing, Status::Written, 2)
            .is_err()
    );
    assert_eq!(
        store.command_status("boot2", 1).unwrap(),
        Some(Status::Writing)
    );
    assert_eq!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap()[&id]
            .get(),
        50
    );
    sql.execute_batch("DROP TRIGGER reject_setting;").unwrap();
    drop(sql);
    store
        .transition_command("boot2", 1, Status::Writing, Status::Written, 2)
        .unwrap();
    let sql=rusqlite::Connection::open(&path).unwrap();sql.execute_batch("DROP TABLE light_setting; ALTER TABLE rule_command DROP COLUMN source; PRAGMA user_version=20;").unwrap();drop(sql);
    drop(store);
    let mut store = Store::open(&path).unwrap();
    store.recover_commands("boot3").unwrap();

    assert_eq!(
        store
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap()[&id]
            .get(),
        0
    );
    assert!(
        store
            .recorded_dimmer_levels(2, std::slice::from_ref(&id))
            .unwrap()
            .is_empty()
    );
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
