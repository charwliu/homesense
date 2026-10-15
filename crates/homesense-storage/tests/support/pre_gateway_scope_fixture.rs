//! Reconstruct a real pre27 input before an existing historical migration test.
//! Only empty new scope metadata is removed; business rows and assertions stay.
pub fn strip(connection: &rusqlite::Connection) {
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        27,
        "historical fixture starts from the current Store, not an already downgraded database"
    );
    for table in [
        "gateway_command_operation",
        "gateway_command_requirement",
        "gateway_operation_wire",
        "gateway_operation",
        "gateway_fault",
    ] {
        let rows: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 0,
            "test fixture cannot erase real scope history in {table}"
        );
    }
    let scoped:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM command_journal WHERE gateway_id IS NOT NULL OR gateway_epoch IS NOT NULL)",[],|r|r.get(0)).unwrap();
    assert!(
        !scoped,
        "test fixture cannot discard a real accepted physical scope"
    );
    let foreign_keys: bool = connection
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    connection
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
        ALTER TABLE command_journal DROP COLUMN gateway_id;",
        )
        .unwrap();
    connection
        .pragma_update(None, "foreign_keys", foreign_keys)
        .unwrap();
}
