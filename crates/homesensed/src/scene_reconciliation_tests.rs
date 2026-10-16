//! Current-boot truth only, synthetic feedback and no physical IO.
use super::*;
use crate::scene_reconciliation::Prepared as Projection;
fn reconciled() -> (Vec<u8>, Json) {
    let (config, mut rules) = inputs(false);
    rules["scene_switch_groups"][0]["reconciliation"] = json!({"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"synthetic reviewed logical light union"});
    (config, rules)
}
async fn reconcile(h: &Harness) {
    let scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    crate::scene_reconciliation::tick(&h.actor, h.controller, &h.document, &scenes, &h.feedback)
        .await
        .unwrap();
}
async fn projection(h: &Harness) -> Projection {
    Projection::prepare(
        &h.actor,
        h.controller,
        h.generation.clone(),
        &h.document.scene_switch_groups[0],
        &h.document,
        &h.feedback,
    )
    .await
    .unwrap()
    .unwrap()
}
fn history(h: &Harness, value: i64) {
    let mut store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
    let version = store
        .logical_variable(1, "selection")
        .unwrap()
        .unwrap()
        .version;
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
                expected_version: version,
                value: Some(Value::Integer(value)),
            }],
        )
        .unwrap();
}
async fn source(h: &Harness) -> crate::scene_selection_hap::Source {
    crate::scene_selection_hap::Source::new(
        h.actor.clone(),
        h.controller,
        h.document.scene_switch_groups.clone(),
    )
    .await
    .unwrap()
}
#[test]
fn binary_builder_normalizes_only_reviewed_union_and_requires_explicit_review() {
    let (config, mut rules) = reconciled();
    rules["timed_scenes"][0]["steps"]
        .as_array_mut()
        .unwrap()
        .pop();
    rules["timed_scenes"][2]["steps"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let config =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    let document = Document::parse(&serde_json::to_vec(&rules).unwrap(), &config).unwrap();
    let patterns = document.scene_switch_groups[0]
        .light_on_patterns(&document.timed_scenes)
        .unwrap();
    use homesense_core::scene_pattern::LightPower::{Off, On};
    assert_eq!(
        patterns[&1]
            .keys()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["down", "light"]
    );
    assert_eq!(
        patterns[&1].values().copied().collect::<Vec<_>>(),
        [Off, On]
    );
    assert_eq!(
        patterns[&3].values().copied().collect::<Vec<_>>(),
        [On, Off]
    );
    for (field, value) in [
        ("max_age_ms", json!(0)),
        ("review_evidence", json!("")),
        ("policy", json!("guess")),
    ] {
        let mut invalid = rules.clone();
        invalid["scene_switch_groups"][0]["reconciliation"][field] = value;
        assert!(Document::parse(&serde_json::to_vec(&invalid).unwrap(), &config).is_err());
    }
    let mut invalid = rules.clone();
    invalid["scene_switch_groups"][0]["initialization"]["enabled"] = json!(true);
    assert!(Document::parse(&serde_json::to_vec(&invalid).unwrap(), &config).is_err());
    rules["scene_switch_groups"][0]["reconciliation"]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    assert!(
        !Document::parse(&serde_json::to_vec(&rules).unwrap(), &config)
            .unwrap()
            .scene_switch_groups[0]
            .reconciliation
            .as_ref()
            .unwrap()
            .enabled
    );
}
#[tokio::test]
async fn startup_masks_saved_intent_before_tick_and_36_vectors_use_power_only() {
    for main in [None, Some(0), Some(17), Some(30), Some(50), Some(100)] {
        for down in [None, Some(0), Some(17), Some(30), Some(50), Some(100)] {
            let (config, rules) = reconciled();
            let mut h = harness(config, rules).await;
            history(&h, 1);
            levels(&mut h, main, down);
            let src = source(&h).await;
            assert!(
                src.sample().await.unwrap().values().all(Option::is_none),
                "must mask history even with samples before first tick"
            );
            reconcile(&h).await;
            let expected = match (main, down) {
                (Some(0), Some(0)) => Some(0),
                (Some(m), Some(0)) if m > 0 => Some(1),
                (Some(m), Some(d)) if m > 0 && d > 0 => Some(2),
                (Some(0), Some(d)) if d > 0 => Some(3),
                _ => None,
            };
            let snapshot = h.selection().await;
            assert_eq!(
                snapshot.stored.value,
                expected.map(Value::Integer),
                "main={main:?}, down={down:?}"
            );
            assert_eq!(
                snapshot.physical.unwrap().history_value,
                Some(Value::Integer(1))
            );
            let sampled = src.sample().await.unwrap();
            for (id, value) in [("first", 1), ("second", 2), ("third", 3)] {
                assert_eq!(sampled[id], expected.map(|v| v == value));
            }
            assert_eq!(
                h.sql()
                    .query_row(
                        "SELECT value FROM logical_variable WHERE name='selection'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            h.no_output();
            h.close().await;
        }
    }
}
#[tokio::test]
async fn restart_and_reconnect_reject_old_projection_before_new_samples() {
    let (config, rules) = reconciled();
    let mut h = harness(config.clone(), rules).await;
    history(&h, 1);
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    let old_proof = projection(&h).await;
    let old_boot = h.actor.boot_id().to_owned();
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
    assert_ne!(h.actor.boot_id(), old_boot);
    h.generation = h.actor.task_generation().await.unwrap();
    assert_eq!(
        h.actor
            .reconcile_scene_selection(h.controller, h.generation.clone(), old_proof)
            .await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    let parsed =
        homesense_config::Configuration::parse(&config, ExecutionContext::Simulation).unwrap();
    h.feedback = Feedback::new(
        &parsed,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),
        h.generation.epochs.transport,
    )
    .unwrap();
    assert!(
        source(&h)
            .await
            .sample()
            .await
            .unwrap()
            .values()
            .all(Option::is_none)
    );
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, None);
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    assert!(
        source(&h)
            .await
            .sample()
            .await
            .unwrap()
            .values()
            .all(|v| *v == Some(false))
    );
    h.actor.transport_failed().await.unwrap();
    h.actor.recover_synchronized(2, 1).await.unwrap();
    h.generation = h.actor.task_generation().await.unwrap();
    h.feedback.invalidate().unwrap();
    h.feedback.resume(2, h.actor.monotonic_ms()).unwrap();
    let src = source(&h).await;
    assert!(src.sample().await.unwrap().values().all(Option::is_none));
    levels(&mut h, Some(0), Some(30));
    assert!(src.sample().await.unwrap().values().all(Option::is_none));
    reconcile(&h).await;
    assert_eq!(src.sample().await.unwrap()["third"], Some(true));
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT value FROM logical_variable WHERE name='selection'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn changed_feedback_stale_lease_and_manual_version_invalidate_current_truth() {
    let (config, mut rules) = reconciled();
    rules["scene_switch_groups"][0]["reconciliation"]["max_age_ms"] = json!(100);
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    levels(&mut h, Some(0), Some(30));
    assert_eq!(h.selection().await.stored.value, None);
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(3)));
    let proof = projection(&h).await;
    history(&h, 2);
    assert_eq!(
        h.actor
            .reconcile_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    assert_eq!(h.selection().await.stored.value, None);
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(3)));
    tokio::time::sleep(Duration::from_millis(110)).await;
    assert!(
        source(&h)
            .await
            .sample()
            .await
            .unwrap()
            .values()
            .all(Option::is_none)
    );
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn ambiguous_patterns_and_missing_hidden_relay_remain_unknown() {
    let (config, mut rules) = reconciled();
    rules["timed_scenes"][1]["steps"][1]["command"]["value"] = json!(0);
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, None);
    h.no_output();
    h.close().await;
    let (config, rules) = reconciled();
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["devices"][0]["supply_relay"] = json!("switch");
    config["devices"][0]["default_on_level"] = json!(100);
    let mut h = harness(serde_json::to_vec(&config).unwrap(), rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, None);
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
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    h.no_output();
    h.close().await;
}
#[tokio::test]
async fn pending_output_cannot_publish_precommand_feedback_as_settled() {
    let (config, rules) = reconciled();
    let mut h = harness(config, rules).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: homesense_core::DeviceId::new("light").unwrap(),
                    command: homesense_core::Command::SetLevel {
                        level: homesense_core::Percent::new(0).unwrap(),
                        transition: homesense_core::LevelTransition::Immediate,
                    },
                }],
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(h.selection().await.stored.value, None);
    let proof = projection(&h).await;
    assert_eq!(
        h.actor
            .reconcile_scene_selection(h.controller, h.generation.clone(), proof)
            .await,
        Err(hap::server::WriteStatus::ResourceBusy)
    );
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    h.actor.finish_write(ticket.command.id, true).await.unwrap();
    assert_eq!(h.selection().await.stored.value, None);
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, None);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(0), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(0)));
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    h.close().await;
}
#[tokio::test]
async fn actual_hap_cache_and_web_details_distinguish_history_from_current_feedback() {
    let (config, rules) = reconciled();
    let mut h = harness(config, rules).await;
    history(&h, 1);
    let (control, mut receiver) =
        crate::automation_control::channel(h.controller, h.generation.clone());
    let mut scenes = crate::scene_scheduler::SceneScheduler::new(
        h.actor.clone(),
        h.controller,
        8,
        h.actor.monotonic_ms(),
    )
    .unwrap();
    use hap::storage::Storage;
    let mut identity = homesense_hap::storage::IdentityStore::open(h.directory.join("identity"))
        .await
        .unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let mut hap = crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
        &identity,
        &h.document,
        ["first", "second", "third"]
            .into_iter()
            .map(|id| homesense_hap::scene_graph::SceneSpec {
                id: id.into(),
                name: id.into(),
            })
            .collect(),
        control.clone(),
        h.actor.clone(),
        h.controller,
        |_, _| true,
    )
    .await
    .unwrap();
    async fn read(
        hap: &crate::runtime_scene_hap::RuntimeSceneHap,
    ) -> hap::Result<serde_json::Value> {
        let (binding, handle) = hap
            .accessories
            .iter()
            .find(|(b, _)| b.scene == "first")
            .unwrap();
        let mut accessory = handle.lock().await;
        accessory
            .get_mut_services()
            .into_iter()
            .flat_map(|s| s.get_mut_characteristics())
            .find(|c| c.get_id() == binding.on_iid)
            .unwrap()
            .get_value()
            .await
    }
    assert!(read(&hap).await.is_err());
    levels(&mut h, Some(0), Some(30));
    reconcile(&h).await;
    hap.flush_resets().await.unwrap();
    assert_eq!(read(&hap).await.unwrap(), json!(false));
    let reply = poll_request(
        &h,
        &mut receiver,
        &mut scenes,
        control.request(crate::automation_control::Action::SceneSelectionDetails {
            controller: h.controller,
            revision: 1,
        }),
    )
    .await
    .unwrap();
    let crate::automation_control::Reply::SceneSelectionDetails(details) = reply else {
        panic!("wrong reply")
    };
    assert_eq!(details["first"].selected, Some(false));
    assert_eq!(details["first"].historical_selected, Some(true));
    assert_eq!(details["first"].selection_source, "physical_feedback");
    assert_eq!(details["third"].selected, Some(true));
    use crate::web_auth::{Grant, Role, Tokens};
    use http_body_util::BodyExt;
    let tokens = std::sync::Arc::new(Tokens::default());
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            uuid::Uuid::new_v4(),
            secret,
            Grant {
                controller: h.controller,
                role: Role::Viewer,
                scenes: ["first".into()].into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    let api = crate::web_api::Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://127.0.0.1:8088".into(),
        tokens,
        runtime: std::sync::RwLock::new(Some(crate::web_api::Runtime {
            control: control.clone(),
            revision: 1,
            boot: h.actor.boot_id().into(),
            scenes: [
                ("first".into(), "First".into()),
                ("third".into(), "Third".into()),
            ]
            .into(),
        })),
        receipts: std::sync::Mutex::new(
            crate::web_receipts::Receipts::open(&h.directory.join("web.sqlite")).unwrap(),
        ),
        drafts: None,
    };
    let request = hyper::Request::builder()
        .uri("/api/v1/scenes")
        .header("host", "127.0.0.1:8088")
        .header("authorization", format!("Bearer {secret}"))
        .body(hyper::body::Bytes::new())
        .unwrap();
    let response = poll_request(&h, &mut receiver, &mut scenes, api.handle(request)).await;
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let payload: Json =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(payload["scenes"][0]["selected"], false);
    assert_eq!(payload["scenes"][0]["historical_selected"], true);
    assert_eq!(
        payload["scenes"][0]["selection_source"],
        "physical_feedback"
    );
    assert!(!payload.to_string().contains("key1"));
    h.no_output();
    drop(hap);
    h.close().await;
}

