use std::process::Command;

#[test]
fn demo_observes_requested_levels_only_after_gateway_feedback() {
    for level in [0, 30, 73, 100] {
        let output = Command::new(env!("CARGO_BIN_EXE_homesensed"))
            .args(["demo", &level.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let written = stdout
            .find("Written: observed=None, quality=Unknown")
            .unwrap();
        let feedback = stdout
            .find(&format!(
                "Feedback: observed=Some(Level(Percent({level}))), quality=Confirmed, pending=None"
            ))
            .unwrap();
        assert!(written < feedback);
    }
}

#[test]
fn invalid_levels_fail_before_any_command_is_queued() {
    for level in ["-1", "101", "256", "NaN", "1.5"] {
        let output = Command::new(env!("CARGO_BIN_EXE_homesensed"))
            .args(["demo", level])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn check_config_requires_explicit_simulation_for_sample() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/simulation.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_homesensed"));
    command.arg("check-config").arg(&path);
    assert!(!command.output().unwrap().status.success());
    let output = command.arg("--simulation").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("enabled=1, context=Simulation; no connection opened")
    );
}
