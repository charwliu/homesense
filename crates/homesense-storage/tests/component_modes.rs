use homesense_core::rule::{Value, ValueType};
use homesense_storage::{
    Store,
    variables::{ReadCheck, Update},
};
#[test]
fn component_values_and_local_inference_audit_rollback_together_and_reopen() {
    let dir = std::env::temp_dir().join(format!(
        "component-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("runtime.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish(None, 1, b"synthetic configuration", &[])
        .unwrap();
    store.ensure_component_mode_audit().unwrap();
    for name in ["a", "b"] {
        store
            .define_logical_variable(1, name, ValueType::Integer)
            .unwrap();
    }
    let updates = || {
        ["a", "b"]
            .iter()
            .map(|name| {
                (
                    Update {
                        revision: 1,
                        name: (*name).into(),
                        expected_version: 0,
                        value: Some(Value::Integer(1)),
                    },
                    format!("policy-{name}"),
                )
            })
            .collect::<Vec<_>>()
    };
    let reads = || {
        ["a", "b"]
            .iter()
            .map(|name| ReadCheck {
                revision: 1,
                name: (*name).into(),
                expected_version: 0,
            })
            .collect::<Vec<_>>()
    };
    let mut conflict = updates();
    conflict[1].0.expected_version = 1;
    assert!(
        store
            .apply_component_mode_inference(
                &conflict,
                &reads(),
                "boot",
                "controller",
                10,
                &[1, 2],
                true
            )
            .is_err()
    );
    for name in ["a", "b"] {
        assert_eq!(
            store.logical_variable(1, name).unwrap().unwrap().value,
            None
        );
        assert!(store.component_mode_audit(1, name, 1).unwrap().is_none());
    }
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_second_component BEFORE INSERT ON component_mode_audit WHEN NEW.name='b' BEGIN SELECT RAISE(ABORT,'synthetic rollback'); END;").unwrap();
    assert!(
        store
            .apply_component_mode_inference(
                &updates(),
                &reads(),
                "boot",
                "controller",
                10,
                &[1, 2],
                true
            )
            .is_err()
    );
    for name in ["a", "b"] {
        assert_eq!(store.logical_variable(1, name).unwrap().unwrap().version, 0);
        assert!(store.component_mode_audit(1, name, 1).unwrap().is_none());
    }
    sql.execute_batch("DROP TRIGGER fail_second_component")
        .unwrap();
    drop(sql);
    store
        .apply_component_mode_inference(
            &updates(),
            &reads(),
            "boot",
            "controller",
            10,
            &[1, 2],
            true,
        )
        .unwrap();
    drop(store);
    let store = Store::open(path).unwrap();
    for name in ["a", "b"] {
        let value = store.logical_variable(1, name).unwrap().unwrap();
        assert_eq!(value.value, Some(Value::Integer(1)));
        assert_eq!(value.version, 1);
        let audit = store.component_mode_audit(1, name, 1).unwrap().unwrap();
        assert_eq!(audit.policy, format!("policy-{name}"));
        assert_eq!(audit.controller, "controller");
    }
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn canonical_source_and_tags_and_provenance_share_original_acceptance_transaction() {
    use homesense_storage::component_modes::{Canonical, SourceValue};
    let dir = std::env::temp_dir().join(format!(
        "canonical-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("runtime.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish(None, 1, b"synthetic configuration", &[])
        .unwrap();
    store.ensure_component_mode_audit().unwrap();
    store
        .define_logical_variable(1, "source", ValueType::Integer)
        .unwrap();
    store
        .define_logical_variable(1, "tag", ValueType::Bool)
        .unwrap();
    store
        .accept_queue_batch_with_variables(
            "boot",
            &[],
            &[],
            0,
            None,
            &[Update {
                revision: 1,
                name: "source".into(),
                expected_version: 0,
                value: Some(Value::Integer(44)),
            }],
        )
        .unwrap();
    let source = Update {
        revision: 1,
        name: "source".into(),
        expected_version: 1,
        value: Some(Value::Integer(4)),
    };
    let tag = Update {
        revision: 1,
        name: "tag".into(),
        expected_version: 0,
        value: Some(Value::Bool(true)),
    };
    let metadata = Canonical {
        update: tag.clone(),
        policy: "reviewed-source4-label".into(),
        sources: vec![SourceValue {
            check: ReadCheck {
                revision: 1,
                name: "source".into(),
                expected_version: 2,
            },
            value: Value::Integer(4),
        }],
    };
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_canonical_audit BEFORE INSERT ON component_mode_audit BEGIN SELECT RAISE(ABORT,'synthetic atomic rollback'); END").unwrap();
    assert!(
        store
            .accept_queue_batch_with_components(
                "boot",
                &[],
                &[],
                1,
                None,
                &[source.clone(), tag.clone()],
                None,
                None,
                &[],
                &[],
                None,
                None,
                "controller",
                std::slice::from_ref(&metadata)
            )
            .is_err()
    );
    assert_eq!(
        store.logical_variable(1, "source").unwrap().unwrap().value,
        Some(Value::Integer(44))
    );
    assert_eq!(
        store.logical_variable(1, "tag").unwrap().unwrap().value,
        None
    );
    sql.execute_batch("DROP TRIGGER fail_canonical_audit")
        .unwrap();
    drop(sql);
    store
        .accept_queue_batch_with_components(
            "boot",
            &[],
            &[],
            1,
            None,
            &[source, tag],
            None,
            None,
            &[],
            &[],
            None,
            None,
            "controller",
            &[metadata],
        )
        .unwrap();
    assert_eq!(
        store.logical_variable(1, "source").unwrap().unwrap().value,
        Some(Value::Integer(4))
    );
    assert_eq!(
        store
            .component_mode_audit(1, "tag", 1)
            .unwrap()
            .unwrap()
            .provenance,
        "local_source_canonicalization"
    );
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row(
            "SELECT source_version FROM component_mode_source WHERE name='tag'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    drop(sql);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
