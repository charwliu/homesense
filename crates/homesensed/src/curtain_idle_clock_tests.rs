//! Receive-boundary expiry semantics with the existing private Admission clock.
//! Production still uses Instant; real transport/timing tests remain integration tests.
use crate as homesensed;
use crate::admission::{Admission, Options};
use homesense_core::{DeviceId, ExecutionContext};
use std::{sync::atomic::{AtomicU64, Ordering}, time::Duration};
use uuid::Uuid;
fn document() -> serde_json::Value {
    serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"26","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"panel-module","address":"4A","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"stop_led_idle_ms":200,"evidence":"synthetic review","reviewed_at_unix_ms":1,"leds":{"open":["open-led"],"close":["close-led"],"stop":["stop-led"]}}},{"id":"open-led","name":"Open LED","module_id":"panel-module","channel":2,"enabled":true},{"id":"stop-led","name":"Stop LED","module_id":"panel-module","channel":4,"enabled":true},{"id":"close-led","name":"Close LED","module_id":"panel-module","channel":6,"enabled":true}]})
}

async fn view(admission: &Admission, controller: Uuid) -> homesensed::curtain_owner::View {
    admission
        .curtain_views(
            controller,
            admission.task_generation().await.unwrap(),
            vec![DeviceId::new("curtain").unwrap()],
        )
        .await
        .unwrap()
        .remove(&DeviceId::new("curtain").unwrap())
        .unwrap()
}
#[tokio::test]
async fn curtain_idle_read_expires_pending_at_receive_boundary_without_motor_replay() {
    let root = std::env::temp_dir().join(format!("curtain-idle-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut d = document();
    d["devices"][0]["curtain_state"]["leds"] = serde_json::json!({});
    let db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_configuration(
            None,
            &serde_json::to_vec(&d).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let controller = Uuid::new_v4();
    static TIME: AtomicU64 = AtomicU64::new(0);
    TIME.store(0, Ordering::Release);
    let actor = Admission::open_with_test_time(
        db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            request_ttl: Duration::from_millis(50),
            ..Default::default()
        },
        move |id| id == controller,
        &TIME,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let device = DeviceId::new("curtain").unwrap();
    let ids = actor
        .submit_motor(
            controller,
            device.clone(),
            homesense_core::MotorAction::Open,
        )
        .await
        .unwrap();
    TIME.store(80, Ordering::Release);
    let s = view(&actor, controller).await;
    assert_eq!(s.pending, None);
    assert_eq!(s.last_written_id, None);
    assert!(actor.dispatch().await.unwrap().is_none());
    assert_eq!(
        homesense_storage::Store::open(&db)
            .unwrap()
            .command_status(actor.boot_id(), ids[0])
            .unwrap(),
        Some(homesense_storage::journal::Status::Cancelled)
    );
    actor
        .submit_motor(controller, device, homesense_core::MotorAction::Stop)
        .await
        .unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    actor.finish_write(ticket.command.id, true).await.unwrap();
    assert_eq!(
        view(&actor, controller)
            .await
            .last_written_command
            .as_deref(),
        Some("Stop")
    );
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn finish_before_deadline_is_written_but_at_deadline_is_uncertain_without_replay() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    for completed_at in [49, 50] {
        TIME.store(0, Ordering::Release);
        let root = std::env::temp_dir().join(format!("curtain-write-boundary-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut d = document();
        d["devices"][0]["curtain_state"]["leds"] = serde_json::json!({});
        let db = root.join("runtime.sqlite");
        homesense_storage::Store::open(&db).unwrap().publish_configuration(
            None, &serde_json::to_vec(&d).unwrap(), ExecutionContext::Simulation, &[],
        ).unwrap();
        let controller = Uuid::new_v4();
        let actor = Admission::open_with_test_time(
            db.clone(),
            Options { context: ExecutionContext::Simulation, request_ttl: Duration::from_millis(50), ..Default::default() },
            move |id| id == controller,
            &TIME,
        ).await.unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let ids = actor.submit_motor(controller, DeviceId::new("curtain").unwrap(), homesense_core::MotorAction::Stop).await.unwrap();
        let ticket = actor.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.id, ids[0]);
        assert_eq!(ticket.command.deadline_ms, 50);
        assert_eq!(homesense_storage::Store::open(&db).unwrap().command_status(actor.boot_id(), ids[0]).unwrap(), Some(homesense_storage::journal::Status::Writing));
        TIME.store(completed_at, Ordering::Release);
        let result = actor.finish_write(ticket.command.id, true).await;
        let status = homesense_storage::Store::open(&db).unwrap().command_status(actor.boot_id(), ids[0]).unwrap();
        if completed_at == 49 {
            assert_eq!(result, Ok(()));
            assert_eq!(status, Some(homesense_storage::journal::Status::Written));
            assert_eq!(view(&actor, controller).await.last_written_command.as_deref(), Some("Stop"));
            assert!(actor.dispatch().await.unwrap().is_none());
        } else {
            assert!(result.is_err(), "late complete-frame report must not turn uncertain into Written");
            assert_eq!(status, Some(homesense_storage::journal::Status::Uncertain));
            assert!(actor.task_generation().await.is_err());
            assert!(actor.dispatch().await.is_err(), "expired in-flight STOP must never be replayed");
            let sql = rusqlite::Connection::open(&db).unwrap();
            assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
