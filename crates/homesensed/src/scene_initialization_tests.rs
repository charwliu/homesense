//! Synthetic initialization, source and feedback-lease integration, no sockets.
use super::*;
use crate::scene_initialization::{Prepared, matching_member};
use homesense_config::scene_switches::LightTarget;
fn inputs(enabled: bool) -> (Vec<u8>, Json) {
    let mut config: Json = serde_json::from_slice(&devices()).unwrap();
    config["devices"].as_array_mut().unwrap().push(json!({"id":"down","name":"Synthetic downlight","gateway":"bus","address":"15","channel":2,"enabled":true}));
    let mut rules = automation();
    for (scene, (main, down)) in rules["timed_scenes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip([(100, 0), (100, 100), (0, 17), (0, 0)])
    {
        scene["steps"][0]["command"]["value"] = json!(main);
        scene["steps"][0]["offset_ms"] = json!(0);
        scene["steps"].as_array_mut().unwrap().push(json!({"source":"synthetic down","offset_ms":0,"device":"down","command":{"kind":"level","value":down,"ramp":false}}));
    }
    rules["scene_switch_groups"][0]["initialization"] = json!({"enabled":enabled,"policy":"unique_light_targets","max_age_ms":5000,"review_evidence":"synthetic complete final vectors"});
    (serde_json::to_vec(&config).unwrap(), rules)
}
async fn harness(config: Vec<u8>, rules: Json) -> Harness {
    let directory =
        std::env::temp_dir().join(format!("homesense-initialization-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.sqlite");
    let bundle = Store::open(&path)
        .unwrap()
        .publish_automation(
            None,
            &config,
            &serde_json::to_vec(&rules).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let actor = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    let generation = actor.task_generation().await.unwrap();
    let feedback = Feedback::new(
        &bundle.configuration,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        generation.epochs.transport,
    )
    .unwrap();
    Harness {
        actor,
        feedback,
        consumer: Consumer::default(),
        controller,
        generation,
        document: bundle.automation.unwrap(),
        directory,
    }
}
fn levels(h: &mut Harness, main: Option<u8>, down: Option<u8>) {
    let frames = [(0xd1, main), (0xd2, down)]
        .into_iter()
        .filter_map(|(command, level)| {
            level.map(|value| hb_protocol::RawFrame {
                address: 0x15,
                command,
                value,
            })
        })
        .collect();
    h.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: h.generation.epochs.transport,
            received_at_ms: h.actor.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: h.generation.epochs.transport,
                frames,
            },
        })
        .unwrap();
}
async fn prepare(h: &Harness) -> Option<Prepared> {
    Prepared::prepare(
        &h.actor,
        h.controller,
        h.generation.clone(),
        &h.document.scene_switch_groups[0],
        &h.document,
        &h.feedback,
    )
    .await
    .unwrap()
}
async fn initialize(h: &Harness) {
    if let Some(prepared) = prepare(h).await {
        h.actor
            .initialize_scene_selection(h.controller, h.generation.clone(), prepared)
            .await
            .unwrap();
    }
}
#[test]
fn complete_exact_vectors_match_36_inputs_and_reject_ambiguity() {
    let (config, rules) = inputs(true);
    let config =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    let document = Document::parse(&serde_json::to_vec(&rules).unwrap(), &config).unwrap();
    let targets = document.scene_switch_groups[0]
        .light_targets(&document.timed_scenes)
        .unwrap();
    for main in [None, Some(0), Some(17), Some(30), Some(50), Some(100)] {
        for down in [None, Some(0), Some(17), Some(30), Some(50), Some(100)] {
            let actual = [("light", main), ("down", down)]
                .into_iter()
                .filter_map(|(id, value)| {
                    value.map(|value| {
                        (
                            homesense_core::DeviceId::new(id).unwrap(),
                            LightTarget::Level(value),
                        )
                    })
                })
                .collect();
            let expected = match (main, down) {
                (Some(0), Some(0)) => Some(0),
                (Some(100), Some(0)) => Some(1),
                (Some(100), Some(100)) => Some(2),
                (Some(0), Some(17)) => Some(3),
                _ => None,
            };
            assert_eq!(
                matching_member(&targets, &actual),
                expected,
                "main={main:?} down={down:?}"
            );
        }
    }
    let mut ambiguous = targets.clone();
    ambiguous.insert(4, targets[&1].clone());
    assert_eq!(matching_member(&ambiguous, &targets[&1]), None);
}
#[test]
fn explicit_policy_is_disabled_by_default_and_requires_reviewed_complete_absolute_vectors() {
    let (config, mut rules) = inputs(true);
    let config =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    rules["scene_switch_groups"][0]["initialization"]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    assert!(
        !Document::parse(&serde_json::to_vec(&rules).unwrap(), &config)
            .unwrap()
            .scene_switch_groups[0]
            .initialization
            .as_ref()
            .unwrap()
            .enabled
    );
    rules["scene_switch_groups"][0]["initialization"]["enabled"] = json!(true);
    let mut unreviewed = rules.clone();
    unreviewed["scene_switch_groups"][0]["initialization"]["review_evidence"] = json!("");
    assert!(Document::parse(&serde_json::to_vec(&unreviewed).unwrap(), &config).is_err());
    let mut missing = rules.clone();
    missing["timed_scenes"][1]["steps"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(Document::parse(&serde_json::to_vec(&missing).unwrap(), &config).is_err());
    let mut bad_type = rules.clone();
    bad_type["timed_scenes"][0]["steps"][0]["command"] = json!({"kind":"switch","on":true});
    assert!(Document::parse(&serde_json::to_vec(&bad_type).unwrap(), &config).is_err());
    let mut age = rules.clone();
    age["scene_switch_groups"][0]["initialization"]["max_age_ms"] = json!(0);
    assert!(Document::parse(&serde_json::to_vec(&age).unwrap(), &config).is_err());
}
#[tokio::test]
async fn initialization_is_one_time_zero_output_and_external_first_key_remains_ambiguous() {
    for (main, down, value) in [(0, 0, 0), (100, 0, 1), (100, 100, 2), (0, 17, 3)] {
        let (config, rules) = inputs(true);
        let mut h = harness(config, rules).await;
        levels(&mut h, Some(main), Some(down));
        h.tick().await;
        initialize(&h).await;
        let snapshot = h.selection().await;
        assert_eq!(snapshot.stored.value, Some(Value::Integer(value)));
        assert_eq!(snapshot.stored.version, 1);
        assert_eq!(snapshot.initialization.as_ref().unwrap().value, value);
        assert!(snapshot.evidence.is_none());
        let details = crate::scene_switches::details(
            &h.document,
            &[("selection".into(), snapshot.clone())].into(),
        )
        .unwrap();
        assert_eq!(details["first"].selection_source, "light_initialization");
        assert_eq!(details["first"].selected, Some(value == 1));
        assert!(prepare(&h).await.is_none());
        h.no_output();
        // Ordinary locals never seed the independent EA possible-value set.
        tokio::time::sleep(Duration::from_millis(2)).await;
        h.press(1).await;
        let inferred = h.selection().await;
        assert_eq!(inferred.stored.value, None);
        assert!(inferred.initialization.is_none());
        assert!(inferred.evidence.is_some());
        assert!(prepare(&h).await.is_none());
        h.no_output();
        h.close().await;
    }
}
#[tokio::test]
async fn disabled_unknown_mixed_and_feedback_or_cas_competition_do_not_initialize() {
    for (enabled, main, down) in [
        (false, Some(100), Some(0)),
        (true, Some(100), None),
        (true, Some(30), Some(0)),
        (true, Some(100), Some(17)),
    ] {
        let (config, rules) = inputs(enabled);
        let mut h = harness(config, rules).await;
        levels(&mut h, main, down);
        assert!(prepare(&h).await.is_none());
        assert_eq!(h.selection().await.stored.version, 0);
        h.no_output();
        h.close().await;
    }
    let (config, rules) = inputs(true);
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(100), Some(0));
    let proof = prepare(&h).await.unwrap();
    levels(&mut h, Some(100), Some(100));
    assert_eq!(
        h.actor
            .initialize_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    assert_eq!(h.selection().await.stored.version, 0);
    let proof = prepare(&h).await.unwrap();
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![],
                subscriptions: vec![],
            },
            vec![Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 0,
                value: Some(Value::Integer(3)),
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        h.actor
            .initialize_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(3)));
    assert!(h.selection().await.initialization.is_none());
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn pending_scene_and_pending_control_reject_the_same_prepared_snapshot() {
    for pending_scene in [false, true] {
        let (config, rules) = inputs(true);
        let mut h = harness(config, rules).await;
        levels(&mut h, Some(100), Some(0));
        let proof = prepare(&h).await.unwrap();
        let mut scenes = crate::scene_scheduler::SceneScheduler::new(
            h.actor.clone(),
            h.controller,
            8,
            h.actor.monotonic_ms(),
        )
        .unwrap();
        if pending_scene {
            scenes
                .start_document(&h.document, "first", h.actor.monotonic_ms())
                .await
                .unwrap();
        } else {
            h.actor
                .submit_rule_update(
                    h.generation.clone(),
                    homesense_hap::requests::Batch {
                        controller: h.controller,
                        intents: vec![homesense_hap::requests::Intent {
                            device: homesense_core::DeviceId::new("light").unwrap(),
                            command: homesense_core::Command::SetLevel {
                                level: homesense_core::Percent::new(50).unwrap(),
                                transition: homesense_core::LevelTransition::Immediate,
                            },
                        }],
                        subscriptions: vec![],
                    },
                    vec![],
                )
                .await
                .unwrap();
        }
        assert_eq!(
            h.actor
                .initialize_scene_selection(h.controller, h.generation.clone(), proof)
                .await,
            Err(hap::server::WriteStatus::ResourceBusy)
        );
        assert_eq!(h.selection().await.stored.version, 0);
        assert!(h.selection().await.initialization.is_none());
        scenes.stop().await;
        h.close().await;
    }
}
#[tokio::test]
async fn missing_combined_sample_old_recorded_zero_and_old_boot_cannot_supply_initialization() {
    let (mut config, rules) = inputs(true);
    let mut raw: Json = serde_json::from_slice(&config).unwrap();
    raw["devices"][0]["supply_relay"] = json!("switch");
    raw["devices"][0]["default_on_level"] = json!(100);
    config = serde_json::to_vec(&raw).unwrap();
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(100), Some(0));
    assert!(
        prepare(&h).await.is_none(),
        "missing power sample must stay unknown"
    );
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
                    value: 0xf1,
                }],
            },
        })
        .unwrap();
    let proof = prepare(&h).await.unwrap();
    // Every physical input is in the shared lease, including the relay.
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
                    value: 0xf2,
                }],
            },
        })
        .unwrap();
    assert_eq!(
        h.actor
            .initialize_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    h.no_output();
    h.close().await;
    let (config, rules) = inputs(true);
    let mut h = harness(config.clone(), rules).await;
    completed_zero_setting(&h).await;
    levels(&mut h, None, Some(0));
    assert!(
        prepare(&h).await.is_none(),
        "durable zero cannot replace physical feedback"
    );
    let parsed =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    for (boot, epoch) in [
        (1, h.generation.epochs.transport),
        (
            uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
            h.generation.epochs.transport + 1,
        ),
    ] {
        h.feedback = Feedback::new(&parsed, ExecutionContext::Simulation, boot, epoch).unwrap();
        h.feedback
            .apply(&GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: epoch,
                received_at_ms: h.actor.monotonic_ms(),
                result: hb_transport::Receive::Frames {
                    transport_epoch: epoch,
                    frames: vec![
                        hb_protocol::RawFrame {
                            address: 0x15,
                            command: 0xd1,
                            value: 100,
                        },
                        hb_protocol::RawFrame {
                            address: 0x15,
                            command: 0xd2,
                            value: 0,
                        },
                    ],
                },
            })
            .unwrap();
        let proof = prepare(&h).await.unwrap();
        assert_eq!(
            h.actor
                .initialize_scene_selection(h.controller, h.generation.clone(), proof)
                .await,
            Err(hap::server::WriteStatus::ResourceBusy)
        );
        assert_eq!(h.selection().await.stored.version, 0);
    }
    h.close().await;
}
#[tokio::test]
async fn initialized_member_hap_web_projection_and_first_off_use_the_reviewed_shutdown() {
    use crate::automation_control::{Action, Reply};
    let (config, rules) = inputs(true);
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(100), Some(0));
    initialize(&h).await;
    h.no_output();
    let source = crate::scene_selection_hap::Source::new(
        h.actor.clone(),
        h.controller,
        h.document.scene_switch_groups.clone(),
    )
    .await
    .unwrap();
    assert_eq!(source.sample().await.unwrap()["first"], Some(true));
    let (control, mut receiver) =
        crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    let reply = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        control.request(Action::SceneSelectionDetails {
            controller: h.controller,
            revision: 1,
        }),
    )
    .await
    .unwrap();
    let Reply::SceneSelectionDetails(details) = reply else {
        panic!("wrong projection");
    };
    assert_eq!(details["first"].selected, Some(true));
    assert_eq!(details["first"].selection_source, "light_initialization");
    use hap::storage::Storage;
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let mut hap = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        ["first", "second", "third"]
            .map(|id| homesense_hap::scene_graph::SceneSpec {
                id: id.into(),
                name: id.into(),
            })
            .into(),
        control,
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    let binding = hap
        .accessories
        .iter()
        .find(|(binding, _)| binding.scene == "first")
        .unwrap()
        .0
        .clone();
    let handle = hap
        .accessories
        .iter()
        .find(|(binding, _)| binding.scene == "first")
        .unwrap()
        .1
        .clone();
    {
        let mut accessory = handle.lock().await;
        let characteristic = accessory
            .get_mut_services()
            .into_iter()
            .flat_map(|service| service.get_mut_characteristics())
            .find(|characteristic| characteristic.get_id() == binding.on_iid)
            .unwrap();
        assert_eq!(characteristic.get_value().await.unwrap(), json!(true));
    }
    let body = json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":false}]})
        .to_string();
    poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        hap.write_handler.write_async(h.controller, body.as_bytes()),
    )
    .await
    .unwrap();
    assert_eq!(hap.flush_resets().await.unwrap(), 0); // No momentary action reset.
    for _ in 0..100 {
        scenes.tick(h.actor.monotonic_ms()).await.unwrap();
        if let Some(ticket) = h.actor.dispatch().await.unwrap() {
            h.actor.finish_write(ticket.command.id, true).await.unwrap();
        }
        let completed: bool = h
            .sql()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scene_run WHERE status='commands_written')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE status='written' AND intent=X'01020000'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    let definition: String = h
        .sql()
        .query_row("SELECT definition FROM scene_run", [], |row| row.get(0))
        .unwrap();
    assert_eq!(definition, "off");
    let snapshot = h.selection().await;
    assert_eq!(snapshot.stored.value, Some(Value::Integer(0)));
    assert!(
        snapshot.initialization.is_none(),
        "manual update cannot keep old physical origin"
    );
    assert_eq!(source.sample().await.unwrap()["first"], Some(false));
    scenes.stop().await;
    h.close().await;
}

