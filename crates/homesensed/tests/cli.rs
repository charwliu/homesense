#[path = "../../test-support/fixtures.rs"]
mod test_fixtures;
#[path = "../../test-support/cli.rs"]
mod test_cli;
use std::process::Command;

#[test]
fn demo_observes_requested_levels_only_after_gateway_feedback() {
    for level in [0, 30, 73, 100] {
        let output = Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed")))
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
        let output = Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed")))
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
    let fixtures = test_fixtures::Directory::new("homesense-cli-config");
    let path = fixtures.write("simulation.json", include_bytes!("../../../config/simulation.json"));
    let mut command = Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed")));
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

#[test]
fn bridge_plan_cli_requires_management_and_excludes_legacy_options() {
    let root=std::env::temp_dir().join(format!("homesense-bridge-cli-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();let plan=root.join("plan.json");std::fs::write(&plan,b"{}").unwrap();
    let state=root.join("state");
    for (extra,expected) in [(vec![],"Web TLS"),(vec!["--hap-listen","127.0.0.1:51826"],"cannot be combined"),(vec!["--hap-plan",plan.to_str().unwrap()],"duplicate")] {
        let output=Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed"))).args(["serve-automation-tcp","missing-device","missing-rules",state.to_str().unwrap(),"1","--coexistence-confirmed","--hap-plan",plan.to_str().unwrap()]).args(extra).output().unwrap();
        assert!(!output.status.success());assert!(String::from_utf8_lossy(&output.stderr).contains(expected),"{:?}",output);assert!(!state.exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}
