use super::*;
use crate::hap_test_support as support;
#[tokio::test]
async fn selected_lights_publish_as_lightbulbs_on_independent_bridges() {
    let root=std::env::temp_dir().join(format!("homesense-hap-subset-{}",Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();
    let mut raw:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["devices"][0]["name"]="First selected light".into();
    let mut second=raw["devices"][0].clone();second["id"]="second".into();second["name"]="Second selected light".into();second["channel"]=2.into();raw["devices"].as_array_mut().unwrap().push(second);
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let mut store=homesense_storage::Store::open(root.join("runtime.sqlite")).unwrap();
    store.publish_automation(None,&serde_json::to_vec(&raw).unwrap(),rules,homesense_core::ExecutionContext::Simulation,&[]).unwrap();drop(store);
    let admission=Admission::open(root.join("runtime.sqlite"),crate::admission::Options{context:homesense_core::ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
    let controller=Uuid::new_v4();
    let mut runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,1_700_000_000_000,1000,64,Default::default(),crate::automation_runtime::StartupPolicy::Suppress).await.unwrap();
    let config=homesense_config::Configuration::parse(&serde_json::to_vec(&raw).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
    let mut physical=crate::feedback::Feedback::new(&config,homesense_core::ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap().snapshot();
    for state in physical.devices.values_mut() {state.quality=homesense_state::Quality::Confirmed;state.observed=Some(homesense_core::Observation::Level(homesense_core::Percent::new(30).unwrap()));state.observed_at_ms=Some(admission.monotonic_ms());}
    let mut endpoints=Vec::new();let mut identities=std::collections::BTreeSet::new();
    for (index,id) in ["simulation:light","second"].into_iter().enumerate() {
        let address=crate::automation_tcp::fault_tests::free_address();
        let settings=Settings{address,scenes:vec![],motors:vec![]};
        let directory=root.join(format!("bridge-{index}"));std::fs::create_dir(&directory).unwrap();
        let mut identity=settings.identity_named(&directory,&format!("Floor {index}")).await.unwrap();
        let config=identity.load_config().await.unwrap();let accessory_id=config.device_id.to_string();let accessory_key=config.device_ed25519_keypair.verifying_key();assert!(identities.insert(accessory_id.clone()));
        identity.initialize_aid_cache().await.unwrap();
        // Seed a valid graph before importing a synthetic pairing, as a real
        // accessory allocates persistent IDs before its first pairing.
        homesense_hap::graph::LightGraph::build(&identity,"HomeSense",vec![homesense_hap::graph::LightSpec{device:homesense_core::DeviceId::new(id).unwrap(),name:if index==0 {"First selected light".into()}else{"Second selected light".into()},dimmable:true,default_on_level:None}]).await.unwrap();
        let signing=ed25519_dalek::SigningKey::from_bytes(&[7+index as u8;32]);
        identity.save_pairing(&hap::pairing::Pairing::new(controller,hap::pairing::Permissions::Admin,signing.verifying_key().to_bytes())).await.unwrap();
        let endpoint=Endpoint::start_selected(&settings,&identity,&admission,controller,&runtime,physical.clone(),false,Some([homesense_core::DeviceId::new(id).unwrap()].into())).await.unwrap().unwrap();
        endpoints.push(endpoint);
        tokio::task::spawn_blocking(move || {
            let mut socket=std::net::TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();socket.set_write_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut session=support::verify(&mut socket,controller,&signing,&accessory_id,&accessory_key);
            let (headers,body)=session.request(&mut socket,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(headers.starts_with("HTTP/1.1 200"),"{headers} {body:?}");
            let graph:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(graph["accessories"].as_array().unwrap().len(),2);
            let text=String::from_utf8(body).unwrap();assert!(text.contains(&format!("Floor {index}")));assert!(text.contains(if index==0 {"First selected light"}else{"Second selected light"}));assert!(!text.contains(if index==0 {"Second selected light"}else{"First selected light"}));
            assert!(graph["accessories"][1]["services"].as_array().unwrap().iter().any(|service|service["type"]=="43"));
        }).await.unwrap();
    }
    for endpoint in endpoints {endpoint.shutdown().await;}
    assert!(runtime.shutdown().await.iter().all(|(_,result)|result.is_ok()));drop(runtime);admission.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reviewed_composed_light_is_a_lightbulb_without_raw_resources() {
    let root=std::env::temp_dir().join(format!("homesense-hap-composed-{}",Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();
    let raw=serde_json::json!({"schema_version":2,"revision":1,"rooms":[{"id":"room","name":"Room"}],
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"dimmer","address":"32","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],
        "devices":[
            {"id":"supply","name":"Raw supply","room":"room","module_id":"relay","channel":1,"enabled":true},
            {"id":"composed","name":"Raw dimmer","room":"room","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100,"catalog_visible":false},
            {"id":"ordinary","name":"Ordinary lamp","room":"room","module_id":"dimmer","channel":2,"enabled":true,"default_on_level":100}],
        "composed_lights":[{"id":"matrix","review_evidence":"synthetic reviewed wiring","lights":[
            {"id":"composed","name":"Reviewed lamp","members":[{"supply":"supply","dimmer":"composed"}],"default_on_level":70}]}]});
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let mut store=homesense_storage::Store::open(root.join("runtime.sqlite")).unwrap();
    store.publish_automation(None,&serde_json::to_vec(&raw).unwrap(),rules,homesense_core::ExecutionContext::Simulation,&[]).unwrap();drop(store);
    let admission=Admission::open(root.join("runtime.sqlite"),crate::admission::Options{context:homesense_core::ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
    let controller=Uuid::new_v4();
    let runtime=AutomationRuntime::load_with_startup(admission.clone(),controller,1_700_000_000_000,1000,64,Default::default(),crate::automation_runtime::StartupPolicy::Suppress).await.unwrap();
    let config=homesense_config::Configuration::parse(&serde_json::to_vec(&raw).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
    let mut physical=crate::feedback::Feedback::new(&config,homesense_core::ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap().snapshot();
    for (id,state) in &mut physical.devices {
        state.quality=homesense_state::Quality::Confirmed;
        state.observed=Some(if id.as_str()=="supply" {homesense_core::Observation::Switch(true)}else{homesense_core::Observation::Level(homesense_core::Percent::new(30).unwrap())});
        state.observed_at_ms=Some(admission.monotonic_ms());
    }
    let address=crate::automation_tcp::fault_tests::free_address();
    let settings=Settings{address,scenes:vec![],motors:vec![]};
    let mut identity=settings.identity(&root).await.unwrap();
    let saved=identity.load_config().await.unwrap();
    identity.initialize_aid_cache().await.unwrap();
    let selected=crate::hap_bridge_plan::all_light_ids(&config);
    homesense_hap::graph::LightGraph::build(&identity,"HomeSense",vec![
        homesense_hap::graph::LightSpec{device:homesense_core::DeviceId::new("composed").unwrap(),name:"Reviewed lamp".into(),dimmable:true,default_on_level:Some(homesense_core::Percent::new(70).unwrap())},
        homesense_hap::graph::LightSpec{device:homesense_core::DeviceId::new("ordinary").unwrap(),name:"Ordinary lamp".into(),dimmable:true,default_on_level:Some(homesense_core::Percent::new(100).unwrap())},
    ]).await.unwrap();
    let signing=ed25519_dalek::SigningKey::from_bytes(&[91;32]);
    identity.save_pairing(&hap::pairing::Pairing::new(controller,hap::pairing::Permissions::Admin,signing.verifying_key().to_bytes())).await.unwrap();
    let endpoint=Endpoint::start_selected(&settings,&identity,&admission,controller,&runtime,physical,false,Some(selected)).await.unwrap().unwrap();
    tokio::task::spawn_blocking(move || {
        let mut socket=std::net::TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();socket.set_write_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        let mut session=support::verify(&mut socket,controller,&signing,&saved.device_id.to_string(),&saved.device_ed25519_keypair.verifying_key());
        let (headers,body)=session.request(&mut socket,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(headers.starts_with("HTTP/1.1 200"),"{headers} {body:?}");
        let graph:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(graph["accessories"].as_array().unwrap().len(),3);
        let text=String::from_utf8(body).unwrap();assert!(text.contains("Reviewed lamp"));assert!(text.contains("Ordinary lamp"));assert!(!text.contains("Raw supply"));assert!(!text.contains("Raw dimmer"));
        assert_eq!(graph["accessories"].as_array().unwrap().iter().flat_map(|a|a["services"].as_array().unwrap()).filter(|service|service["type"]=="43").count(),2);
    }).await.unwrap();
    endpoint.shutdown().await;
    drop(runtime);admission.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
}
