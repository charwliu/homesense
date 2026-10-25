//! Synthetic native frames and query provenance. No physical IO or network.
#[path = "native_scene_indicator_tests.rs"]
mod indicator_joint;
use super::*;
use crate::native_scene::Manager;
use homesense_core::{Command, DeviceId, LevelTransition, Percent};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};
fn inputs_native(combined: bool) -> (Vec<u8>, Json) {
    let (config, mut rules) = inputs(false);
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["gateways"][0]["modules"][0]["profile"] = json!("ad8_din_rs485_v2_2_1");
    if combined {
        config["devices"][0]["supply_relay"] = json!("switch");
        config["devices"][0]["default_on_level"] = json!(100);
    }
    rules["scene_switch_groups"][0]["reconciliation"] = json!({"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"synthetic relative scope"});
    (serde_json::to_vec(&config).unwrap(), rules)
}
struct Wire {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_after: Option<usize>,
}
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut target = self.bytes.lock().unwrap();
        if let Some(n) = self.fail_after {
            if target.len() >= n {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let n = bytes.len().min(n - target.len());
            target.extend_from_slice(&bytes[..n]);
            Ok(n)
        } else {
            target.extend_from_slice(bytes);
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn drain(h: &Harness) -> Vec<(u64, DeviceId, Command, [u8; 5])> {
    let mut result = vec![];
    let bundle=h.actor.automation_bundle(h.controller,h.generation.clone()).await.unwrap();
    let mut wire_feedback=crate::feedback::Feedback::new(&bundle.configuration,homesense_core::ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),h.generation.epochs.transport).unwrap();
    for _ in 0..200 {
        if let Some(ticket) = h.actor.dispatch().await.unwrap() {
            let bytes = Arc::new(Mutex::new(vec![]));
            let mut wire = hb_transport::Connection::for_simulation(
                Wire {
                    bytes: bytes.clone(),
                    fail_after: None,
                },
                h.generation.epochs.transport,
            );
            wire.send(&ticket.frame, ticket.command.deadline_ms, || {
                h.actor.monotonic_ms()
            })
            .unwrap();
            let frame: [u8; 5] = bytes.lock().unwrap().clone().try_into().unwrap();
            result.push((
                ticket.command.id,
                ticket.command.device_id,
                ticket.command.command,
                frame,
            ));
            h.actor.finish_write(ticket.command.id, true).await.unwrap();
            if h.actor.module_confirmation_active() && matches!(frame[2],1..=8) {
                tokio::time::sleep(Duration::from_millis(3)).await;
                let value=if frame[1]==0x15 && matches!(frame[3],0xf1|0xf2){if frame[3]==0xf1{100}else{0}}else{frame[3]};
                wire_feedback.apply(&crate::transport_owner::GatewayRead{gateway_id:"bus".into(),transport_epoch:h.generation.epochs.transport,
                    received_at_ms:h.actor.monotonic_ms(),result:hb_transport::Receive::Frames{transport_epoch:h.generation.epochs.transport,
                    frames:vec![hb_protocol::RawFrame{address:frame[1],command:0xd0+frame[2],value}]}}).unwrap();
                for _ in 0..2 {wire_feedback.reconcile_control_phases(&h.actor,h.controller).await.unwrap();}
            }
        } else {
            let ready = h
                .actor
                .diagnostics(h.controller, h.generation.clone())
                .await
                .unwrap();
            if ready.queued == 0 && !ready.in_flight {
                return result;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("synthetic queue did not drain")
}
async fn start(
    h: &Harness,
    manager: &mut Manager,
    scene: &str,
    key: &str,
) -> crate::native_scene::Receipt {
    manager
        .start(
            &h.actor,
            h.controller,
            h.generation.clone(),
            &h.document,
            scene.into(),
            key.into(),
            &h.feedback,
        )
        .await
        .unwrap()
}
async fn row(h: &Harness, key: &str) -> homesense_storage::native_scene::Record {
    h.actor
        .native_scene_status(h.controller, h.generation.clone(), key.into())
        .await
        .unwrap()
        .unwrap()
}
fn relay(h: &mut Harness, on: bool) {
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames: vec![hb_protocol::RawFrame {
                    address: 3,
                    command: 0xd1,
                    value: if on { 0xf1 } else { 0xf2 },
                }],
            },
        })
        .unwrap();
}
async fn finish_standalone(h: &mut Harness, manager: &mut Manager, key: &str, main: u8, down: u8) {
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    let queries = drain(h).await;
    assert_eq!(queries.len(), 2);
    assert!(queries.iter().all(|(_, _, c, _)| *c == Command::Query));
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(h, Some(main), Some(down));
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(h, key).await.status,
        homesense_storage::native_scene::Status::Observed
    );
}
#[tokio::test]
async fn unknown_native_toggle_is_single_and_actual_query_level_replaces_old_setting_without_guessing()
 {
    for actual in [0, 17, 30, 100] {
        let (config, rules) = inputs_native(false);
        let mut h = harness(config, rules).await;
        let id = DeviceId::new("light").unwrap();
        let mut history = Store::open(h.directory.join("runtime.sqlite")).unwrap();
        let command = homesense_core::queue::QueuedCommand {
            id: 1,
            device_id: id.clone(),
            command: Command::SetLevel {
                level: Percent::new(60).unwrap(),
                transition: LevelTransition::Ramp,
            },
            origin: homesense_core::queue::Origin::Http,
            epochs: h.generation.epochs,
            deadline_ms: 1000,
            run_id: None,
        };
        history
            .accept_commands("previous-synthetic-boot", &[command], 0)
            .unwrap();
        history
            .transition_command(
                "previous-synthetic-boot",
                1,
                homesense_storage::journal::Status::Accepted,
                homesense_storage::journal::Status::Writing,
                1,
            )
            .unwrap();
        history
            .transition_command(
                "previous-synthetic-boot",
                1,
                homesense_storage::journal::Status::Writing,
                homesense_storage::journal::Status::Written,
                2,
            )
            .unwrap();
        drop(history);
        let mut manager = Manager::default();
        assert_eq!(
            start(&h, &mut manager, "first", "single").await.status,
            "pending"
        );
        assert_eq!(h.selection().await.stored.value, None);
        assert!(h.selection().await.physical.unwrap().pending_relative);
        assert_eq!(
            start(&h, &mut manager, "first", "single").await.status,
            "pending"
        );
        assert!(
            manager
                .start(
                    &h.actor,
                    h.controller,
                    h.generation.clone(),
                    &h.document,
                    "second".into(),
                    "another".into(),
                    &h.feedback
                )
                .await
                .is_err()
        );
        let controls = drain(&h).await;
        assert_eq!(
            controls
                .iter()
                .filter(|(_, _, c, _)| *c == Command::Toggle)
                .count(),
            1
        );
        assert_eq!(
            controls
                .iter()
                .find(|(_, _, c, _)| *c == Command::Toggle)
                .unwrap()
                .3,
            [0xae, 0x15, 0xa1, 0xf1, 0xee]
        );
        finish_standalone(&mut h, &mut manager, "single", actual, 0).await;
        assert_eq!(
            row(&h, "single").await.selected,
            Some(if actual == 0 { 0 } else { 1 })
        );
        let settings = Store::open(h.directory.join("runtime.sqlite"))
            .unwrap()
            .recorded_dimmer_levels(1, std::slice::from_ref(&id))
            .unwrap();
        assert_eq!(settings[&id].get(), actual);
        assert_eq!(
            start(&h, &mut manager, "first", "single").await.status,
            "observed"
        );
        assert!(drain(&h).await.is_empty());
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        h.close().await;
    }
}
#[tokio::test]
async fn known_positive_is_absolute_negative_and_source_unused_lamps_are_off() {
    for (scene, main, down, target) in [("first", 30, 0, 100), ("third", 0, 30, 17)] {
        let (config, mut rules) = inputs_native(false);
        if scene == "first" {
            rules["timed_scenes"][0]["steps"]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        let mut h = harness(config, rules).await;
        levels(&mut h, Some(main), Some(down));
        let mut manager = Manager::default();
        start(&h, &mut manager, scene, "absolute").await;
        let controls = drain(&h).await;
        assert!(controls.iter().all(|(_, _, c, _)| *c != Command::Toggle));
        assert!(
            controls
                .iter()
                .any(|(_, _, c, _)| matches!(c,Command::SetLevel{level,..} if level.get()==target))
        );
        assert!(
            controls
                .iter()
                .any(|(_, _, c, _)| matches!(c,Command::SetLevel{level,..} if level.get()==0))
        );
        assert!(
            controls
                .iter()
                .all(|(_, id, _, _)| ["light", "down"].contains(&id.as_str()))
        );
        let (main, down) = if scene == "first" { (100, 0) } else { (0, 17) };
        if controls.iter().any(|(_,_,command,_)|matches!(command,Command::SetLevel{transition:LevelTransition::Ramp,..})) {
            manager.tick(&h.actor,&h.document,&h.feedback).await;
            assert!(drain(&h).await.is_empty(),"native ramp must not immediately query an intermediate level");
            tokio::time::sleep(Duration::from_millis(6001)).await;
        }
        finish_standalone(&mut h, &mut manager, "absolute", main, down).await;
        h.close().await;
    }
}
#[tokio::test]
async fn combined_unknown_toggles_only_supply_then_on_query_sets_target_and_queries_both() {
    for powered in [false, true] {
        let (config, rules) = inputs_native(true);
        let mut h = harness(config, rules).await;
        Store::open(h.directory.join("runtime.sqlite"))
            .unwrap()
            .remember_light_levels(
                1,
                &[(DeviceId::new("light").unwrap(), Percent::new(60).unwrap())],
            )
            .unwrap();
        let mut manager = Manager::default();
        start(&h, &mut manager, "first", "combined").await;
        let controls = drain(&h).await;
        assert_eq!(
            controls
                .iter()
                .filter(|(_, _, c, _)| *c == Command::Toggle)
                .count(),
            1
        );
        let toggle = controls
            .iter()
            .find(|(_, _, c, _)| *c == Command::Toggle)
            .unwrap();
        assert_eq!(toggle.1.as_str(), "switch");
        assert_eq!(toggle.3, [0xae, 3, 0xa1, 0xf1, 0xee]);
        assert!(!controls.iter().any(|(_, id, _, _)| id.as_str() == "light"));
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        let queries = drain(&h).await;
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].1.as_str(), "switch");
        assert_eq!(queries[0].2, Command::Query);
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        assert!(drain(&h).await.is_empty());
        tokio::time::sleep(Duration::from_millis(2)).await;
        relay(&mut h, powered);
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        let dimmer = drain(&h).await;
        assert_eq!(dimmer.len(), usize::from(powered));
        if powered {
            assert_eq!(dimmer[0].1.as_str(), "light");
            assert!(matches!(dimmer[0].2,Command::SetLevel{level,..} if level.get()==100));
        }
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        let queries = drain(&h).await;
        assert_eq!(queries.len(), 3);
        assert!(queries.iter().all(|(_, _, c, _)| *c == Command::Query));
        tokio::time::sleep(Duration::from_millis(2)).await;
        levels(&mut h, Some(if powered { 100 } else { 60 }), Some(0));
        relay(&mut h, powered);
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        assert_eq!(
            row(&h, "combined").await.status,
            homesense_storage::native_scene::Status::Observed
        );
        assert_eq!(
            row(&h, "combined").await.selected,
            Some(if powered { 1 } else { 0 })
        );
        if !powered {
            assert_eq!(
                Store::open(h.directory.join("runtime.sqlite"))
                    .unwrap()
                    .light_restore_levels(1)
                    .unwrap()[&DeviceId::new("light").unwrap()]
                    .get(),
                60
            );
        }
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        h.close().await;
    }
}
#[tokio::test]
async fn stale_invalid_read_not_ready_and_unauthorized_are_not_legitimate_unknown() {
    for kind in [
        "stale",
        "invalid",
        "not_ready",
        "feedback_failed",
        "unauthorized",
    ] {
        let (config, mut rules) = inputs_native(false);
        if kind == "stale" {
            rules["scene_switch_groups"][0]["reconciliation"]["max_age_ms"] = json!(1);
        }
        let mut h = harness(config, rules).await;
        match kind {
            "stale" => {
                levels(&mut h, Some(30), Some(0));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            "invalid" => {
                levels(&mut h, Some(101), None);
            }
            "not_ready" => {
                h.actor.transport_failed().await.unwrap();
            }
            "feedback_failed" => {
                h.feedback.invalidate().unwrap();
            }
            _ => {}
        }
        let controller = if kind == "unauthorized" {
            uuid::Uuid::new_v4()
        } else {
            h.controller
        };
        assert!(
            Manager::default()
                .start(
                    &h.actor,
                    controller,
                    h.generation.clone(),
                    &h.document,
                    "first".into(),
                    "rejected".into(),
                    &h.feedback
                )
                .await
                .is_err(),
            "{kind}"
        );
        h.no_output();
        h.close().await;
    }
}
#[tokio::test]
async fn partial_and_prequery_samples_stay_pending_then_fresh_complete_samples_finish() {
    let (config, rules) = inputs_native(false);
    let mut h = harness(config, rules).await;
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "fresh").await;
    drain(&h).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(30), Some(0));
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain(&h).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(&h, "fresh").await.status,
        homesense_storage::native_scene::Status::Pending
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(30), None);
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(&h, "fresh").await.status,
        homesense_storage::native_scene::Status::Pending
    );
    assert_eq!(h.selection().await.stored.value, None);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, None, Some(0));
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(&h, "fresh").await.status,
        homesense_storage::native_scene::Status::Observed
    );
    assert_eq!(row(&h, "fresh").await.selected, Some(1));
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    h.close().await;
}

