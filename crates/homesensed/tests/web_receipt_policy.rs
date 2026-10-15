use homesensed::{
    web_receipt_policy::{Expiry, Policy},
    web_receipts::{Claim, Endpoint, Receipts},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use uuid::Uuid;
fn path() -> PathBuf {
    std::env::temp_dir().join(format!("receipt-policy-{}.sqlite", Uuid::new_v4()))
}
fn clock(n: &Arc<AtomicU64>) -> Arc<dyn Fn() -> Result<u64, String> + Send + Sync> {
    let n = n.clone();
    Arc::new(move || Ok(n.load(Ordering::Acquire)))
}
fn policy(limit: u32) -> Policy {
    Policy {
        max_records: limit,
        expiry: Expiry::MarkExpiredPreserveFence { window_ms: 100 },
    }
}
#[test]
fn endpoint_scope_expiry_reopen_policy_change_and_late_finish_keep_same_record_and_fence() {
    let p = path();
    let n = Arc::new(AtomicU64::new(1000));
    let mut r = Receipts::open_with_clock(&p, policy(3), clock(&n)).unwrap();
    for e in [Endpoint::DeviceCommands, Endpoint::SceneActions] {
        assert_eq!(
            r.claim_scoped("p", &e, "same", "body").unwrap().claim,
            Claim::New
        );
    }
    r.finish_scoped(
        "p",
        &Endpoint::DeviceCommands,
        "same",
        "original exact response",
    )
    .unwrap();
    n.store(1100, Ordering::Release);
    let old = r
        .claim_scoped("p", &Endpoint::DeviceCommands, "same", "body")
        .unwrap();
    assert_eq!(
        old.claim,
        Claim::Existing(Some("original exact response".into()))
    );
    assert!(old.expired);
    assert_eq!(
        r.claim_scoped("p", &Endpoint::DeviceCommands, "same", "other")
            .unwrap()
            .claim,
        Claim::Conflict
    );
    let unresolved = r
        .claim_scoped("p", &Endpoint::SceneActions, "same", "body")
        .unwrap();
    assert_eq!(unresolved.claim, Claim::Existing(None));
    assert!(unresolved.expired);
    r.finish_scoped(
        "p",
        &Endpoint::SceneActions,
        "same",
        "late same-request outcome",
    )
    .unwrap();
    drop(r);
    n.store(0, Ordering::Release);
    let mut r = Receipts::open_with_clock(&p, Policy::default(), clock(&n)).unwrap();
    let old = r
        .claim_scoped("p", &Endpoint::DeviceCommands, "same", "body")
        .unwrap();
    assert!(old.expired);
    assert_eq!(
        old.claim,
        Claim::Existing(Some("original exact response".into()))
    );
    assert!(
        r.lookup_scoped("p", &Endpoint::DeviceCommands, "same")
            .unwrap()
            .unwrap()
            .expired
    );
    assert!(
        r.claim_scoped("p", &Endpoint::ModuleCommands, "new", "body")
            .is_err()
    );
    assert_eq!(
        r.claim_scoped("p", &Endpoint::SceneActions, "same", "body")
            .unwrap()
            .claim,
        Claim::Existing(Some("late same-request outcome".into()))
    );
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn expiry_never_releases_capacity_and_reduced_capacity_preserves_history() {
    let p = path();
    let n = Arc::new(AtomicU64::new(1000));
    let mut r = Receipts::open_with_clock(&p, policy(2), clock(&n)).unwrap();
    for k in ["a", "b"] {
        r.claim_scoped("p", &Endpoint::SceneActions, k, k).unwrap();
    }
    n.store(9999, Ordering::Release);
    assert!(
        r.claim_scoped("p", &Endpoint::SceneActions, "a", "a")
            .unwrap()
            .expired
    );
    assert!(
        r.claim_scoped("p", &Endpoint::SceneActions, "c", "c")
            .is_err()
    );
    drop(r);
    let mut r = Receipts::open_with_clock(&p, policy(1), clock(&n)).unwrap();
    assert_eq!(
        r.claim_scoped("p", &Endpoint::SceneActions, "b", "b")
            .unwrap()
            .claim,
        Claim::Existing(None)
    );
    assert!(
        r.claim_scoped("p", &Endpoint::SceneActions, "c", "c")
            .is_err()
    );
    assert_eq!(
        rusqlite::Connection::open(&p)
            .unwrap()
            .query_row("SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn legacy_migration_preserves_exact_unknown_metadata_and_blocks_old_binary_writes() {
    let p = path();
    let sql = rusqlite::Connection::open(&p).unwrap();
    sql.execute_batch("CREATE TABLE web_receipt(principal TEXT NOT NULL,request_key TEXT NOT NULL,fingerprint TEXT NOT NULL,response TEXT,PRIMARY KEY(principal,request_key));INSERT INTO web_receipt VALUES('p','done','exact old body','exact old response');INSERT INTO web_receipt VALUES('p','unknown','unknown body',NULL);").unwrap();
    drop(sql);
    let mut r = Receipts::open_with_policy(&p, policy(10)).unwrap();
    for e in [
        Endpoint::DeviceCommands,
        Endpoint::SceneActions,
        Endpoint::ModuleQueries,
    ] {
        let row = r.lookup_scoped("p", &e, "done").unwrap().unwrap();
        assert!(row.legacy);
        assert_eq!(row.created_ms, None);
        assert_eq!(row.expires_ms, None);
        assert_eq!(row.response, Some("exact old response".into()));
        assert_eq!(
            r.claim_scoped("p", &e, "done", "exact old body")
                .unwrap()
                .claim,
            Claim::Existing(row.response)
        );
        assert_eq!(
            r.claim_scoped("p", &e, "unknown", "different")
                .unwrap()
                .claim,
            Claim::Conflict
        );
        assert_eq!(
            r.claim_scoped("p", &e, "unknown", "unknown body")
                .unwrap()
                .claim,
            Claim::Existing(None)
        );
    }
    let sql = rusqlite::Connection::open(&p).unwrap();
    assert!(sql.execute_batch("CREATE TABLE IF NOT EXISTS web_receipt (principal TEXT NOT NULL,request_key TEXT NOT NULL,fingerprint TEXT NOT NULL,response TEXT,PRIMARY KEY(principal,request_key));").is_err());
    assert!(
        sql.execute(
            "INSERT INTO web_receipt VALUES('p','new-old-binary','body',NULL)",
            []
        )
        .is_err()
    );
    assert!(
        sql.execute(
            "UPDATE web_receipt SET response='lost' WHERE request_key='done'",
            []
        )
        .is_err()
    );
    assert!(sql.execute("DELETE FROM web_receipt", []).is_err());
    assert_eq!(
        sql.query_row(
            "SELECT response FROM web_receipt_legacy WHERE request_key='done'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "exact old response"
    );
    drop(sql);
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn invalid_policy_and_ambiguous_migration_have_no_admission_or_partial_schema() {
    for text in [
        r#"{"max_records":0,"expiry":{"kind":"never"}}"#,
        r#"{"max_records":100001,"expiry":{"kind":"never"}}"#,
        r#"{"max_records":2,"expiry":{"kind":"delete_and_reuse"}}"#,
        r#"{"max_records":2,"expiry":{"kind":"mark_expired_preserve_fence","window_ms":0}}"#,
    ] {
        let dir = std::env::temp_dir().join(format!("policy-file-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("web-receipt-policy.json"), text).unwrap();
        let path = dir.join("web.sqlite");
        assert!(Receipts::open_configured(&path).is_err());
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    let p = path();
    let sql = rusqlite::Connection::open(&p).unwrap();
    sql.execute_batch(
        "CREATE TABLE web_receipt (wrong TEXT);INSERT INTO web_receipt VALUES('original');",
    )
    .unwrap();
    drop(sql);
    assert!(Receipts::open(&p).is_err());
    let sql = rusqlite::Connection::open(&p).unwrap();
    assert_eq!(
        sql.query_row("SELECT wrong FROM web_receipt", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "original"
    );
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn simultaneous_scope_claims_and_capacity_use_persistent_transactions() {
    let p = path();
    let n = Arc::new(AtomicU64::new(1000));
    drop(Receipts::open_with_clock(&p, policy(1), clock(&n)).unwrap());
    let handles = (0..8)
        .map(|_| {
            let p = p.clone();
            let n = n.clone();
            std::thread::spawn(move || {
                Receipts::open_with_clock(&p, policy(1), clock(&n))
                    .unwrap()
                    .claim_scoped("p", &Endpoint::SceneActions, "same", "body")
                    .unwrap()
                    .claim
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|c| **c == Claim::New).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|c| **c == Claim::Existing(None))
            .count(),
        7
    );
    let mut r = Receipts::open_with_clock(&p, policy(1), clock(&n)).unwrap();
    assert!(
        r.claim_scoped("p", &Endpoint::DeviceCommands, "same", "body")
            .is_err()
    );
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn canonical_routes_reject_aliases_and_configuration_uuid_is_normalized() {
    for p in [
        "/api/v1/%73cene-actions",
        "/api/v1/scene-actions/",
        "/api/v1/./scene-actions",
        "/api/v1//scene-actions",
        "/api/v1/SCENE-actions",
    ] {
        assert!(Endpoint::control(p).is_err());
    }
    let p = path();
    let id = Uuid::new_v4();
    let mut r = Receipts::open(&p).unwrap();
    let e = Endpoint::ConfigurationActivate(id);
    assert_eq!(
        r.claim_scoped("p", &e, "k", "body").unwrap().claim,
        Claim::New
    );
    let same =
        Endpoint::ConfigurationActivate(Uuid::parse_str(&id.to_string().to_uppercase()).unwrap());
    assert_eq!(
        r.claim_scoped("p", &same, "k", "body").unwrap().claim,
        Claim::Existing(None)
    );
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn first_lookup_expiry_write_failure_is_closed_and_high_water_commits_atomically() {
    let p = path();
    let n = Arc::new(AtomicU64::new(1000));
    let mut r = Receipts::open_with_clock(&p, policy(3), clock(&n)).unwrap();
    r.claim_scoped("p", &Endpoint::SceneActions, "key", "body")
        .unwrap();
    let sql = rusqlite::Connection::open(&p).unwrap();
    sql.execute_batch("CREATE TRIGGER expiry_failure BEFORE UPDATE OF expired_ms ON web_receipt_scoped BEGIN SELECT RAISE(ABORT,'synthetic expiry failure'); END;").unwrap();
    n.store(1200, Ordering::Release);
    assert!(
        r.lookup_scoped("p", &Endpoint::SceneActions, "key")
            .is_err()
    );
    assert_eq!(
        sql.query_row("SELECT expired_ms FROM web_receipt_scoped", [], |r| r
            .get::<_, Option<i64>>(0))
            .unwrap(),
        None
    );
    assert_eq!(
        sql.query_row("SELECT last_seen_ms FROM web_receipt_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1000
    );
    sql.execute_batch("DROP TRIGGER expiry_failure;").unwrap();
    assert!(
        r.lookup_scoped("p", &Endpoint::SceneActions, "key")
            .unwrap()
            .unwrap()
            .expired
    );
    n.store(1100, Ordering::Release);
    assert!(
        r.claim_scoped("p", &Endpoint::DeviceCommands, "new", "body")
            .is_err()
    );
    assert_eq!(
        r.claim_scoped("p", &Endpoint::SceneActions, "key", "body")
            .unwrap()
            .claim,
        Claim::Existing(None)
    );
    drop(sql);
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn new_format_backup_restore_preserves_fences_and_old_binary_cannot_insert() {
    let p = path();
    let backup = path();
    let n = Arc::new(AtomicU64::new(1000));
    let mut r = Receipts::open_with_clock(&p, policy(2), clock(&n)).unwrap();
    r.claim_scoped("p", &Endpoint::SceneActions, "key", "body")
        .unwrap();
    n.store(1200, Ordering::Release);
    assert!(
        r.lookup_scoped("p", &Endpoint::SceneActions, "key")
            .unwrap()
            .unwrap()
            .expired
    );
    let sql = rusqlite::Connection::open(&p).unwrap();
    let mut restored = rusqlite::Connection::open(&backup).unwrap();
    rusqlite::backup::Backup::new(&sql, &mut restored)
        .unwrap()
        .run_to_completion(10, std::time::Duration::from_millis(1), None)
        .unwrap();
    drop(restored);
    drop(sql);
    drop(r);
    n.store(0, Ordering::Release);
    let mut restored = Receipts::open_with_clock(&backup, Policy::default(), clock(&n)).unwrap();
    let old = restored
        .claim_scoped("p", &Endpoint::SceneActions, "key", "body")
        .unwrap();
    assert!(old.expired);
    assert_eq!(old.claim, Claim::Existing(None));
    let old = rusqlite::Connection::open(&backup).unwrap();
    assert!(
        old.execute(
            "INSERT INTO web_receipt VALUES('p','new-old-program','body',NULL)",
            []
        )
        .is_err()
    );
    drop(old);
    drop(restored);
    std::fs::remove_file(p).unwrap();
    std::fs::remove_file(backup).unwrap();
}
#[test]
fn configured_file_is_strict_and_legacy_rows_consume_the_same_capacity() {
    let dir = std::env::temp_dir().join(format!("receipt-config-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("web-receipt-policy.json"),
        r#"{"max_records":1,"expiry":{"kind":"mark_expired_preserve_fence","window_ms":10}}"#,
    )
    .unwrap();
    let p = dir.join("web.sqlite");
    let mut r = Receipts::open_configured(&p).unwrap();
    assert_eq!(r.policy().max_records, 1);
    r.claim("p", "old-global", "old").unwrap();
    assert!(
        r.claim_scoped("p", &Endpoint::SceneActions, "fresh", "body")
            .is_err()
    );
    assert_eq!(
        r.claim_scoped("p", &Endpoint::DeviceCommands, "old-global", "old")
            .unwrap()
            .claim,
        Claim::Existing(None)
    );
    drop(r);
    std::fs::remove_dir_all(dir).unwrap();
}
#[path = "support/baseline_web_receipts.rs"]
mod historical;
#[test]
fn actual_baseline_ledger_initialization_fails_after_upgrade_and_raw_records_remain() {
    let p = path();
    let mut old = historical::Receipts::open(&p).unwrap();
    assert_eq!(
        old.claim("original", "key", "exact-original").unwrap(),
        historical::Claim::New
    );
    assert_eq!(
        old.lookup("original", "key").unwrap(),
        Some(("exact-original".into(), None))
    );
    old.finish("original", "key", "exact-old-response").unwrap();
    drop(old);
    let mut new = Receipts::open(&p).unwrap();
    assert!(historical::Receipts::open(&p).is_err());
    assert_eq!(
        new.claim_scoped(
            "original",
            &Endpoint::DeviceCommands,
            "key",
            "exact-original"
        )
        .unwrap()
        .claim,
        Claim::Existing(Some("exact-old-response".into()))
    );
    drop(new);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn intermediate_view_format_upgrade_preserves_full_state() {
    let p = path();
    let sql = rusqlite::Connection::open(&p).unwrap();
    sql.execute_batch("CREATE TABLE web_receipt_legacy(principal TEXT,request_key TEXT,fingerprint TEXT,response TEXT,PRIMARY KEY(principal,request_key));CREATE TABLE web_receipt_scoped(principal TEXT,endpoint TEXT,request_key TEXT,fingerprint TEXT,response TEXT,created_ms INTEGER,expires_ms INTEGER,expired_ms INTEGER,PRIMARY KEY(principal,endpoint,request_key));CREATE TABLE web_receipt_meta(singleton INTEGER PRIMARY KEY,last_seen_ms INTEGER);INSERT INTO web_receipt_meta VALUES(1,9000);INSERT INTO web_receipt_scoped VALUES('p','/api/v1/scene-actions','key','body','exact',1000,1100,1200);CREATE VIEW web_receipt AS SELECT principal,request_key,fingerprint,response FROM web_receipt_legacy UNION ALL SELECT principal,request_key,fingerprint,response FROM web_receipt_scoped;PRAGMA user_version=2;").unwrap();
    drop(sql);
    let n = Arc::new(AtomicU64::new(0));
    let mut r = Receipts::open_with_clock(&p, Policy::default(), clock(&n)).unwrap();
    let record = r
        .lookup_scoped("p", &Endpoint::SceneActions, "key")
        .unwrap()
        .unwrap();
    assert!(record.expired);
    assert_eq!(record.response, Some("exact".into()));
    assert_eq!(record.created_ms, Some(1000));
    assert_eq!(record.expires_ms, Some(1100));
    assert!(
        r.claim_scoped("p", &Endpoint::SceneActions, "new", "body")
            .is_err()
    );
    let sql = rusqlite::Connection::open(&p).unwrap();
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        sql.query_row("SELECT last_seen_ms FROM web_receipt_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        9000
    );
    drop(sql);
    drop(r);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn failed_view_upgrade_does_not_drop_legacy_view_or_bump_format() {
    let p = path();
    let sql = rusqlite::Connection::open(&p).unwrap();
    sql.execute_batch("CREATE TABLE web_receipt_legacy(principal TEXT,request_key TEXT,fingerprint TEXT,response TEXT);INSERT INTO web_receipt_legacy VALUES('p','k','exact-body','exact-response');CREATE TABLE web_receipt_scoped(principal TEXT,request_key TEXT,fingerprint TEXT,response TEXT);CREATE TABLE web_receipt_meta(singleton INTEGER PRIMARY KEY,last_seen_ms INTEGER);INSERT INTO web_receipt_meta VALUES(1,NULL);CREATE VIEW web_receipt AS SELECT * FROM web_receipt_legacy;PRAGMA user_version=2;").unwrap();
    drop(sql);
    assert!(Receipts::open(&p).is_err());
    let sql = rusqlite::Connection::open(&p).unwrap();
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        sql.query_row("SELECT response FROM web_receipt", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "exact-response"
    );
    assert_eq!(
        sql.query_row(
            "SELECT type FROM sqlite_master WHERE name='web_receipt'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "view"
    );
    drop(sql);
    std::fs::remove_file(p).unwrap();
}