#[tokio::test]
async fn physical_scene_projection_reuses_same_all_off_sample_for_leds_and_ignores_old_zero_intent()
{
    for (main, remembered, expected_leds) in [(0, 0, 1), (0, 80, 1), (30, 0, 1)] {
        let (config, mut rules) = reconciled();
        rules["indicator_links"] = json!([{"id":"first-off","enabled":true,"review_evidence":"synthetic link","scene":"first","lights":["light","down"],"leds":["key1"]}]);
        let mut h = harness(config, rules).await;
        h.actor
            .submit_rule_update(
                h.generation.clone(),
                homesense_hap::requests::Batch {
                    controller: h.controller,
                    intents: vec![homesense_hap::requests::Intent {
                        device: homesense_core::DeviceId::new("light").unwrap(),
                        command: homesense_core::Command::SetLevel {
                            level: homesense_core::Percent::new(remembered).unwrap(),
                            transition: homesense_core::LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                },
                vec![],
            )
            .await
            .unwrap();
        let ticket = loop {
            if let Some(ticket) = h.actor.dispatch().await.unwrap() {
                break ticket;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        h.actor.finish_write(ticket.command.id, true).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        // A written setpoint remains an owned transition until fresh feedback
        // confirms it; this case exercises a later independent physical edge.
        levels(&mut h,Some(remembered),Some(0));
        h.actor.remember_levels(1,h.feedback.snapshot(),5000).await.unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        history(&h, 1);
        levels(&mut h, Some(main), Some(0));
        reconcile(&h).await;
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
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE device_id='key1'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            expected_leds
        );
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT value FROM logical_variable WHERE name='selection'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(h.selection().await.stored.value, Some(Value::Integer(if main>0 {1} else {0})),"queued LED-only output must not invalidate settled light truth");
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT count(*) FROM command_journal WHERE device_id='light'",
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
async fn external_candidate_stays_independent_and_prekey_samples_cannot_supply_truth() {
    let (config, rules) = reconciled();
    let mut h = harness(config, rules).await;
    history(&h, 1);
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    h.tick().await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    h.press(1).await;
    let snapshot = h.selection().await;
    assert_eq!(
        snapshot.evidence.as_ref().unwrap().basis,
        homesense_core::scene_observer::Basis::UnknownInitial
    );
    assert_eq!(snapshot.stored.value, None);
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, None);
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h, Some(30), Some(0));
    reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
    assert_eq!(
        h.selection().await.evidence.unwrap().basis,
        homesense_core::scene_observer::Basis::UnknownInitial
    );
    h.no_output();
    h.close().await;
}

#[path = "scene_indicator_projection_tests.rs"]
mod indicator_projection;

#[path="scene_brightness_reconciliation_tests.rs"]
mod brightness;

#[path = "scene_projection_cache_tests.rs"]
mod cache_tests;

#[tokio::test]
async fn explicit_off_refreshes_versioned_physical_proof_without_using_saved_intent() {
    let (config,rules)=reconciled();
    let mut h=harness(config,rules).await;
    levels(&mut h,Some(100),Some(0));
    reconcile(&h).await;
    // A completed ON advances intent version. Its old projection is masked,
    // although the same current settled physical vector still matches.
    history(&h,1);
    assert_eq!(h.selection().await.stored.value,None);
    let (control,mut receiver)=crate::automation_control::channel(h.controller,h.generation.clone());
    let mut scenes=crate::scene_scheduler::SceneScheduler::new(h.actor.clone(),h.controller,8,h.actor.monotonic_ms()).unwrap();
    let reply=poll_request(&h,&mut receiver,&mut scenes,control.request(crate::automation_control::Action::SceneAction{controller:h.controller,revision:1,scene:"first".into(),execute:false})).await.unwrap();
    assert!(matches!(reply,crate::automation_control::Reply::SceneAccepted(Some(_))));
    h.close().await;
}

#[tokio::test]
async fn scene_catalog_reads_seventeen_scenes_in_one_control_request_and_rejects_bad_revision() {
    let (config,mut rules)=reconciled();
    let template=rules["timed_scenes"][0].clone();
    for n in 0..13 {
        let mut scene=template.clone();scene["id"]=json!(format!("directory-{n}"));
        rules["timed_scenes"].as_array_mut().unwrap().push(scene);
    }
    let mut h=harness(config,rules).await;
    levels(&mut h,Some(100),Some(0));
    let (control,mut receiver)=crate::automation_control::channel(h.controller,h.generation.clone());
    let mut scenes=crate::scene_scheduler::SceneScheduler::new(h.actor.clone(),h.controller,8,h.actor.monotonic_ms()).unwrap();
    let ids=h.document.timed_scenes.keys().cloned().collect::<Vec<_>>();
    assert_eq!(ids.len(),17);
    let reply=poll_request(&h,&mut receiver,&mut scenes,control.request(crate::automation_control::Action::SceneCatalog{controller:h.controller,revision:1,scenes:ids.clone()})).await.unwrap();
    let crate::automation_control::Reply::SceneCatalog(entries)=reply else{panic!("catalog reply")};
    assert_eq!(entries.len(),17);
    assert!(entries.values().all(|v|v.tracking.is_none()&&v.last_run.is_none()));
    assert!(poll_request(&h,&mut receiver,&mut scenes,control.request(crate::automation_control::Action::SceneCatalog{controller:h.controller,revision:2,scenes:ids})).await.is_err());
    h.no_output();h.close().await;
}