#[tokio::test]
async fn partial_write_and_disconnect_are_uncertain_without_resending_native_toggle() {
    for partial in [false, true] {
        let (config, rules) = inputs_native(false);
        let h = harness(config, rules).await;
        let mut manager = Manager::default();
        start(&h, &mut manager, "first", "failure").await;
        if partial {
            let ticket = h.actor.dispatch().await.unwrap().unwrap();
            let bytes = Arc::new(Mutex::new(vec![]));
            let mut wire = hb_transport::Connection::for_simulation(
                Wire {
                    bytes: bytes.clone(),
                    fail_after: Some(2),
                },
                h.generation.epochs.transport,
            );
            let error = wire
                .send(&ticket.frame, ticket.command.deadline_ms, || {
                    h.actor.monotonic_ms()
                })
                .unwrap_err();
            assert_eq!(error.bytes_written, 2);
            assert_eq!(bytes.lock().unwrap().len(), 2);
            h.actor
                .finish_write(ticket.command.id, false)
                .await
                .unwrap();
        } else {
            drain(&h).await;
            h.actor.transport_failed().await.unwrap();
        }
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        assert_eq!(
            row(&h, "failure").await.status,
            homesense_storage::native_scene::Status::Unknown
        );
        let commands: i64 = h
            .sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            start(&h, &mut manager, "first", "failure").await.status,
            "unknown"
        );
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        assert_eq!(
            h.sql()
                .query_row("SELECT count(*) FROM command_journal", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            commands
        );
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        h.close().await;
    }
}
#[tokio::test]
async fn restart_preserves_consumed_request_but_never_recreates_physical_phases() {
    let (config, rules) = inputs_native(false);
    let mut h = harness(config.clone(), rules).await;
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "restart").await;
    drain(&h).await;
    drop(manager);
    h.actor.shutdown().await.unwrap();
    let controller = h.controller;
    h.actor = Admission::open(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    let parsed =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    h.feedback = Feedback::new(
        &parsed,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        h.generation.epochs.transport,
    )
    .unwrap();
    let mut manager = Manager::default();
    assert_eq!(
        start(&h, &mut manager, "first", "restart").await.status,
        "unknown"
    );
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert!(drain(&h).await.is_empty());
    assert_eq!(h.selection().await.stored.value, None);
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    h.close().await;
}
#[tokio::test]
async fn manual_intent_version_and_manual_light_command_stop_further_native_phases() {
    for (combined, variable, waiting) in [
        (false, false, false),
        (false, true, false),
        (false, false, true),
        (false, true, true),
        (true, false, false),
        (true, true, false),
        (true, false, true),
        (true, true, true),
    ] {
        let (config, rules) = inputs_native(combined);
        let h = harness(config, rules).await;
        let mut manager = Manager::default();
        start(&h, &mut manager, "first", "manual").await;
        drain(&h).await;
        if waiting {
            manager.tick(&h.actor, &h.document, &h.feedback).await;
            assert_eq!(
                drain(&h)
                    .await
                    .iter()
                    .filter(|(_, _, command, _)| *command == Command::Query)
                    .count(),
                if combined { 1 } else { 2 }
            );
        }
        if variable {
            let mut store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
            store
                .accept_queue_batch_with_variables(
                    h.actor.boot_id(),
                    &[],
                    &[],
                    h.actor.monotonic_ms(),
                    None,
                    &[Update {
                        revision: 1,
                        name: "selection".into(),
                        expected_version: 0,
                        value: Some(Value::Integer(2)),
                    }],
                )
                .unwrap();
        } else {
            h.actor
                .submit(homesense_hap::requests::Batch {
                    controller: h.controller,
                    intents: vec![homesense_hap::requests::Intent {
                        device: DeviceId::new("light").unwrap(),
                        command: Command::SetLevel {
                            level: Percent::new(50).unwrap(),
                            transition: LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                })
                .await
                .unwrap();
            drain(&h).await;
        }
        let before: i64 = h
            .sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r.get(0))
            .unwrap();
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        assert_eq!(
            row(&h, "manual").await.status,
            homesense_storage::native_scene::Status::Unknown
        );
        assert_eq!(
            h.sql()
                .query_row("SELECT count(*) FROM command_journal", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        h.close().await;
    }
}
#[tokio::test]
async fn missing_query_results_timeout_without_retry_or_false_selection() {
    let (config, rules) = inputs_native(false);
    let mut h = harness(config, rules).await;
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "timeout").await;
    drain(&h).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain(&h).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(30), None);
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(&h, "timeout").await.status,
        homesense_storage::native_scene::Status::Pending
    );
    tokio::time::sleep(Duration::from_millis(5100)).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    assert_eq!(
        row(&h, "timeout").await.status,
        homesense_storage::native_scene::Status::Unknown
    );
    assert_eq!(h.selection().await.stored.value, None);
    assert!(drain(&h).await.is_empty());
    assert_eq!(
        start(&h, &mut manager, "first", "timeout").await.status,
        "unknown"
    );
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    h.close().await;
}

#[tokio::test]
async fn web_relative_action_is_explicit_scoped_and_deduplicated_with_pending_state() {
    use crate::web_auth::{Grant, Role, Tokens};
    use http_body_util::BodyExt;
    let (config, rules) = inputs_native(false);
    let mut h = harness(config, rules).await;
    let (control, mut receiver) =
        crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let tokens = std::sync::Arc::new(Tokens::default());
    let member = "abcdefghijklmnopqrstuvwxyz0123456789";
    let viewer = "abcdefghijklmnopqrstuvwxyz9876543210";
    for (secret, role) in [(member, Role::Member), (viewer, Role::Viewer)] {
        tokens
            .insert(
                uuid::Uuid::new_v4(),
                secret,
                Grant {
                    controller: h.controller,
                    role,
                    scenes: ["first".into()].into(),
                    devices: Default::default(),
                },
            )
            .unwrap();
    }
    let api = crate::web_api::Api {
        daily_backup: Default::default(),
        hap_placement:Default::default(),hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://127.0.0.1:8088".into(),
        tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control,
            revision: 1,
            boot: h.actor.boot_id().into(),
            scenes: [
                ("first".into(), "First".into()),
                ("second".into(), "Second".into()),
            ]
            .into(),
        })),
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap(),
        ),
        drafts: None,
    };
    fn request(
        path: &str,
        secret: &str,
        body: Option<Json>,
        origin: bool,
    ) -> hyper::Request<hyper::body::Bytes> {
        let mut builder = hyper::Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:8088")
            .header("authorization", format!("Bearer {secret}"));
        if body.is_some() {
            builder = builder
                .method("POST")
                .header("content-type", "application/json");
        }
        if origin {
            builder = builder.header("origin", "http://127.0.0.1:8088");
        }
        builder
            .body(body.map(|b| b.to_string()).unwrap_or_default().into())
            .unwrap()
    }
    async fn payload(response: hyper::Response<http_body_util::Full<hyper::body::Bytes>>) -> Json {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    let body = json!({"scene":"first","request_id":"web-once"});
    for (secret, body, origin, expected) in [
        (viewer, body.clone(), true, hyper::StatusCode::FORBIDDEN),
        (member, body.clone(), false, hyper::StatusCode::FORBIDDEN),
        (
            member,
            json!({"scene":"second","request_id":"denied"}),
            true,
            hyper::StatusCode::FORBIDDEN,
        ),
        (
            member,
            json!({"scene":"first","request_id":"bad","on":true}),
            true,
            hyper::StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            api.handle(request(
                "/api/v1/scene-relative-actions",
                secret,
                Some(body),
                origin
            ))
            .await
            .status(),
            expected
        );
    }
    h.no_output();
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(request(
            "/api/v1/scene-relative-actions",
            member,
            Some(body.clone()),
            true,
        )),
    )
    .await;
    assert_eq!(response.status(), hyper::StatusCode::ACCEPTED);
    let receipt = payload(response).await;
    assert_eq!(receipt["status"], "pending");
    assert_eq!(receipt["retry"], false);
    let duplicate = payload(
        api.handle(request(
            "/api/v1/scene-relative-actions",
            member,
            Some(body),
            true,
        ))
        .await,
    )
    .await;
    assert_eq!(duplicate, receipt);
    // Cross-endpoint independence is exercised after the native request settles below.
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(request("/api/v1/scenes", member, None, false)),
    )
    .await;
    let list = payload(response).await;
    assert_eq!(list["scenes"][0]["pending_relative"], true);
    assert_eq!(list["scenes"][0]["selected"], Json::Null);
    assert_eq!(list["scenes"][0]["can_control"], false);
    assert_eq!(list["scenes"][0]["relative_unknown_allowed"], false);
    let control_writes = drain(&h).await;
    assert_eq!(
        control_writes
            .iter()
            .filter(|(_, _, c, _)| *c == Command::Toggle)
            .count(),
        1
    );
    // Polling the status advances only the one bounded query phase, not another Toggle.
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(request(
            "/api/v1/scene-relative-requests/web-once",
            member,
            None,
            false,
        )),
    )
    .await;
    assert_eq!(payload(response).await["status"], "pending");
    assert_eq!(drain(&h).await.len(), 2);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(30), Some(0));
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(request(
            "/api/v1/scene-relative-requests/web-once",
            member,
            None,
            false,
        )),
    )
    .await;
    let observed = payload(response).await;
    assert_eq!(observed["status"], "observed");
    assert_eq!(observed["selected"], true);
    assert_eq!(observed["retry"], false);
    assert!(drain(&h).await.is_empty());
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE intent=x'01070000'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    // The same principal/key now has a separate absolute-action scope. Drive
    // the real control receiver and scheduler, rather than accepting a timeout
    // receipt from an unpolled request. Native observation above remains intact.
    let absolute = json!({"scene":"first","request_id":"web-once","on":false});
    let response = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        api.handle(request("/api/v1/scene-actions", member, Some(absolute.clone()), true)),
    ).await;
    assert_eq!(response.status(), hyper::StatusCode::ACCEPTED);
    let absolute_receipt = payload(response).await;
    assert_eq!(absolute_receipt["outcome"], "accepted");
    let run = absolute_receipt["run"].as_u64().unwrap();
    let mut written = Vec::new();
    for _ in 0..8 {
        let ticks = scenes.tick(h.actor.monotonic_ms()).await.unwrap();
        assert!(ticks.iter().all(|tick| tick.result.is_ok()));
        written.extend(drain(&h).await);
        if scenes.settled_terminal(run) { break; }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(scenes.settled_terminal(run));
    for device in ["light", "down"] {
        assert!(written.iter().any(|(_, id, command, frame)| {
            id.as_str() == device
                && matches!(command, Command::SetLevel { level, .. } if level.get() == 0)
                && frame[3] == 0
        }));
    }
    assert_eq!(h.sql().query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let command_count = h.sql().query_row("SELECT count(*) FROM command_journal", [], |r| r.get::<_, i64>(0)).unwrap();
    let repeat = api.handle(request("/api/v1/scene-actions", member, Some(absolute), true)).await;
    assert_eq!(repeat.status(), hyper::StatusCode::OK);
    assert_eq!(payload(repeat).await, absolute_receipt);
    let relative_repeat = api.handle(request("/api/v1/scene-relative-actions", member,
        Some(json!({"scene":"first","request_id":"web-once"})), true)).await;
    assert_eq!(relative_repeat.status(), hyper::StatusCode::OK);
    assert_eq!(payload(relative_repeat).await, receipt);
    assert_eq!(api.handle(request("/api/v1/scene-actions", member,
        Some(json!({"scene":"first","request_id":"web-once","on":true})), true)).await.status(),
        hyper::StatusCode::CONFLICT);
    assert_eq!(h.sql().query_row("SELECT count(*) FROM command_journal", [], |r| r.get::<_, i64>(0)).unwrap(), command_count);
    assert_eq!(h.sql().query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(api.receipts.lock().unwrap().record_count().unwrap(), 2);
    assert!(drain(&h).await.is_empty());
    h.close().await;
}

fn preserved_native_inputs() -> (Vec<u8>, Json) {
    let (config, mut rules) = inputs_native(false);
    rules["scene_switch_groups"][0]["members"].as_array_mut().unwrap().truncate(2);
    rules["scene_observation"]["bindings"].as_array_mut().unwrap().truncate(2);
    rules["scene_switch_groups"][0]["reconciliation"]["omitted_targets"] = json!("preserve");
    rules["timed_scenes"][0]["steps"].as_array_mut().unwrap().pop();
    rules["timed_scenes"][1]["steps"][0]["command"]["value"] = json!(0);
    let parsed = homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    Document::parse(&serde_json::to_vec(&rules).unwrap(), &parsed).unwrap();
    (config, rules)
}
#[tokio::test]
async fn native_sparse_first_member_never_controls_or_queries_an_omitted_output() {
    let (config, rules) = preserved_native_inputs();
    let mut h = harness(config, rules).await;
    levels(&mut h, None, Some(80));
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "sparse-first").await;
    let controls = drain(&h).await;
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0].1.as_str(), "light");
    assert_eq!(controls[0].2, Command::Toggle);
    assert_eq!(controls[0].3, [0xae, 0x15, 0xa1, 0xf1, 0xee]);
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    let queries = drain(&h).await;
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].1.as_str(), "light");
    assert_eq!(queries[0].2, Command::Query);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(100), Some(80));
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    let complete = row(&h, "sparse-first").await;
    assert_eq!(complete.status, homesense_storage::native_scene::Status::Observed);
    assert_eq!(complete.selected, Some(1));
    assert_eq!(start(&h, &mut manager, "first", "sparse-first").await.status, "observed");
    assert!(drain(&h).await.is_empty(), "a duplicate request cannot replay the sparse command");
}
#[tokio::test]
async fn native_sparse_nonfirst_member_captures_its_own_scope_not_the_first_members_scope() {
    let (config, rules) = preserved_native_inputs();
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(80), Some(0));
    let mut manager = Manager::default();
    start(&h, &mut manager, "second", "sparse-second").await;
    let controls = drain(&h).await;
    assert_eq!(controls.len(), 2);
    assert_eq!(controls.iter().map(|(_, id, _, _)| id.as_str()).collect::<std::collections::BTreeSet<_>>(), ["down", "light"].into());
    finish_standalone(&mut h, &mut manager, "sparse-second", 0, 100).await;
    assert_eq!(row(&h, "sparse-second").await.selected, Some(2));
}

