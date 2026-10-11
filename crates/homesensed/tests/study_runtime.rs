use homesense_core::{ExecutionContext, queue::Origin};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    study_runtime::{StudyRuntime, TimingPolicy},
};
use std::path::PathBuf;
use uuid::Uuid;
struct Db(PathBuf);
impl Drop for Db {
    fn drop(&mut self) {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", self.0.display()));
        }
        let _ = std::fs::remove_file(self.0.with_extension("runtime.lock"));
    }
}
fn database() -> Db {
    let mut d: hap::serde_json::Value = hap::serde_json::from_slice(include_bytes!(
        "../../../config/import/study-lights.draft.json"
    ))
    .unwrap();
    let c = &mut d["configuration"];
    for dev in c["devices"].as_array_mut().unwrap() {
        dev["enabled"] = true.into();
    }
    for m in c["gateways"][0]["modules"].as_array_mut().unwrap() {
        m["verification"] = hap::serde_json::json!({"status":"verified","evidence":"isolated in-memory test fixture only","verified_at_unix_ms":1});
    }
    c["gateways"][0]["modules"].as_array_mut().unwrap().push(hap::serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"verified","evidence":"isolated in-memory test fixture only","verified_at_unix_ms":1},"led_mode":"manual"}));
    for key in [1, 3, 5] {
        c["devices"].as_array_mut().unwrap().push(hap::serde_json::json!({"id":format!("control4:63:key:{key}"),"name":"fixture","gateway":"hopebeauty","address":"41","channel":key,"enabled":true}));
    }
    let db = Db(std::env::temp_dir().join(format!("study-{}.sqlite", Uuid::new_v4())));
    Store::open(&db.0)
        .unwrap()
        .publish_configuration(
            None,
            &hap::serde_json::to_vec(c).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    db
}
async fn actor(db: &Db, id: Uuid) -> Admission {
    let a = Admission::open(
        db.0.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |v| v == id,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    a
}
fn runtime(a: &Admission, id: Uuid) -> StudyRuntime {
    StudyRuntime::new(
        a.clone(),
        id,
        include_bytes!("../../../config/scenes/study.json"),
        TimingPolicy::ModuleNative,
    )
    .unwrap()
}
#[tokio::test]
async fn rule_origin_cancel_persistence_and_no_restart_replay() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    assert!(scene.toggle(1).await.is_err());
    let first = scene.activate(1).await.unwrap();
    assert_eq!(first.command_ids.len(), 7);
    assert_eq!(scene.mode().await.unwrap(), Some(1));
    let ticket = a.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.origin, Origin::Rule);
    assert_eq!(ticket.command.run_id, Some(first.command_ids[0]));
    assert_eq!(scene.cancel().await.unwrap(), 6);
    a.finish_write(ticket.command.id, true).await.unwrap();
    assert!(a.dispatch().await.unwrap().is_none());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store
            .command_status(a.boot_id(), first.command_ids[1])
            .unwrap(),
        Some(Status::Cancelled)
    );
    let off = scene.toggle(1).await.unwrap();
    assert_eq!(off.mode, 0);
    let boot = a.boot_id().to_owned();
    a.shutdown().await.unwrap();
    let b = actor(&db, id).await;
    assert_eq!(b.study_mode().await.unwrap(), Some(0));
    assert!(b.dispatch().await.unwrap().is_none());
    assert_eq!(
        store.command_status(&boot, off.command_ids[0]).unwrap(),
        Some(Status::Uncertain)
    );
    b.shutdown().await.unwrap();
}
#[tokio::test]
async fn scene_acceptance_failure_rolls_back_mode_and_commands() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_mode BEFORE INSERT ON study_mode BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(runtime(&a, id).activate(3).await.is_err());
    assert_eq!(Store::open(&db.0).unwrap().study_mode(1).unwrap(), None);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn replacement_cancels_old_run_and_denies_other_principal() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    assert!(runtime(&a, Uuid::new_v4()).activate(1).await.is_err());
    let mut scene = runtime(&a, id);
    let old = scene.activate(1).await.unwrap();
    let new = scene.activate(5).await.unwrap();
    assert_eq!(new.mode, 3);
    let store = Store::open(&db.0).unwrap();
    for command in old.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), command).unwrap(),
            Some(Status::Cancelled)
        );
    }
    for _ in 0..7 {
        let ticket = a.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.run_id, Some(new.command_ids[0]));
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn direct_manual_control_cancels_only_that_devices_pending_scene_action() {
    use homesense_core::{Command, DeviceId};
    use homesense_hap::requests::{Batch, Intent};
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    let run = scene.activate(1).await.unwrap();
    a.submit(Batch {
        controller: id,
        intents: vec![Intent {
            device: DeviceId::new("control4:210").unwrap(),
            command: Command::Switch(false),
        }],
        subscriptions: vec![],
    })
    .await
    .unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store
            .command_status(a.boot_id(), run.command_ids[0])
            .unwrap(),
        Some(Status::Cancelled)
    );
    let manual = a.dispatch().await.unwrap().unwrap();
    assert_eq!(manual.command.origin, Origin::Hap);
    a.finish_write(manual.command.id, true).await.unwrap();
    let mut remaining = 0;
    while let Some(ticket) = a.dispatch().await.unwrap() {
        assert_ne!(ticket.command.device_id.as_str(), "control4:210");
        remaining += 1;
        a.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert_eq!(remaining, 6);
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn energy_scene_executes_through_transport_owner_in_source_order() {
    use std::{
        io::{Read, Write},
        sync::{Arc, Mutex},
    };
    struct Wire(Arc<Mutex<Vec<u8>>>);
    impl Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::TimedOut.into())
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let db = database();
    let id = Uuid::new_v4();
    let a = Admission::open(db.0.clone(), Options::default(), move |v| v == id)
        .await
        .unwrap();
    a.synchronized(1, 1).await.unwrap();
    let mut scene = runtime(&a, id);
    let run = scene.activate(5).await.unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        a.clone(),
        [(
            "hopebeauty".into(),
            hb_transport::Connection::new(Wire(bytes.clone()), 1),
        )]
        .into(),
    );
    for command in &run.command_ids {
        let out = owner.step().await.unwrap().unwrap();
        assert_eq!(*command, out.command_id);
        out.result.unwrap();
    }
    assert_eq!(
        *bytes.lock().unwrap(),
        vec![
            0xae, 0x41, 5, 0xf1, 0xee, 0xae, 0x41, 1, 0xf2, 0xee, 0xae, 0x41, 3, 0xf2, 0xee, 0xae,
            3, 4, 0xf2, 0xee, 0xae, 7, 6, 0xf1, 0xee, 0xae, 0x11, 0xb5, 17, 0xee, 0xae, 0x12, 4, 0,
            0xee
        ]
    );
    let store = Store::open(&db.0).unwrap();
    for command in run.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), command).unwrap(),
            Some(Status::Written)
        );
    }
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn failed_cancellation_keeps_pending_run_and_audit_consistent() {
    let db = database();
    let id = Uuid::new_v4();
    let a = actor(&db, id).await;
    let mut scene = runtime(&a, id);
    let run = scene.activate(3).await.unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE OF status ON command_journal WHEN NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(scene.cancel().await.is_err());
    let store = Store::open(&db.0).unwrap();
    for command in &run.command_ids {
        assert_eq!(
            store.command_status(a.boot_id(), *command).unwrap(),
            Some(Status::Accepted)
        );
    }
    sql.execute_batch("DROP TRIGGER reject_cancel;").unwrap();
    assert_eq!(scene.cancel().await.unwrap(), 7);
    assert!(a.dispatch().await.unwrap().is_none());
    a.shutdown().await.unwrap();
}