#[tokio::test]
async fn prior_zero_setting_does_not_cancel_new_physical_initialization_through_led_reconciliation()
{
    let (config, mut rules) = inputs(true);
    rules["indicator_links"] = json!([{"id":"first-off","enabled":true,"review_evidence":"synthetic link","scene":"first","lights":["light","down"],"leds":["key1"]}]);
    let mut h = harness(config, rules).await;
    completed_zero_setting(&h).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(100), Some(0));
    initialize(&h).await;
    let before: i64 = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, 1);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(100), Some(0));
    let scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    crate::indicator_sync::Synchronizer::default()
        .tick(
            &h.actor,
            h.controller,
            &h.document,
            &scenes,
            &h.feedback,
            5000,
        )
        .await
        .unwrap();
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    assert!(h.selection().await.initialization.is_some());
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    h.close().await;
}
#[tokio::test]
async fn existing_local_record_keeps_historical_origin_on_restart_without_initialization_outputs() {
    let (config, rules) = inputs(true);
    let mut h = harness(config.clone(), rules).await;
    levels(&mut h, Some(100), Some(0));
    initialize(&h).await;
    let original = h.selection().await.initialization.unwrap();
    h.actor.clone().shutdown().await.unwrap();
    let caller = h.controller;
    h.actor = Admission::open(
        h.directory.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == caller,
    )
    .await
    .unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    h.feedback = Feedback::new(
        &homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap(),
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        h.generation.epochs.transport,
    )
    .unwrap();
    assert!(prepare(&h).await.is_none());
    let snapshot = h.selection().await;
    assert_eq!(snapshot.stored.value, Some(Value::Integer(1)));
    assert_eq!(snapshot.initialization, Some(original));
    h.no_output();
    h.close().await;
}

