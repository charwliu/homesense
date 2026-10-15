#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
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
    let mut document = serde_json::json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Curtain","gateway":"bus","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}]});
    document["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}));
    document["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"light","name":"Light","gateway":"bus","address":"15","channel":1,"enabled":true}));
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
    let reservation=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address=reservation.local_addr().unwrap();
    let settings=hap::Config{host:address.ip(),port:address.port(),..Default::default()};
    let accessory_id=settings.device_id.to_string();let accessory_key=settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
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
    let bindings=graph.accessories.iter().map(|(b,_)|b.clone()).collect::<Vec<_>>();
    let mut snapshot=homesensed::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap().snapshot();
    let observed=snapshot.devices.get_mut(&DeviceId::new("light").unwrap()).unwrap();observed.observed=Some(homesense_core::Observation::Level(homesense_core::Percent::new(40).unwrap()));observed.quality=homesense_state::Quality::Confirmed;observed.observed_at_ms=Some(0);
    let lights=homesensed::runtime_hap::RuntimeHap::new(&config,snapshot.clone(),&identity,admission.clone(),5000).await.unwrap();
    let mut unified=homesensed::unified_hap::UnifiedHap{lights,scenes:None,motors:Some(graph),away:None};
    let server=hap::server::IpServer::new_loopback(settings,identity.clone()).await.unwrap();
    unified.install(&server).await.unwrap();
    let signing=ed25519_dalek::SigningKey::from_bytes(&[31;32]);
    identity.save_pairing(&hap::pairing::Pairing::new(controller,hap::pairing::Permissions::Admin,signing.verifying_key().to_bytes())).await.unwrap();
    drop(reservation);
    let (ready,started)=tokio::sync::oneshot::channel();let active=server.clone();
    let listener=tokio::spawn(async move {active.run_handle_ready(ready).await});started.await.unwrap();
    let (stop,receive)=tokio::sync::watch::channel(false);let (snapshots,updates)=tokio::sync::watch::channel(snapshot.clone());
    let projection=tokio::spawn(async move {unified.project(updates,receive).await});
    let (steps,mut incoming)=tokio::sync::mpsc::channel(1);let (ack,acknowledged)=std::sync::mpsc::channel();
    let client=tokio::task::spawn_blocking(move || {
        let mut stream=std::net::TcpStream::connect(address).unwrap();stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let mut session=support::verify(&mut stream,controller,&signing,&accessory_id,&accessory_key);
        let (header,body)=session.request(&mut stream,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(header.starts_with("HTTP/1.1 200"),"{header} {body:?}");
        let value:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(value["accessories"].as_array().unwrap().len(),5);
        for operation in [MotorOperation::Open,MotorOperation::Close,MotorOperation::Stop] {
            let binding=bindings.iter().find(|b|b.operation==operation).unwrap();
            let body=serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]}).to_string();
            let request=format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
            let (header,body)=session.request(&mut stream,request.as_bytes());assert!(header.starts_with("HTTP/1.1 204"),"{header} {body:?}");
            steps.blocking_send(operation).unwrap();acknowledged.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            let request=format!("GET /characteristics?id={}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",binding.aid,binding.on_iid);
            let (_,body)=session.request(&mut stream,request.as_bytes());let value:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(value["characteristics"][0]["value"],false);
        }
    });
    for expected in [MotorMotion::Down,MotorMotion::Up,MotorMotion::Stop] {
        incoming.recv().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(21)).await;
        let ticket=admission.dispatch().await.unwrap().unwrap();assert_eq!(ticket.command.command,Command::Motor(expected));admission.finish_write(ticket.command.id,true).await.unwrap();
        snapshots.send_replace(snapshot.clone());ack.send(()).unwrap();
    }
    client.await.unwrap();stop.send(true).unwrap();projection.await.unwrap().unwrap();listener.abort();let _=listener.await;drop(server);
    admission.shutdown().await.unwrap();
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
