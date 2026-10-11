use hap::{server::WriteStatus, storage::Storage};
use homesense_core::{Command, DeviceId, ExecutionContext, MotorMotion};
use homesense_hap::{identifiers::MotorOperation, motor_graph::MotorSpec, storage::IdentityStore};
use homesensed::{
    admission::{Admission, Options},
    runtime_motor_hap::RuntimeMotorHap,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;
#[tokio::test]
async fn motor_adapter_authorizes_resets_and_reserves_capacity_before_admission() {
    let root = std::env::temp_dir().join(format!("motor-hap-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let document = serde_json::json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Curtain","gateway":"bus","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}]});
    let bytes = serde_json::to_vec(&document).unwrap();
    let config =
        homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
    let db = root.join("runtime.sqlite");
    homesense_storage::Store::open(&db)
        .unwrap()
        .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        db.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let mut uncalibrated = document.clone();
    uncalibrated["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("motor_calibration");
    let invalid = homesense_config::Configuration::parse(
        &serde_json::to_vec(&uncalibrated).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        RuntimeMotorHap::new(
            &invalid,
            &identity,
            admission.clone(),
            vec![MotorSpec {
                device: DeviceId::new("motor").unwrap(),
                name: "Curtain".into()
            }],
            |_, _| true
        )
        .await
        .is_err()
    );
    let allowed = Arc::new(AtomicBool::new(true));
    let auth = allowed.clone();
    let mut graph = RuntimeMotorHap::new(
        &config,
        &identity,
        admission.clone(),
        vec![MotorSpec {
            device: DeviceId::new("motor").unwrap(),
            name: "Curtain".into(),
        }],
        move |_, _| auth.load(Ordering::SeqCst),
    )
    .await
    .unwrap();
    let body = |operation, value| {
        let b = &graph
            .accessories
            .iter()
            .find(|(b, _)| b.operation == operation)
            .unwrap()
            .0;
        serde_json::to_vec(
            &serde_json::json!({"characteristics":[{"aid":b.aid,"iid":b.on_iid,"value":value}]}),
        )
        .unwrap()
    };
    let open = body(MotorOperation::Open, true);
    let reset = body(MotorOperation::Open, false);
    let stop = body(MotorOperation::Stop, true);
    assert_eq!(
        graph.write_handler.write_async(Uuid::new_v4(), &open).await,
        Err(WriteStatus::InsufficientPrivileges)
    );
    allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        graph.write_handler.write_async(controller, &reset).await,
        Err(WriteStatus::InsufficientPrivileges)
    );
    allowed.store(true, Ordering::SeqCst);
    graph
        .write_handler
        .write_async(controller, &reset)
        .await
        .unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    assert_eq!(graph.flush_resets().await.unwrap(), 0);
    graph
        .write_handler
        .write_async(controller, &open)
        .await
        .unwrap();
    let ticket = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.command, Command::Motor(MotorMotion::Down));
    admission
        .finish_write(ticket.command.id, true)
        .await
        .unwrap();
    assert_eq!(graph.flush_resets().await.unwrap(), 1);
    assert_eq!(graph.flush_resets().await.unwrap(), 0);
    // Drain accepted commands while leaving virtual acknowledgments queued.
    for _ in 0..32 {
        graph
            .write_handler
            .write_async(controller, &stop)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(21)).await;
        let ticket = admission.dispatch().await.unwrap().unwrap();
        admission
            .finish_write(ticket.command.id, true)
            .await
            .unwrap();
    }
    let sql = rusqlite::Connection::open(&db).unwrap();
    let before: i64 = sql
        .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        graph.write_handler.write_async(controller, &open).await,
        Err(WriteStatus::ResourceBusy)
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(graph.flush_resets().await.unwrap(), 32);
    drop(sql);
    admission.shutdown().await.unwrap();
    drop(graph);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
