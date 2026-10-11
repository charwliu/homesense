//! Synthetic input only: no household project data is needed for boundary tests.
#[test]
fn rejects_oversized_and_invalid_bindings_without_partial_output() {
    let dir = std::env::temp_dir().join(format!(
        "event-cli-limits-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let event = dir.join("event.xml");
    let bindings = dir.join("bindings.json");
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_compile-event"))
            .arg(&event)
            .arg(&bindings)
            .args(["100", "100"])
            .output()
            .unwrap()
    };
    let reject = |expected: &str| {
        let output = run();
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "failed conversion emitted a partial draft"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    std::fs::write(&bindings, "{}").unwrap();
    std::fs::write(&event, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    reject("input exceeds size limit");
    std::fs::write(&event, "<event/>").unwrap();
    std::fs::write(&bindings, vec![b' '; 1024 * 1024 + 1]).unwrap();
    reject("input exceeds size limit");
    std::fs::write(
        &bindings,
        r#"{"1":{"device":"simulation:light","kind":"unknown"}}"#,
    )
    .unwrap();
    reject("unknown binding kind");
    std::fs::write(
        &bindings,
        r#"{"1":{"device":"simulation:light","kind":"dimmer","enable":true}}"#,
    )
    .unwrap();
    reject("unknown field");
    std::fs::write(&event, [0xff]).unwrap();
    reject("UTF-8");
    std::fs::remove_dir_all(dir).unwrap();
}
