#[path = "../../test-support/cli.rs"]
mod test_cli;
#[test]
fn cli_keeps_fade_reference_and_definition_disabled_and_rejects_invalid_input() {
    let dir=std::env::temp_dir().join(format!("event-cli-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    let xml = include_str!("fixtures/synthetic-ramp.xml").to_owned();
    let event=dir.join("event.xml");let bindings=dir.join("bindings.json");
    std::fs::write(&event,&xml).unwrap();
    std::fs::write(&bindings,r#"{"1":{"device":"simulation:light","kind":"dimmer"}}"#).unwrap();
    let run=||std::process::Command::new(test_cli::binary(env!("CARGO_BIN_EXE_compile-event"))).arg(&event).arg(&bindings).args(["100","100"]).output().unwrap();
    let output=run();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let draft:serde_json::Value=serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(draft["automatic_execution"],false);
    assert_eq!(draft["enabled"],false);
    assert_eq!(draft["actions"][0]["action"]["id"],draft["timed_scenes"][0]["id"]);
    assert_eq!(draft["timed_scenes"][0]["enabled"],false);
    assert_eq!(draft["timed_scenes"][0]["steps"][0]["fade"]["duration_ms"],1000);
    std::fs::write(&event,xml.replace("<static>1000</static>","<variable>1000</variable>")).unwrap();
    let output=run();assert!(!output.status.success());assert!(output.stdout.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
