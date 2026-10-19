//! Synthetic source -> strict document -> real admission/router -> memory wire.
#[path = "../../homesense-import/tests/support/reviewed_scene.rs"]
mod fixture;
use fixture::*;
use homesense_config::rule_document::Document;
use homesense_core::{ExecutionContext, rule::Budget};
use homesense_import::reviewed_scenes::{convert, link_event};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    configured_rules::ConfiguredRules,
    event_rules::Trigger,
    transport_owner::TransportOwner,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
struct Memory(Arc<Mutex<Vec<u8>>>);
impl std::io::Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Memory {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn full_target36_runtime_receipts_wire_order_and_caller_member_chain_survive_reopen() {
    let (a, b) = topology();
    let event = guarded_event(&call(3, 7, 50, false, true));
    let xml = project(&a, &b, &event, false);
    let cfg = config(false);
    let review = review(&xml);
    let bundle = convert(&xml, &cfg, &review).unwrap();
    let draft = link_event(&xml, &event, &cfg, &review, &bundle).unwrap();
    let mut doc = bundle.document.clone();
    doc["variables"] = json!([{"id":"7","kind":"integer","lifetime":"persistent"}]);
    doc["rules"] = json!([document_rule(draft["actions"].clone(), false)]);
    let directory = std::env::temp_dir().join(format!("reviewed-scene-runtime-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.sqlite");
    Store::open(&path)
        .unwrap()
        .publish_configuration(
            None,
            &config_bytes(false),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let id = Uuid::new_v4();
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            request_ttl: std::time::Duration::from_secs(30),
            ..Options::default()
        },
        move |candidate| candidate == id,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let boot = actor.boot_id().to_string();
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    let mut disabled =
        ConfiguredRules::load_document(actor.clone(), id, &parsed, Budget::default())
            .await
            .unwrap();
    let result = disabled
        .events
        .dispatch(Trigger::StartupSynchronized, None)
        .await;
    assert!(result.completed.is_empty());
    assert!(actor.dispatch().await.unwrap().is_none());
    // Only the synthetic lab explicitly enables both definitions and caller.
    doc["rules"][0]["enabled"] = true.into();
    for s in doc["immediate_scenes"].as_array_mut().unwrap() {
        s["enabled"] = true.into();
    }
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    let mut configured =
        ConfiguredRules::load_document(actor.clone(), id, &parsed, Budget::default())
            .await
            .unwrap();
    let result = configured
        .events
        .dispatch(Trigger::StartupSynchronized, None)
        .await;
    assert!(result.failure.is_none(), "{:?}", result.failure);
    assert_eq!(result.completed.len(), 1);
    assert_eq!(result.completed[0].commands.len(), 36);
    assert!(result.completed[0].unknown_branches.is_empty());
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut owner = TransportOwner::new(
        actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
        )]
        .into(),
    );
    for expected in &result.completed[0].commands {
        let written = owner.step_with_frame_budget(1).await.unwrap().unwrap();
        assert_eq!(written.command_id, *expected);
        written.result.unwrap();
    }
    assert!(owner.step_with_frame_budget(1).await.unwrap().is_none());
    let expected = (4u8..40)
        .flat_map(|i| [0xae, 16 + i / 8, i % 8 + 1, 0xf2, 0xee])
        .collect::<Vec<_>>();
    assert_eq!(*bytes.lock().unwrap(), expected);
    actor.shutdown().await.unwrap();
    let mut reopened = Store::open(&path).unwrap();
    let chain = reopened
        .rule_chain(&boot, result.correlation_id, 0, 64)
        .unwrap();
    assert_eq!(chain.records.len(), 1);
    assert_eq!(chain.records[0].commands.len(), 36);
    for (i, command) in chain.records[0].commands.iter().enumerate() {
        assert_eq!(command.device, format!("control4:{}", 1004 + i));
        assert_eq!(command.status, Status::Written);
        assert!(command.source.as_ref().unwrap().starts_with(&format!(
            "device:900/event:2/code:2/code:3 -> control4:scene:7:11/member:{i}/element:0/sha256:"
        )));
    }
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("select count(*) from command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        36
    );
    assert_eq!(
        sql.query_row(
            "select count(*) from logical_variable where name='7'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(sql);
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn false_original_guard_keeps_all_target_members_unwritten() {
    let (a, b) = topology();
    let event = guarded_event(&call(3, 7, 50, false, true)).replacen(
        "type=\"int\">1</param>",
        "type=\"int\">0</param>",
        1,
    );
    let xml = project(&a, &b, &event, false);
    let cfg = config(false);
    let review = review(&xml);
    let bundle = convert(&xml, &cfg, &review).unwrap();
    let draft = link_event(&xml, &event, &cfg, &review, &bundle).unwrap();
    let mut doc = bundle.document;
    doc["variables"] = json!([{"id":"7","kind":"integer","lifetime":"persistent"}]);
    doc["rules"] = json!([document_rule(draft["actions"].clone(), true)]);
    for s in doc["immediate_scenes"].as_array_mut().unwrap() {
        s["enabled"] = true.into();
    }
    let directory =
        std::env::temp_dir().join(format!("reviewed-scene-false-guard-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.sqlite");
    Store::open(&path)
        .unwrap()
        .publish_configuration(
            None,
            &config_bytes(false),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let id = Uuid::new_v4();
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |candidate| candidate == id,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    let mut configured =
        ConfiguredRules::load_document(actor.clone(), id, &parsed, Budget::default())
            .await
            .unwrap();
    let result = configured
        .events
        .dispatch(Trigger::StartupSynchronized, None)
        .await;
    assert!(result.failure.is_none());
    assert_eq!(result.completed.len(), 1);
    assert!(result.completed[0].commands.is_empty());
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.shutdown().await.unwrap();
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("select count(*) from command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn reviewed_logical_combined_target_writes_exact_level_and_supply_with_one_member_source() {
    let a = scene(102, "none", &[(2000, 40, 0, 0)]);
    let event = guarded_event(&call(3, 7, 102, true, true));
    let xml = project(&a, "", &event, true);
    let cfg = combined_config();
    let review = combined_review(&xml);
    let bundle = convert(&xml, &cfg, &review).unwrap();
    let draft = link_event(&xml, &event, &cfg, &review, &bundle).unwrap();
    let mut doc = bundle.document;
    doc["variables"] = json!([{"id":"7","kind":"integer","lifetime":"persistent"}]);
    doc["rules"] = json!([document_rule(draft["actions"].clone(), true)]);
    doc["immediate_scenes"][0]["enabled"] = true.into();
    let directory =
        std::env::temp_dir().join(format!("reviewed-combined-runtime-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.sqlite");
    Store::open(&path)
        .unwrap()
        .publish_configuration(
            None,
            &combined_config_bytes(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let id = Uuid::new_v4();
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |candidate| candidate == id,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let boot = actor.boot_id().to_string();
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    let mut configured =
        ConfiguredRules::load_document(actor.clone(), id, &parsed, Budget::default())
            .await
            .unwrap();
    let result = configured
        .events
        .dispatch(Trigger::StartupSynchronized, None)
        .await;
    assert!(result.failure.is_none(), "{:?}", result.failure);
    assert_eq!(result.completed[0].commands.len(), 2);
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut owner = TransportOwner::new(
        actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
        )]
        .into(),
    );
    for expected in &result.completed[0].commands {
        let output = owner.step_with_frame_budget(1).await.unwrap().unwrap();
        assert_eq!(output.command_id, *expected);
        output.result.unwrap();
    }
    assert!(owner.step_with_frame_budget(1).await.unwrap().is_none());
    assert_eq!(
        *bytes.lock().unwrap(),
        [0xae, 0x20, 0x01, 40, 0xee, 0xae, 0x10, 2, 0xf1, 0xee]
    );
    let mixed = scene(103, "none", &[(2000, 40, 0, 0), (2001, 100, 0, 0)]);
    let mixed_event = guarded_event(&call(3, 7, 103, true, true));
    let mixed_xml = project(&mixed, "", &mixed_event, true);
    let mixed_review = combined_review(&mixed_xml);
    let mixed_bundle = convert(&mixed_xml, &cfg, &mixed_review).unwrap();
    assert_eq!(mixed_bundle.scenes[0].status, "held");
    assert!(mixed_bundle.scenes[0].blockers[0].contains("owned-supply/alias conflict"));
    assert!(link_event(&mixed_xml, &mixed_event, &cfg, &mixed_review, &mixed_bundle).is_err());
    let mut invalid = mixed_bundle.document;
    invalid["rules"] = json!([document_rule(
        json!([{"source":"synthetic invalid caller","action":{"kind":"scene","id":"control4:scene:7:103","activate":true}}]),
        true
    )]);
    let rejected = Document::parse(&serde_json::to_vec(&invalid).unwrap(), &cfg).unwrap();
    assert!(
        ConfiguredRules::load_document(actor.clone(), id, &rejected, Budget::default())
            .await
            .is_err()
    );
    assert!(owner.step_with_frame_budget(1).await.unwrap().is_none());
    assert_eq!(
        bytes.lock().unwrap().len(),
        10,
        "held mixed aliases never add output"
    );

    actor.shutdown().await.unwrap();
    let mut store = Store::open(&path).unwrap();
    let chain = store
        .rule_chain(&boot, result.correlation_id, 0, 64)
        .unwrap();
    let commands = &chain.records[0].commands;
    assert_eq!(
        commands
            .iter()
            .map(|c| c.device.as_str())
            .collect::<Vec<_>>(),
        ["control4:1000", "control4:1001"]
    );
    for c in commands {
        assert_eq!(c.status, Status::Written);
        assert!(c.source.as_ref().unwrap().starts_with(
            "device:900/event:2/code:2/code:3 -> control4:scene:7:102/member:0/element:0/sha256:"
        ));
    }
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}
