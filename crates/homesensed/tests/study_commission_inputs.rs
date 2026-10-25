#[path = "../../test-support/cli.rs"]
mod test_cli;

#[test]
fn legacy_invocation_has_no_embedded_configuration_fallback() {
    let output = std::process::Command::new(test_cli::binary(env!("CARGO_BIN_EXE_study-scene")))
        .args(["unused-state", "1", "--coexistence-confirmed"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DEVICES_JSON SCENES_JSON"));
}

#[test]
fn explicit_configuration_must_be_read_before_any_network_operation() {
    let missing = std::env::temp_dir().join(format!("missing-study-{}", uuid::Uuid::new_v4()));
    let output = std::process::Command::new(test_cli::binary(env!("CARGO_BIN_EXE_study-scene")))
        .arg(&missing)
        .arg(&missing)
        .arg(missing.join("state"))
        .args(["1", "--coexistence-confirmed"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("read device configuration"));
    assert!(!missing.exists());
}
