use homesense_core::ExecutionContext;
use homesensed::{
    admission::{Admission, Options},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
    web_api::Api,
    web_auth::{Grant, Role, Tokens},
    web_receipts::Receipts,
};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(bytes.len()) // In-memory simulation sink; no gateway connection.
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn root_same_key_is_independent_across_authenticated_web_endpoints() {
    let root = std::env::temp_dir().join(format!("web-runtime-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut definition:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    definition["rooms"]=serde_json::json!([{"id":"visible","name":"Visible room","floor":"Test floor"},{"id":"hidden","name":"Unassigned room"}]);
    definition["devices"][0]["room"]=serde_json::json!("visible");

    definition["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    for (id,channel) in [("z:motor",2),("z:uncalibrated",3)] {
        let mut device=serde_json::json!({"id":id,"name":"Curtain","gateway":"simulation","address":"22","channel":channel,"enabled":true});
        if channel==2 {device["motor_calibration"]=serde_json::json!({"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1});}
        definition["devices"].as_array_mut().unwrap().push(device);
    }
    let device_bytes=serde_json::to_vec(&definition).unwrap();let devices=device_bytes.as_slice();
    let writes=Arc::new(Mutex::new(vec![]));
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"light","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    let path = root.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(None, devices, rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let caller = Uuid::new_v4();
    let token_id = Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == caller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let mut runtime = AutomationRuntime::load(
        admission.clone(),
        caller,
        1_800_000_000_000,
        1000,
        8,
        Default::default(),
    )
    .await
    .unwrap();
    let control = runtime.control();
    let config =
        homesense_config::Configuration::parse(devices, ExecutionContext::Simulation).unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    // An unobserved dimmer without a default must never invent 100% on power-on.
    let device = homesense_core::DeviceId::new("simulation:light").unwrap();
    let generation = admission.task_generation().await.unwrap();
    assert!(
        homesensed::management_devices::prepare(
            &admission,
            caller,
            generation.clone(),
            device.clone(),
            homesensed::management_devices::DeviceAction::Power { on: true },
            &feedback,
            1000
        )
        .await
        .is_err()
    );
    assert!(
        homesensed::management_devices::prepare(
            &admission,
            Uuid::new_v4(),
            generation.clone(),
            device.clone(),
            homesensed::management_devices::DeviceAction::Brightness { level: 40 },
            &feedback,
            1000
        )
        .await
        .is_err()
    );
    let batch = homesensed::management_devices::prepare(
        &admission,
        caller,
        generation,
        device,
        homesensed::management_devices::DeviceAction::Brightness { level: 40 },
        &feedback,
        1000,
    )
    .await
    .unwrap();
    assert_eq!(batch.controller, caller);
    assert_eq!(batch.intents.len(), 1);
    assert!(
        matches!(batch.intents[0].command,homesense_core::Command::SetLevel{level,..} if level.get()==40)
    );
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            admission.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(Wire(writes.clone()), 1),
            )]
            .into(),
        ),
        feedback,
    );
    let tokens = Arc::new(Tokens::default());
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            token_id,
            secret,
            Grant {
                controller: caller,
                role: Role::Member,
                scenes: ["light".into()].into(),
                devices: ["simulation:light".into(),"z:motor".into(),"z:uncalibrated".into()].into(),
            },
        )
        .unwrap();
    let api = Api {
        daily_backup:Default::default(),
        hap_placement:Default::default(),hap_bridges:Default::default(),hap_status: Default::default(),
        backup_source:None,
        publisher:None,
        origin: "http://127.0.0.1:8088".into(),
        tokens: tokens.clone(),
        runtime: std::sync::RwLock::new(Some(homesensed::web_api::Runtime {
            control: control.clone(),
            revision: 1,
            boot: admission.boot_id().to_owned(),
            scenes: [("light".into(), "Test".into())].into(),
        })),
        drafts: Some((
            ExecutionContext::Simulation,
            Mutex::new(homesensed::web_drafts::Drafts::open(&root.join("drafts.sqlite")).unwrap()),
        )),
        receipts: Mutex::new(Receipts::open(&root.join("web.sqlite")).unwrap()),
    };
    let (stop, receive) = tokio::sync::watch::channel(false);
    let client = async {
        let make = |endpoint:&str,body:&str| Request::builder().method("POST").uri(endpoint)
            .header("host","127.0.0.1:8088").header("origin",&api.origin)
            .header("authorization",format!("Bearer {secret}"))
            .header("content-type","application/json").body(Bytes::copy_from_slice(body.as_bytes())).unwrap();
        let body=r#"{"device":"simulation:light","request_id":"shared-endpoint-key","command":{"action":"brightness","level":40}}"#;
        let first=api.handle(make("/api/v1/device-commands",body)).await;
        let first_status=first.status();let first_json=first.into_body().collect().await.unwrap().to_bytes();
        let repeat=api.handle(make("/api/v1/device-commands",body)).await;
        let repeat_status=repeat.status();let repeat_json=repeat.into_body().collect().await.unwrap().to_bytes();
        let scene=api.handle(make("/api/v1/scene-actions",r#"{"scene":"light","request_id":"shared-endpoint-key"}"#)).await;
        let scene_status=scene.status();let scene_json=scene.into_body().collect().await.unwrap().to_bytes();
        stop.send(true).unwrap();
        (first_status,first_json,repeat_status,repeat_json,scene_status,scene_json)
    };
    let (drive,result)=tokio::join!(runtime.drive(&mut owner,Duration::from_millis(1),1000,receive,||Ok(1_800_000_000_000)),client);
    drive.unwrap();
    let scene_runs=rusqlite::Connection::open(&path).unwrap().query_row("select count(*) from scene_run",[],|r|r.get::<_,i64>(0)).unwrap();
    drop(api);admission.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
    assert_eq!(result.0,StatusCode::ACCEPTED,"first authenticated device action failed");
    assert_eq!(result.2,StatusCode::OK,"same endpoint/key must reuse receipt");
    assert_eq!(result.1,result.3,"same endpoint must return the exact old execution record");
    assert_eq!(result.4,StatusCode::ACCEPTED,"different endpoint must own an independent key scope; actual body {}",String::from_utf8_lossy(&result.5));
    assert_eq!(scene_runs,1,"accepted scene endpoint must create actual target run");
}
