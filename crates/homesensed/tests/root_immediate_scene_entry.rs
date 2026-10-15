//! Public adapter construction over a synthetic, explicitly enabled definition.
//! No port, gateway, control request, or original household source is used.
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::ExecutionContext;
use homesense_hap::{scene_graph::SceneSpec, storage::IdentityStore};
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    automation_runtime::{AutomationRuntime, StartupPolicy},
    runtime_scene_hap::RuntimeSceneHap,
};
use serde_json::json;
use uuid::Uuid;

async fn construct(immediate: bool, enabled: bool) -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("root-immediate-entry-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let config_bytes = include_bytes!("../../../config/simulation.json");
    let config = Configuration::parse(config_bytes, ExecutionContext::Simulation).unwrap();
    let mut input = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    if immediate {
        input["immediate_scenes"] = json!([{"id":"synthetic-reviewed","enabled":enabled,"review_evidence":"root synthetic explicit immediate contract","activate":[{"source":"root/member/1","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":100,"ramp":false}}}]}]);
    } else {
        input["timed_scenes"] = json!([{"id":"synthetic-reviewed","enabled":enabled,"review_evidence":"root synthetic explicit timed contract","max_lateness_ms":1000,"steps":[{"source":"root/member/1","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":100,"ramp":false}}]}]);
    }
    let rules = serde_json::to_vec(&input).unwrap();
    let document = Document::parse(&rules, &config).unwrap();
    assert_eq!(document.immediate_scenes.contains_key("synthetic-reviewed"), immediate);
    Store::open(root.join("runtime.sqlite")).unwrap().publish_automation(
        None, config_bytes, &rules, ExecutionContext::Simulation, &[],
    ).unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(root.join("runtime.sqlite"), Options {
        context: ExecutionContext::Simulation, ..Options::default()
    }, move |id| id == controller).await.unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let runtime = AutomationRuntime::load_with_startup(admission.clone(), controller,
        1_800_000_000_000, 1000, 16, Default::default(), StartupPolicy::Suppress,
    ).await.unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default()).await.unwrap();
    let result = RuntimeSceneHap::new(&identity, &document, vec![SceneSpec {
        id: "synthetic-reviewed".into(), name: "Root synthetic scene".into(),
    }], runtime.control(), move |id, scene| id == controller && scene == "synthetic-reviewed").await;
    assert!(admission.dispatch().await.unwrap().is_none(), "construction must not dispatch output");
    let result = result.map(|bridge| { assert_eq!(bridge.accessories.len(), 1); drop(bridge); });
    drop(runtime); drop(admission); drop(identity);
    std::fs::remove_dir_all(root).unwrap();
    result
}

#[tokio::test]
async fn reviewed_enabled_immediate_definition_has_hap_action_entry() {
    let result = construct(true, true).await;
    assert!(result.is_ok(), "strict-runtime immediate scene is accepted but HAP publication rejects it: {result:?}");
}

#[tokio::test]
async fn existing_reviewed_timed_definition_positive_control() {
    construct(false, true).await.unwrap();
}

#[tokio::test]
async fn disabled_immediate_definition_must_not_be_published() {
    assert!(construct(true, false).await.is_err());
}
