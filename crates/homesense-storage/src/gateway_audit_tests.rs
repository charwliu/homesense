use super::*;
use crate::journal::Status;
use homesense_core::{DeviceId, MotorMotion, queue::Origin};

#[path = "../tests/support/pre_gateway_scope_fixture.rs"]
mod pre_gateway_scope_fixture;

const BOOT: &str = "synthetic-gateway-audit";
fn global() -> Epochs {
    Epochs {
        transport: 7,
        configuration: 1,
        control: 3,
    }
}
fn wire(id: &str, epoch: u64) -> WireScope {
    WireScope::from_persisted(id.into(), epoch).unwrap()
}
fn database() -> Store {
    let mut store = Store::open(":memory:").unwrap();
    store
        .publish(None, 1, b"synthetic configuration", &[])
        .unwrap();
    store
}
fn command(id: u64, action: Command) -> QueuedCommand {
    QueuedCommand {
        id,
        device_id: DeviceId::new(format!("synthetic:{id}")).unwrap(),
        command: action,
        origin: Origin::Rule,
        epochs: global(),
        deadline_ms: 1000,
        run_id: None,
    }
}
fn scope(id: u64, gateway: &str) -> CommandScope {
    CommandScope {
        command: id,
        physical: wire(gateway, 1),
        requirements: vec![],
        operations: vec![],
    }
}
fn accept(store: &mut Store, commands: &[QueuedCommand], scopes: Acceptance) -> Result<(), Error> {
    store.accept_queue_batch_scoped(
        BOOT,
        commands,
        &[],
        10,
        None,
        &[],
        None,
        None,
        &[],
        &[],
        None,
        &[],
        &scopes,
    )
}
fn key() -> OperationKey {
    OperationKey {
        kind: OperationKind::Delay,
        id: "synthetic-cross-bus-run".into(),
    }
}
fn operation() -> OperationScope {
    OperationScope {
        key: key(),
        global: global(),
        wires: vec![wire("bus-a", 1), wire("bus-b", 1)],
    }
}
fn count(store: &Store, table: &str) -> i64 {
    store
        .connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn local_fault_preserves_independent_writes_and_truthful_inflight_cross_bus_effect() {
    let mut store = database();
    let stop = Command::Motor(MotorMotion::Stop);
    let commands = vec![
        command(1, Command::Switch(true)),
        command(2, Command::Switch(true)),
        command(3, Command::Switch(true)),
        command(4, Command::Switch(true)),
        command(5, stop),
        command(6, stop),
        command(7, Command::Switch(true)),
        command(8, Command::Switch(true)),
    ];
    let mut scopes = vec![
        scope(1, "bus-a"),
        scope(2, "bus-b"),
        scope(3, "bus-b"),
        scope(4, "bus-b"),
        scope(5, "bus-b"),
        scope(6, "bus-a"),
        scope(7, "bus-b"),
        scope(8, "bus-a"),
    ];
    // A decision read affects B-only output, independently of a scene link.
    scopes[2].requirements.push(wire("bus-a", 1));
    for i in [3, 4, 5, 6] {
        scopes[i].operations.push(key());
    }
    accept(
        &mut store,
        &commands,
        Acceptance {
            commands: scopes,
            operations: vec![operation()],
        },
    )
    .unwrap();
    for (id, gateway) in [(2, "bus-b"), (7, "bus-b"), (8, "bus-a")] {
        assert!(
            store
                .begin_command_write_scoped(BOOT, id, 20, &wire(gateway, 1))
                .unwrap()
        );
    }
    let fault = store
        .fail_gateway(BOOT, global(), &wire("bus-a", 1), 30)
        .unwrap();
    assert_eq!(fault.cancelled, [1, 3, 4, 6]);
    assert_eq!(fault.uncertain, [8]);
    assert_eq!(fault.operations, [key()]);
    assert_eq!(
        store.command_status(BOOT, 2).unwrap(),
        Some(Status::Writing)
    );
    assert_eq!(
        store.command_status(BOOT, 7).unwrap(),
        Some(Status::Writing)
    );
    for id in [2, 7] {
        store
            .transition_command(BOOT, id, Status::Writing, Status::Written, 40)
            .unwrap();
        assert_eq!(
            store.command_status(BOOT, id).unwrap(),
            Some(Status::Written)
        );
    }
    assert!(
        store
            .begin_command_write_scoped(BOOT, 5, 40, &wire("bus-b", 1))
            .unwrap(),
        "accepted B STOP survives parent cancellation with its own bus intact"
    );
    assert_eq!(
        store.command_status(BOOT, 6).unwrap(),
        Some(Status::Cancelled)
    );
    let new_b = command(9, Command::Switch(true));
    accept(
        &mut store,
        &[new_b],
        Acceptance {
            commands: vec![scope(9, "bus-b")],
            operations: vec![],
        },
    )
    .unwrap();
    assert!(
        store
            .begin_command_write_scoped(BOOT, 9, 40, &wire("bus-b", 1))
            .unwrap()
    );
}

#[test]
fn failed_wire_and_old_operation_are_never_recaptured_after_recovery() {
    let mut store = database();
    store
        .reserve_gateway_operations(BOOT, &[operation()], 10)
        .unwrap();
    store
        .fail_gateway(BOOT, global(), &wire("bus-a", 1), 20)
        .unwrap();
    assert!(matches!(
        accept(
            &mut store,
            &[command(1, Command::Switch(true))],
            Acceptance {
                commands: vec![scope(1, "bus-a")],
                operations: vec![]
            }
        ),
        Err(Error::Conflict)
    ));
    let mut fresh = scope(2, "bus-a");
    fresh.physical = wire("bus-a", 2);
    accept(
        &mut store,
        &[command(2, Command::Switch(true))],
        Acceptance {
            commands: vec![fresh],
            operations: vec![],
        },
    )
    .unwrap();
    assert!(
        store
            .begin_command_write_scoped(BOOT, 2, 30, &wire("bus-a", 2))
            .unwrap()
    );
    let mut old_operation = operation();
    old_operation.wires = vec![wire("bus-a", 2), wire("bus-b", 1)];
    assert!(
        matches!(
            store.reserve_gateway_operations(BOOT, &[old_operation], 30),
            Err(Error::Conflict)
        ),
        "the same consumed operation key cannot adopt new connection epochs"
    );
    assert_eq!(count(&store, "command_journal"), 1);
}

#[test]
fn synthetic_query_and_module_off_receipts_use_real_wire_scope_not_device_prefix() {
    let mut store = database();
    let mut query_a = command(1, Command::Query);
    query_a.device_id = DeviceId::new("module-query-ticket:1").unwrap();
    let mut query_b = command(2, Command::Query);
    query_b.device_id = DeviceId::new("module-query-ticket:2").unwrap();
    let mut off = command(3, Command::ModuleAllOff);
    off.device_id = DeviceId::new("module-off-ticket:3").unwrap();
    let effects = crate::module_off::Effects {
        command: 3,
        module_id: "synthetic-module".into(),
        gateway_id: "bus-a".into(),
        address: 0x11,
        profile: "r8i8_din_pro_v2_7_7".into(),
        devices: (1..=8)
            .map(|i| (i, format!("synthetic-port:{i}")))
            .collect(),
    };
    store
        .accept_queue_batch_scoped(
            BOOT,
            &[query_a, query_b, off],
            &[],
            10,
            None,
            &[],
            None,
            None,
            &[],
            &[],
            None,
            &[effects],
            &Acceptance {
                commands: vec![scope(1, "bus-a"), scope(2, "bus-b"), scope(3, "bus-a")],
                operations: vec![],
            },
        )
        .unwrap();
    let fault = store
        .fail_gateway(BOOT, global(), &wire("bus-a", 1), 20)
        .unwrap();
    assert_eq!(fault.cancelled, [1, 3]);
    assert_eq!(
        store.command_status(BOOT, 2).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        store.command_wire_scope(BOOT, 3).unwrap(),
        Some(wire("bus-a", 1))
    );
    assert!(
        store
            .begin_command_write_scoped(BOOT, 2, 30, &wire("bus-b", 1))
            .unwrap()
    );
}

#[test]
fn wrong_or_conflicting_dependency_rolls_back_all_acceptance_and_operation_rows() {
    let mut store = database();
    let mut incompatible = scope(1, "bus-a");
    incompatible.requirements.push(wire("bus-a", 2));
    let failed = accept(
        &mut store,
        &[
            command(1, Command::Switch(true)),
            command(2, Command::Switch(true)),
        ],
        Acceptance {
            commands: vec![incompatible, scope(2, "bus-b")],
            operations: vec![operation()],
        },
    );
    assert!(matches!(failed, Err(Error::Conflict)));
    for table in [
        "command_journal",
        "gateway_command_requirement",
        "gateway_operation",
        "gateway_operation_wire",
    ] {
        assert_eq!(count(&store, table), 0, "no partial committed {table}");
    }
}

#[test]
fn fault_audit_failure_rolls_back_cancellations_uncertainty_and_operation_changes() {
    let mut store = database();
    let mut a = scope(1, "bus-a");
    a.operations.push(key());
    accept(
        &mut store,
        &[
            command(1, Command::Switch(true)),
            command(2, Command::Switch(true)),
        ],
        Acceptance {
            commands: vec![a, scope(2, "bus-b")],
            operations: vec![operation()],
        },
    )
    .unwrap();
    store.connection.execute_batch("CREATE TRIGGER deny_fault BEFORE INSERT ON gateway_fault BEGIN SELECT RAISE(ABORT,'synthetic fault-audit failure'); END;").unwrap();
    assert!(matches!(
        store.fail_gateway(BOOT, global(), &wire("bus-a", 1), 20),
        Err(Error::Database(_))
    ));
    assert_eq!(
        store.command_status(BOOT, 1).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        store.command_status(BOOT, 2).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(count(&store, "gateway_fault"), 0);
    assert_eq!(
        store
            .connection
            .query_row("SELECT status FROM gateway_operation", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "active"
    );
}

#[test]
fn missing_current_boot_scope_is_fatal_and_never_guessed_from_device_identity() {
    let mut store = database();
    store
        .accept_commands(BOOT, &[command(1, Command::Switch(true))], 10)
        .unwrap();
    assert!(matches!(
        store.fail_gateway(BOOT, global(), &wire("bus-a", 1), 20),
        Err(Error::InvalidAudit)
    ));
    assert_eq!(
        store.command_status(BOOT, 1).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(count(&store, "gateway_fault"), 0);
}

#[test]
fn wrong_physical_write_scope_never_transitions_accepted_command_to_writing() {
    let mut store = database();
    accept(
        &mut store,
        &[command(1, Command::Switch(true))],
        Acceptance {
            commands: vec![scope(1, "bus-b")],
            operations: vec![],
        },
    )
    .unwrap();
    for wrong in [wire("bus-a", 1), wire("bus-b", 2)] {
        assert!(matches!(
            store.begin_command_write_scoped(BOOT, 1, 20, &wrong),
            Err(Error::Conflict)
        ));
        assert_eq!(
            store.command_status(BOOT, 1).unwrap(),
            Some(Status::Accepted)
        );
    }
}

#[test]
fn new_wire_epoch_supersedes_old_uncertainty_at_equal_timestamp_but_not_pending_work() {
    let mut store = database();
    let a = command(1, Command::Switch(true));
    accept(
        &mut store,
        std::slice::from_ref(&a),
        Acceptance {
            commands: vec![scope(1, "bus-a")],
            operations: vec![],
        },
    )
    .unwrap();
    assert!(
        store
            .begin_command_write_scoped(BOOT, 1, 20, &wire("bus-a", 1))
            .unwrap()
    );
    store
        .fail_gateway(BOOT, global(), &wire("bus-a", 1), 20)
        .unwrap();
    assert!(
        !store
            .feedback_is_settled_in_wire_scope(
                BOOT,
                &a.device_id,
                Some(20),
                global(),
                &wire("bus-a", 1)
            )
            .unwrap()
    );
    assert!(
        store
            .feedback_is_settled_in_wire_scope(
                BOOT,
                &a.device_id,
                Some(20),
                global(),
                &wire("bus-a", 2)
            )
            .unwrap(),
        "new actual wire sample supersedes old terminal receipt even in same millisecond"
    );
    let b = command(2, Command::Switch(true));
    accept(
        &mut store,
        std::slice::from_ref(&b),
        Acceptance {
            commands: vec![scope(2, "bus-b")],
            operations: vec![],
        },
    )
    .unwrap();
    assert!(
        !store
            .feedback_is_settled_in_wire_scope(
                BOOT,
                &b.device_id,
                Some(20),
                global(),
                &wire("bus-b", 2)
            )
            .unwrap(),
        "a pending receipt still requires explicit fault audit"
    );
}

#[test]
fn local_led_fence_is_bounded_with_same_device_old_wire_history_and_empty_new_epoch() {
    use crate::module_history::LATEST_LED_WIRE_REQUEST_SQL;
    use rusqlite::StatementStatus;
    let mut store = database();
    let id = DeviceId::new("synthetic-led").unwrap();
    let mut inserted = 0;
    let mut steps = vec![];
    for count in [1000, 10000, 100000] {
        let tx = store.connection.transaction().unwrap();
        {
            let mut q=tx.prepare("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status,gateway_id,gateway_epoch) VALUES(?1,?2,1,'synthetic-led','rule',x'01060100',7,3,1000,100,'written','bus-a',1)").unwrap();
            for command in inserted + 1..=count {
                q.execute(params![BOOT, command]).unwrap();
            }
        }
        tx.commit().unwrap();
        inserted = count;
        assert!(
            store
                .latest_led_request_in_wire_scope(BOOT, 1, &id, global(), &wire("bus-a", 2))
                .unwrap()
                .is_none()
        );
        let mut query = store
            .connection
            .prepare(LATEST_LED_WIRE_REQUEST_SQL)
            .unwrap();
        let found = query
            .query_row(params![BOOT, 1, id.as_str(), 7, 3, "bus-a", 2], |r| {
                r.get::<_, i64>(0)
            })
            .optional()
            .unwrap();
        assert_eq!(found, None);
        steps.push((count, query.get_status(StatementStatus::VmStep)));
    }
    assert!(
        steps.iter().all(|(_, n)| *n <= 128),
        "local empty epoch lookup cannot scan old history: {steps:?}"
    );
    let explain = store
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {LATEST_LED_WIRE_REQUEST_SQL}"))
        .unwrap()
        .query_map(params![BOOT, 1, id.as_str(), 7, 3, "bus-a", 2], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(explain.iter().any(|s| s.contains("command_led_wire_scope")));
    eprintln!("actual local LED empty-epoch VM steps at1k/10k/100k: {steps:?}; plan={explain:?}");
    store.connection.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status,gateway_id,gateway_epoch) VALUES(?1,100001,1,'synthetic-led','rule',x'010A0000',7,3,1000,100,'uncertain','bus-a',2)",[BOOT]).unwrap();
    let latest = store
        .latest_led_request_in_wire_scope(BOOT, 1, &id, global(), &wire("bus-a", 2))
        .unwrap()
        .unwrap();
    assert_eq!(latest.command, 100001);
    assert_eq!(latest.status, "uncertain");
}

#[test]
fn schema26_migration_preserves_legacy_audit_and_pre_migration_snapshot_without_guessing_scope() {
    use std::time::Duration;
    let directory = std::env::temp_dir().join(format!(
        "gateway-migration-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let active = directory.join("active.sqlite");
    let before_snapshot = directory.join("before-migration.sqlite");
    let mut store = Store::open(&active).unwrap();
    store
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation.json"),
            homesense_core::ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .accept_commands("previous-boot", &[command(1, Command::Switch(true))], 10)
        .unwrap();
    // Reconstruct the exact predecessor row layout and schema version. There
    // are no local-scope rows to discard: this fixture represents schema26.
    store
        .connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
        DROP TABLE gateway_command_operation;
        DROP TABLE gateway_command_requirement;
        DROP TABLE gateway_operation_wire;
        DROP TABLE gateway_operation;
        DROP TABLE gateway_fault;
        DROP TRIGGER immutable_command_wire;
        DROP INDEX command_gateway_pending;
        DROP INDEX command_led_wire_scope;
        ALTER TABLE command_journal DROP COLUMN gateway_epoch;
        ALTER TABLE command_journal DROP COLUMN gateway_id;
        PRAGMA user_version=26;
        PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    let before:Vec<rusqlite::types::Value>=store.connection.query_row(
        "SELECT boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,run_id,status,recovered_by,cancellation_reason FROM command_journal",[],
        |r|(0..14).map(|i|r.get(i)).collect()).unwrap();
    crate::backup::create(&active, &before_snapshot, Duration::from_secs(2)).unwrap();
    let snapshot_bytes = std::fs::read(&before_snapshot).unwrap();
    drop(store);
    let mut migrated = Store::open(&active).unwrap();
    assert_eq!(
        migrated
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        27
    );
    let after:Vec<rusqlite::types::Value>=migrated.connection.query_row(
        "SELECT boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,run_id,status,recovered_by,cancellation_reason FROM command_journal",[],
        |r|(0..14).map(|i|r.get(i)).collect()).unwrap();
    assert_eq!(after, before);
    assert!(
        matches!(
            migrated.command_wire_scope("previous-boot", 1),
            Err(Error::InvalidAudit)
        ),
        "migration cannot guess a legacy wire scope from the latest mapping"
    );
    assert_eq!(migrated.recover_commands("fresh-boot").unwrap(), 1);
    assert_eq!(
        migrated.command_status("previous-boot", 1).unwrap(),
        Some(Status::Uncertain)
    );
    assert_eq!(std::fs::read(&before_snapshot).unwrap(), snapshot_bytes);
    assert_eq!(
        crate::backup::inspect(
            &before_snapshot,
            homesense_core::ExecutionContext::Simulation
        )
        .unwrap()
        .schema,
        26,
        "rollback uses the preserved standalone predecessor snapshot, never scope deletion from live27"
    );
    drop(migrated);
    let reopened = Store::open(&active).unwrap();
    assert_eq!(count(&reopened, "gateway_fault"), 0);
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn parsed_64_gateway_configuration_sync_and_full_dependency_persistence_preserve_capacity() {
    use homesense_core::gateway_scope::{GatewayTable, MAX_GATEWAYS, Port, ReadRequirement};
    fn document(count: usize) -> Vec<u8> {
        let gateways=(0..count).map(|i|format!(r#"{{"id":"bus-{i}","endpoint":"127.0.0.1:{}","modules":[{{"id":"module-{i}","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{{"status":"simulated"}}}}]}}"#,5000+i)).collect::<Vec<_>>().join(",");
        let devices=(0..count).map(|i|format!(r#"{{"id":"device-{i}","name":"Synthetic {i}","module_id":"module-{i}","channel":1,"enabled":true}}"#)).collect::<Vec<_>>().join(",");
        format!(
            r#"{{"schema_version":2,"revision":1,"gateways":[{gateways}],"devices":[{devices}]}}"#
        )
        .into_bytes()
    }
    assert_eq!(MAX_GATEWAYS, 64);
    let bytes = document(64);
    let config = homesense_config::Configuration::parse(
        &bytes,
        homesense_core::ExecutionContext::Simulation,
    )
    .unwrap();
    let mut table = GatewayTable::new(global(), config.gateways().keys().cloned()).unwrap();
    for gateway in config.gateways().keys() {
        let scope = table.begin_attempt(gateway).unwrap();
        table
            .begin_synchronization(
                &scope,
                (1..=8).map(|channel| ReadRequirement {
                    port: Port {
                        address: 0x11,
                        channel,
                    },
                    before_count: 0,
                    sent_at_ms: 10,
                    deadline_ms: 100,
                }),
            )
            .unwrap();
        for channel in 1..=8 {
            table
                .received(
                    &scope,
                    Port {
                        address: 0x11,
                        channel,
                    },
                    1,
                    20,
                )
                .unwrap();
        }
        assert!(table.ready(&scope));
    }
    let dependencies = table.capture(config.gateways().keys().cloned()).unwrap();
    assert_eq!(dependencies.wires().len(), 64);
    let mut store = Store::open(":memory:").unwrap();
    store
        .publish_configuration(
            None,
            &bytes,
            homesense_core::ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let mut cmd = command(
        1,
        Command::SetLevel {
            level: homesense_core::Percent::new(30).unwrap(),
            transition: homesense_core::LevelTransition::Immediate,
        },
    );
    cmd.device_id = DeviceId::new("device-0").unwrap();
    let op = OperationScope {
        key: key(),
        global: global(),
        wires: dependencies.wires().to_vec(),
    };
    accept(
        &mut store,
        &[cmd],
        Acceptance {
            operations: vec![op],
            commands: vec![CommandScope {
                command: 1,
                physical: wire("bus-0", 1),
                requirements: dependencies.wires().to_vec(),
                operations: vec![key()],
            }],
        },
    )
    .unwrap();
    assert_eq!(count(&store, "gateway_operation_wire"), 64);
    assert_eq!(count(&store, "gateway_command_requirement"), 64);
    assert!(
        homesense_config::Configuration::parse(
            &document(65),
            homesense_core::ExecutionContext::Simulation
        )
        .is_err()
    );
    assert!(GatewayTable::new(global(), (0..65).map(|i| format!("bus-{i}"))).is_err());
    let too_many = OperationScope {
        key: OperationKey {
            kind: OperationKind::Delay,
            id: "too-many".into(),
        },
        global: global(),
        wires: (0..65).map(|i| wire(&format!("bus-{i}"), 1)).collect(),
    };
    assert!(matches!(
        store.reserve_gateway_operations(BOOT, &[too_many], 30),
        Err(Error::InvalidAudit)
    ));
}

#[test]
fn claimed_schema26_with_wrong_scope_table_is_rejected_without_mutating_rows_or_schema() {
    let directory = std::env::temp_dir().join(format!(
        "gateway-invalid-migration-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("database.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish(None, 1, b"synthetic configuration", &[])
        .unwrap();
    store
        .accept_commands("previous-boot", &[command(1, Command::Switch(true))], 10)
        .unwrap();
    pre_gateway_scope_fixture::strip(&store.connection);
    store
        .connection
        .execute_batch(
            "CREATE TABLE gateway_operation (boot_id TEXT);
        INSERT INTO gateway_operation VALUES('wrong-shape'); PRAGMA user_version=26;",
        )
        .unwrap();
    drop(store);
    let before = std::fs::read(&path).unwrap();
    assert!(
        Store::open(&path).is_err(),
        "mandatory scope migration must not adopt same-name wrong structure"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "failed migration must preserve original database bytes"
    );
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        26
    );
    assert_eq!(
        sql.query_row("SELECT status FROM command_journal", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "accepted"
    );
    assert_eq!(
        sql.query_row("SELECT boot_id FROM gateway_operation", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "wrong-shape"
    );
    assert!(
        !sql.prepare("PRAGMA table_info(command_journal)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .any(|name| name.unwrap() == "gateway_id")
    );
    drop(sql);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn standalone_schema27_backup_is_verified_read_only_and_can_be_exported_for_old_reader_check() {
    use std::time::Duration;
    let directory = std::env::temp_dir().join(format!(
        "gateway-new-backup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("active.sqlite");
    let backup = directory.join("snapshot.sqlite");
    let mut store = Store::open(&source).unwrap();
    store
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation.json"),
            homesense_core::ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    crate::backup::create(&source, &backup, Duration::from_secs(2)).unwrap();
    let before = std::fs::read(&backup).unwrap();
    let report =
        crate::backup::inspect(&backup, homesense_core::ExecutionContext::Simulation).unwrap();
    assert_eq!(report.schema, 27);
    assert_eq!(report.active_revision, Some(1));
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    // Opt-in synthetic evidence only; no private configuration/identity data.
    if let Some(destination) = std::env::var_os("HOMESENSE_PRIVATE_SCOPE_EVIDENCE") {
        let destination = std::path::PathBuf::from(destination);
        std::fs::create_dir_all(&destination).unwrap();
        crate::backup::create(
            &source,
            &destination.join("schema27.sqlite"),
            Duration::from_secs(2),
        )
        .unwrap();
    }
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn child_led_dependencies_retain_original_motor_wire_and_full_decision_reads() {
    let mut store=database();
    let mut motor=scope(51,"motor-a");motor.requirements=vec![wire("predicate-b",3)];
    accept(&mut store,&[command(51,Command::PanelLed(true))],Acceptance {commands:vec![motor],operations:vec![]}).unwrap();
    let original=store.command_gateway_requirements(BOOT,51).unwrap();
    assert_eq!(original,vec![wire("motor-a",1),wire("predicate-b",3)]);
    store.fail_gateway(BOOT,global(),&wire("motor-a",1),20).unwrap();
    assert_eq!(store.command_gateway_requirements(BOOT,51).unwrap(),original);
    assert!(store.command_gateway_requirements(BOOT,999).is_err());
}
#[test]
fn standalone_scene_begin_captures_full_scope_atomically_without_definition() {
    let mut store=database();
    let key=OperationKey {kind:OperationKind::Scene,id:"standalone-timeline".into()};
    let operation=OperationScope {key:key.clone(),global:global(),wires:vec![wire("a",1),wire("b",2)]};
    store.begin_scene_run_scoped(BOOT,&key.id,1,None,10,&operation).unwrap();
    let (stored,status)=store.gateway_operation_scope(BOOT,&key).unwrap().unwrap();
    assert_eq!(stored.wires,operation.wires);assert_eq!(status,"active");
    let fault=store.fail_gateway(BOOT,global(),&wire("a",1),20).unwrap();
    assert_eq!(fault.operations,vec![key.clone()]);
    let mut bad=operation;bad.key.id="atomic-bad-timeline".into();bad.wires=vec![wire("a",1),wire("a",2)];
    assert!(store.begin_scene_run_scoped(BOOT,&bad.key.id,1,None,30,&bad).is_err());
    assert!(store.gateway_operation_scope(BOOT,&bad.key).unwrap().is_none());
    let count:i64=store.connection.query_row("SELECT count(*) FROM scene_run WHERE run_id=?1",[&bad.key.id],|row|row.get(0)).unwrap();assert_eq!(count,0);
}

#[test]
fn completed_timeline_scope_keeps_history_without_later_fault_reclassification() {
    let mut store=database();
    let key=OperationKey {kind:OperationKind::Scene,id:"completed-timeline".into()};
    let operation=OperationScope {key:key.clone(),global:global(),wires:vec![wire("a",1)]};
    store.begin_scene_run_scoped(BOOT,&key.id,1,None,10,&operation).unwrap();
    store.checkpoint_scene_run(BOOT,&key.id,0,crate::scene_run::Status::CommandsWritten,20).unwrap();
    let (original,status)=store.gateway_operation_scope(BOOT,&key).unwrap().unwrap();
    assert_eq!(status,"completed");assert_eq!(original.wires,operation.wires);
    assert!(store.fail_gateway(BOOT,global(),&wire("a",1),30).unwrap().operations.is_empty());
    assert_eq!(store.scene_run(BOOT,&key.id).unwrap().unwrap().status,crate::scene_run::Status::CommandsWritten);
}

fn native_scoped_request(store:&mut Store,id:u64)->(crate::native_scene::Request,OperationKey) {
    store.define_logical_variable(1,"native-selection",homesense_core::rule::ValueType::Integer).unwrap();
    let request=crate::native_scene::Request {controller:"synthetic-native-controller".into(),key:"one-native-birth".into(),scene:"one-scene".into(),group:"one-group".into(),variable:"native-selection".into(),variable_version:0,boot:BOOT.into(),epochs:global(),deadline_ms:1000};
    store.reserve_native_scene(&request,1).unwrap();store.consume_native_phase(&request,0,2).unwrap();
    let key=OperationKey {kind:OperationKind::Native,id:((1u64<<62)+id).to_string()};
    let operation=OperationScope {key:key.clone(),global:global(),wires:vec![wire("a",1),wire("b",1)]};
    let mut physical=scope(id,"b");physical.operations=vec![key.clone()];
    accept(store,&[command(id,Command::Query)],Acceptance {commands:vec![physical],operations:vec![operation]}).unwrap();
    store.native_phase_accepted(&request,1,&[id],0,10).unwrap();
    (request,key)
}
#[test]
fn native_pending_phase_is_unknown_on_original_gateway_fault_without_consuming_again() {
    let mut store=database();let(request,key)=native_scoped_request(&mut store,81);
    let fault=store.fail_gateway(BOOT,global(),&wire("a",1),20).unwrap();assert_eq!(fault.operations,vec![key]);
    let record=store.native_scene_request(&request.controller,&request.key).unwrap().unwrap();
    assert_eq!(record.status,crate::native_scene::Status::Unknown);assert_eq!(record.phase,1);
    assert!(store.consume_native_phase(&request,1,30).is_err());
    assert_eq!(store.command_status(BOOT,81).unwrap(),Some(crate::journal::Status::Cancelled));
}
#[test]
fn successful_native_request_closes_only_its_original_scope_in_same_transaction() {
    let mut store=database();let(request,key)=native_scoped_request(&mut store,82);
    assert!(store.begin_command_write_scoped(BOOT,82,11,&wire("b",1)).unwrap());
    store.transition_command(BOOT,82,Status::Writing,Status::Written,12).unwrap();
    store.finish_native_scene(&request,1,Some(1),&[],20).unwrap();
    let (scope,status)=store.gateway_operation_scope(BOOT,&key).unwrap().unwrap();assert_eq!(status,"completed");assert_eq!(scope.wires,vec![wire("a",1),wire("b",1)]);
    assert!(store.fail_gateway(BOOT,global(),&wire("a",1),30).unwrap().operations.is_empty());
    assert_eq!(store.native_scene_request(&request.controller,&request.key).unwrap().unwrap().status,crate::native_scene::Status::Observed);
}
