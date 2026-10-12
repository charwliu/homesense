use homesense_config::Configuration;
use homesense_core::ExecutionContext;
use homesensed::configuration_preview::device_changes;
fn parse(value:&serde_json::Value)->Configuration {
    Configuration::parse(&serde_json::to_vec(value).unwrap(),ExecutionContext::Simulation).unwrap()
}
#[test]
fn publication_preview_explains_device_binding_and_calibration_changes() {
    let original:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    let current=parse(&original);
    assert!(device_changes(&current,&current).is_empty());
    for (field,value,reason) in [
        ("name",serde_json::json!("Renamed"),"metadata"),
        ("enabled",serde_json::json!(false),"disabled"),
        ("channel",serde_json::json!(2),"binding"),
        ("default_on_level",serde_json::json!(40),"default_on_level"),
    ] {
        let mut next=original.clone();next["devices"][0][field]=value;
        let changes=device_changes(&current,&parse(&next));assert_eq!(changes.len(),1);assert_eq!(changes[0].device,"simulation:light");assert_eq!(changes[0].changes,vec![reason]);
    }
    let mut added=original.clone();let mut device=added["devices"][0].clone();device["id"]="simulation:second".into();device["channel"]=2.into();added["devices"].as_array_mut().unwrap().push(device);
    assert_eq!(device_changes(&current,&parse(&added))[0].changes,vec!["added"]);
    assert_eq!(device_changes(&parse(&added),&current)[0].changes,vec!["removed"]);
    let mut endpoint=original.clone();endpoint["gateways"][0]["endpoint"]="127.0.0.1:50008".into();
    assert_eq!(device_changes(&current,&parse(&endpoint))[0].changes,vec!["gateway_or_module"]);
    let mut motor=serde_json::json!({"schema_version":1,"revision":1,"gateways":[{"id":"test","endpoint":"127.0.0.1:50007","modules":[{"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Synthetic","gateway":"test","address":"22","channel":2,"enabled":true}]});
    let uncalibrated=parse(&motor);
    motor["devices"][0]["motor_calibration"]=serde_json::json!({"open_direction":"up","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1});
    let calibrated=parse(&motor);
    assert_eq!(device_changes(&uncalibrated,&calibrated)[0].changes,vec!["motor_calibration"]);
    assert_eq!(device_changes(&calibrated,&uncalibrated)[0].changes,vec!["motor_calibration"]);
    motor["devices"][0]["motor_calibration"]["open_direction"]="down".into();
    assert_eq!(device_changes(&calibrated,&parse(&motor))[0].changes,vec!["motor_calibration"]);
}