#[tokio::test]
async fn accepted_native_final_queries_own_the_bus_turn_before_autonomous_refresh() {
    let (config, rules) = inputs_native(false);
    let h = harness(config, rules).await;
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "foreground-native-read").await;
    let controls = drain(&h).await;
    assert_eq!(controls.len(), 2);
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    let state = h.actor.diagnostics(h.controller, h.generation.clone()).await.unwrap();
    assert_eq!(state.queued, 2, "both final queries are accepted but not yet written");
    assert!(state.protected_queued, "a fresh automatic read must not interleave ahead of a native operation's fixed-deadline final reads");
    assert!(matches!(h.actor.reserve_physical_read("bus".into(), h.actor.monotonic_ms()+500).await, Err(hap::server::WriteStatus::ResourceBusy)));
    let queries = drain(&h).await;
    assert_eq!(queries.len(), 2);
    assert!(queries.iter().all(|(_, _, c, _)| *c == Command::Query));
    assert!(!h.actor.diagnostics(h.controller, h.generation.clone()).await.unwrap().protected_queued, "the reservation ends when the explicit read tickets leave the queue");
    assert!(h.actor.reserve_physical_read("bus".into(), h.actor.monotonic_ms()+500).await.is_ok());
}

#[test]
fn foreground_native_reads_are_limited_to_the_reserved_run_current_epoch_and_original_deadline() {
    use homesense_core::queue::{Epochs, Origin, QueuedCommand};
    let epochs = Epochs { configuration: 1, transport: 2, control: 3 };
    let mut command = QueuedCommand { id: 1, device_id: DeviceId::new("light").unwrap(), command: Command::Query, origin: Origin::Hap, epochs, deadline_ms: 500, run_id: Some((1u64 << 62)+1) };
    assert!(crate::native_scene::foreground_query(&command, epochs, 499));
    assert!(!crate::native_scene::foreground_query(&command, epochs, 500));
    assert!(!crate::native_scene::foreground_query(&command, Epochs { transport: 4, ..epochs }, 499));
    for id in [None, Some((1u64 << 62)-1), Some(1u64 << 63)] {
        command.run_id=id;
        assert!(!crate::native_scene::foreground_query(&command, epochs, 499));
    }
    command.run_id=Some((1u64 << 62)+1);
    command.command=Command::Toggle;
    assert!(!crate::native_scene::foreground_query(&command, epochs, 499));
}