#[tokio::test]
async fn stale_feedback_generation_and_readiness_guards_prevent_initialization() {
    let (config, mut rules) = inputs(true);
    rules["scene_switch_groups"][0]["initialization"]["max_age_ms"] = json!(1);
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(0), Some(0));
    tokio::time::sleep(Duration::from_millis(3)).await;
    assert!(prepare(&h).await.is_none());
    h.no_output();
    h.close().await;
    let (config, rules) = inputs(true);
    let mut h = harness(config.clone(), rules).await;
    levels(&mut h, Some(100), Some(0));
    let proof = prepare(&h).await.unwrap();
    let mut changed = h.generation.clone();
    changed.epochs.control += 1;
    assert_eq!(
        h.actor
            .initialize_scene_selection(h.controller, changed, proof)
            .await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    let proof = prepare(&h).await.unwrap();
    let mut store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
    let parsed =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    assert_eq!(
        proof.commit(
            &mut store,
            &parsed,
            &h.document,
            h.actor.boot_id(),
            h.generation.epochs,
            h.actor.monotonic_ms() + 5000
        ),
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    drop(store);
    let proof = prepare(&h).await.unwrap();
    h.actor.transport_failed().await.unwrap();
    assert_eq!(
        h.actor
            .initialize_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    let raw: (Option<i64>, i64) = h
        .sql()
        .query_row(
            "SELECT value,version FROM logical_variable WHERE name='selection'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw, (None, 0));
    h.no_output();
    h.close().await;
}

#[path = "scene_reconciliation_tests.rs"]
mod reconciliation;
