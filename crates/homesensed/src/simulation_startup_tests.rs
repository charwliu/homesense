//! Actual simulator input must initialize lights without inventing motor position.
use super::*;
fn documents()->(Vec<u8>,Vec<u8>) {
    let config=serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
        {"id":"relay","address":"20","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
        {"id":"dimmer","address":"21","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
        {"id":"motor","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}
    ]}],"devices":[
        {"id":"relay","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true},
        {"id":"dimmer","name":"Synthetic dimmer","module_id":"dimmer","channel":1,"enabled":true},
        {"id":"motor","name":"Synthetic motor","module_id":"motor","channel":1,"enabled":true}
    ]});
    let rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    (serde_json::to_vec(&config).unwrap(),serde_json::to_vec(&rules).unwrap())
}
#[tokio::test]
async fn simulator_unknown_motor_does_not_block_service_startup_or_fabricate_position() {
    let (devices,rules)=documents();
    let root=std::env::temp_dir().join(format!("simulation-startup-{}",Uuid::new_v4()));
    tokio::time::timeout(Duration::from_secs(3),serve_automation_simulation(&devices,&rules,&root,Duration::from_millis(40),None)).await.unwrap().unwrap();
    let journal=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(journal.query_row("SELECT count(*) FROM command_journal",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    drop(journal);std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn actual_simulator_feedback_keeps_motor_unknown_and_missing_light_feedback_unready() {
    let (devices,rules)=documents();let config=homesense_config::Configuration::parse(&devices,ExecutionContext::Simulation).unwrap();
    for missing in [None,Some(0x20),Some(0x21)] {
        let root=std::env::temp_dir().join(format!("simulation-startup-gate-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("runtime.sqlite");
        Store::open(&path).unwrap().publish_automation(None,&devices,&rules,ExecutionContext::Simulation,&[]).unwrap();
        let admission=Admission::open(path,Options{context:ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
        let gateway=&config.gateways()["bus"];
        let queries=["relay","dimmer"].into_iter().filter_map(|id|{
            let (device,module)=config.binding(&homesense_core::DeviceId::new(id).unwrap()).unwrap();
            (missing!=Some(module.address().get())).then(||hb_protocol::encode_in_context(module,device.channel,homesense_core::Command::Query,ExecutionContext::Simulation).unwrap().bytes())
        }).collect();
        let stream=SimulatedStream {gateway:Gateway::new(gateway.modules().values().cloned().collect(),Faults::default(),128).unwrap(),clock:Instant::now(),pending:VecDeque::new(),queries,next_query_ms:0};
        let connections=[("bus".into(),hb_transport::Connection::for_simulation(stream,1))].into();
        let feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap();
        let mut owner=ObservedTransport::new(TransportOwner::new(admission.clone(),connections),feedback);
        assert!(!simulation_initial_feedback_ready(&owner.snapshot()));
        for _ in 0..4 {owner.receive("bus").await.unwrap();}
        let snapshot=owner.snapshot();let motor=&snapshot.devices[&homesense_core::DeviceId::new("motor").unwrap()];
        assert_eq!(motor.quality,homesense_state::Quality::Unknown);assert_eq!(motor.observed,None);assert_eq!(motor.observed_at_ms,None);
        assert_eq!(simulation_initial_feedback_ready(&snapshot),missing.is_none(),"a missing switch OR dimmer must still prevent startup");
        for (address,id) in [(0x20,"relay"),(0x21,"dimmer")] {
            assert_eq!(snapshot.devices[&homesense_core::DeviceId::new(id).unwrap()].quality,if missing==Some(address){homesense_state::Quality::Unknown}else{homesense_state::Quality::Confirmed});
        }
        admission.shutdown().await.unwrap();drop(owner);std::fs::remove_dir_all(root).unwrap();
    }
}
