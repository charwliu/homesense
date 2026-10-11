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
        let mut command = Command::new(env!("CARGO_BIN_EXE_homesensed"));
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
