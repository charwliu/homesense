#[path = "../../test-support/cli.rs"]
mod test_cli;
use std::process::Command;
#[test]
fn offline_cli_checks_disabled_example_and_enabled_scene_without_creating_state() {
    let directory = std::env::temp_dir().join(format!("homesense-check-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let devices = directory.join("devices.json");
    let automation = directory.join("automation.json");
    std::fs::write(&devices, include_bytes!("../../../config/simulation.json")).unwrap();
    let mut input: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/automation.simulation.json"))
            .unwrap();
    let run = |simulation: bool| {
        let mut command = Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed")));
        command
            .current_dir(&directory)
            .arg("check-automation")
            .arg(&devices)
            .arg(&automation);
        if simulation {
            command.arg("--simulation");
        }
        command.output().unwrap()
    };
    std::fs::write(&automation, hap::serde_json::to_vec(&input).unwrap()).unwrap();
    let result = run(true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("enabled=0"));
    input["rules"][0]["enabled"] = true.into();
    input["rules"][0]["review_evidence"] = "offline test".into();
    std::fs::write(&automation, hap::serde_json::to_vec(&input).unwrap()).unwrap();
    let result = run(true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("enabled=1"));
    assert!(!run(false).status.success());
    let mut device_input: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    device_input["devices"][0]["enabled"] = false.into();
    std::fs::write(&devices, hap::serde_json::to_vec(&device_input).unwrap()).unwrap();
    let result = run(true);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("disabled output"));
    input["rules"][0]["enabled"] = false.into();
    std::fs::write(&automation, hap::serde_json::to_vec(&input).unwrap()).unwrap();
    assert!(run(true).status.success());
    input["rules"][0]["enabled"] = true.into();
    std::fs::write(&devices, include_bytes!("../../../config/simulation.json")).unwrap();
    input["rules"][0]["actions"][0]["action"]["id"] = "missing".into();
    std::fs::write(&automation, hap::serde_json::to_vec(&input).unwrap()).unwrap();
    assert!(!run(true).status.success());
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn offline_cli_statistics_use_the_validated_deferred_linker_without_weakening_rejections() {
    use hap::serde_json::{json,to_vec,Value};
    let directory=std::env::temp_dir().join(format!("homesense-check-deferred-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
    let bus=std::net::TcpListener::bind("127.0.0.1:0").unwrap();bus.set_nonblocking(true).unwrap();
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","verified_at_unix_ms":1,"evidence":"Generated offline CLI fixture, not physical calibration"});
    let input=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"boot","enabled":true,"review_evidence":"Generated offline only","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"scene","id":"delayed","activate":true}}]}],"timed_scenes":[{"id":"delayed","enabled":true,"review_evidence":"Generated offline only","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":10000,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]});
    let run=|config:&Value,rules:&Value| {
        std::fs::write(directory.join("devices.json"),to_vec(config).unwrap()).unwrap();std::fs::write(directory.join("automation.json"),to_vec(rules).unwrap()).unwrap();
        let output=Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed"))).current_dir(&directory).arg("check-automation").arg(directory.join("devices.json")).arg(directory.join("automation.json")).output().unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(),2,"offline check must not create state");
        assert_eq!(bus.accept().unwrap_err().kind(),std::io::ErrorKind::WouldBlock,"offline check must not connect to the gateway");
        output
    };
    let expect=|output:std::process::Output,success:bool,label:&str| {
        assert_eq!(output.status.success(),success,"{label}: stdout={} stderr={}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));output
    };
    let output=expect(run(&devices,&input),true,"direct reviewed startup to timed");assert!(String::from_utf8_lossy(&output.stdout).contains("enabled=1"));
    let mut nested_timed=input.clone();nested_timed["immediate_scenes"]=json!([{"id":"wrapper","review_evidence":"Generated offline only","activate":[{"source":"fixture","action":{"kind":"scene","id":"delayed","activate":true}}]}]);nested_timed["rules"][0]["actions"][0]["action"]["id"]=json!("wrapper");
    let output=expect(run(&devices,&nested_timed),false,"immediate-to-timed remains explicitly unsupported");
    assert!(String::from_utf8_lossy(&output.stderr).contains("MissingScene"),"{}",String::from_utf8_lossy(&output.stderr));
    let mut nested=input.clone();nested["immediate_scenes"]=json!([
        {"id":"wrapper","review_evidence":"Generated offline only","activate":[{"source":"fixture","action":{"kind":"scene","id":"leaf","activate":true}}]},
        {"id":"leaf","review_evidence":"Generated offline only","activate":[{"source":"fixture","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}}]}
    ]);
    nested["rules"][0]["actions"].as_array_mut().unwrap().push(json!({"source":"fixture","action":{"kind":"scene","id":"wrapper","activate":true}}));
    expect(run(&devices,&nested),true,"legal mixed direct timed and nested immediate calls");
    let mut disabled=input.clone();disabled["timed_scenes"][0]["enabled"]=json!(false);expect(run(&devices,&disabled),false,"disabled timed");
    let mut unreviewed=input.clone();unreviewed["timed_scenes"][0]["review_evidence"]=json!("");expect(run(&devices,&unreviewed),false,"unreviewed timed");
    let mut missing=input.clone();missing["rules"][0]["actions"][0]["action"]["id"]=json!("missing");expect(run(&devices,&missing),false,"missing enabled reference");
    missing["rules"][0]["enabled"]=json!(false);let output=expect(run(&devices,&missing),true,"disabled unresolved rule held");assert!(String::from_utf8_lossy(&output.stdout).contains("enabled=0"));
    let mut invalid_device=input.clone();invalid_device["timed_scenes"][0]["steps"][0]["device"]=json!("missing");expect(run(&devices,&invalid_device),false,"invalid physical device");
    let mut unsupported=devices.clone();unsupported["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");expect(run(&unsupported,&input),false,"AD8 arbitraryImmediate encoding must still fail");
    nested["immediate_scenes"][0]["activate"][0]["action"]["id"]=json!("wrapper");expect(run(&devices,&nested),false,"cyclic immediate call");
    std::fs::remove_dir_all(directory).unwrap();
}
