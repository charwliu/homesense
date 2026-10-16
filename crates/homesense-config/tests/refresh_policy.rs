use homesense_config::{Configuration, RefreshPolicy};
use homesense_core::ExecutionContext;
fn input() -> serde_json::Value {
    serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap()
}
#[test]
fn adaptive_defaults_and_explicit_long_idle_are_validated() {
    let mut v = input();
    let c = Configuration::parse(
        &serde_json::to_vec(&v).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(c.refresh_policy(), RefreshPolicy::default());
    assert_eq!(c.refresh_policy().idle_interval_ms, 10_800_000);
    let p = RefreshPolicy {
        idle_interval_ms: 43_200_000,
        ..Default::default()
    };
    v["refresh_policy"] = serde_json::to_value(p).unwrap();
    let c = Configuration::parse(
        &serde_json::to_vec(&v).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(c.refresh_policy().observation_max_age_ms(), 43_215_000);
    for (field, value) in [
        ("idle_interval_ms", 0),
        ("idle_interval_ms", 86_400_001),
        ("active_interval_ms", 60_000),
        ("quiet_rounds_per_step", 0),
        ("round_budget_ms", 0),
        ("observation_grace_ms", 0),
    ] {
        let mut invalid = v.clone();
        invalid["refresh_policy"][field] = value.into();
        assert!(
            Configuration::parse(
                &serde_json::to_vec(&invalid).unwrap(),
                ExecutionContext::Simulation
            )
            .is_err(),
            "{field}"
        );
    }
    v["refresh_policy"]["unexpected"] = true.into();
    assert!(
        Configuration::parse(
            &serde_json::to_vec(&v).unwrap(),
            ExecutionContext::Simulation
        )
        .is_err()
    );
}
