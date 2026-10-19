use serde_json::{Value,json};

/// Explicit read-only fault/fairness stimulus; never an actuator invocation.
/// Idle polling keeps the production 300s floor, so short bounded outage tests
/// declare their own observation dependency instead of relying on fast sweeps.
pub fn observation_rules(device:&str,max_age_ms:u64)->Value {
    json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],
        "timed_scenes":[{"id":"fault-observation","enabled":true,"review_evidence":"Synthetic read consumer only; never invoked","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":device,"command":{"kind":"level","value":42,"ramp":false}}]}],
        "scene_tracking":[{"scene":"fault-observation","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic fault observation","max_age_ms":max_age_ms,"review_evidence":"Explicit bounded read consumer; not an idle sweep override","members":[{"id":"light","device":device,"predicate":{"kind":"power","on":true},"review_evidence":"Synthetic single-port observation"}]}]})
}
