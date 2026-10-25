//! Real runtime/Admission projections; no sockets or household connection.
use super::*;

async fn tick(h: &mut Harness) {
    h.receiver
        .poll_causal(
            &h.actor,
            &h.document,
            &mut h.scheduler,
            &h.feedback,
            5000,
            None,
            Some(&mut h.router),
        )
        .await;
}
async fn enable(h: &mut Harness) -> tokio::sync::watch::Sender<bool> {
    let (stop, receive) = tokio::sync::watch::channel(false);
    h.receiver
        .bind_selection_reader(&h.actor, &h.document, receive)
        .await
        .unwrap();
    h.receiver.enable_current_model().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while h.receiver.current_model_samples() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server owner failed initial publication"
        );
        tick(h).await;
    }
    stop
}
fn devices(h: &Harness, controller: Uuid) -> Action {
    Action::Devices {
        controller,
        revision: 1,
        ids: h.configuration.devices().keys().cloned().collect(),
    }
}

#[tokio::test]
async fn zero_one_and_many_consumers_never_start_projection_or_bus_work() {
    let mut counts = vec![];
    for clients in [0, 1, 24] {
        let mut h = Harness::new(rules()).await;
        h.samples(true);
        let _stop = enable(&mut h).await;
        let before = h.receiver.current_model_samples();
        let until = tokio::time::Instant::now() + Duration::from_millis(850);
        while tokio::time::Instant::now() < until {
            tick(&mut h).await;
            for _ in 0..clients {
                let started = tokio::time::Instant::now();
                assert!(matches!(
                    h.control.request(devices(&h, h.member)).await.unwrap(),
                    Reply::Devices(_)
                ));
                assert!(matches!(
                    h.control
                        .request(Action::SceneCatalog {
                            controller: h.member,
                            revision: 1,
                            scenes: vec!["act".into()]
                        })
                        .await
                        .unwrap(),
                    Reply::SceneCatalog(_)
                ));
                assert!(started.elapsed() < Duration::from_millis(500));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let count = h.receiver.current_model_samples() - before;
        counts.push(count);
        assert!(count >= 2, "sampling must continue with no clients");
        assert_eq!(
            h.count("command_journal"),
            0,
            "subscriptions cannot issue hardware queries or controls"
        );
        let frozen = h.receiver.current_model_samples();
        for _ in 0..100 {
            h.control.request(devices(&h, h.member)).await.unwrap();
        }
        assert_eq!(
            h.receiver.current_model_samples(),
            frozen,
            "reads cannot refresh the producer"
        );
        assert!(h.wire().await.is_empty());
        h.finish().await;
    }
    assert!(
        counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1,
        "sample count scales with clients: {counts:?}"
    );
}

#[tokio::test]
async fn cached_reads_recheck_authority_stop_transport_and_age_without_refreshing() {
    let mut h = Harness::new(rules()).await;
    let stop = enable(&mut h).await;
    let before = h.receiver.current_model_samples();
    h.allowed.store(false, Ordering::Release);
    assert!(h.control.request(devices(&h, h.member)).await.is_err());
    h.allowed.store(true, Ordering::Release);
    h.control.request(devices(&h, h.member)).await.unwrap();
    assert!(
        h.control
            .request(Action::Devices {
                controller: h.member,
                revision: 2,
                ids: vec![]
            })
            .await
            .is_err()
    );
    assert!(
        h.control
            .request(Action::Devices {
                controller: h.member,
                revision: 1,
                ids: vec![homesense_core::DeviceId::new("unpublished").unwrap()]
            })
            .await
            .is_err()
    );
    assert!(
        h.control
            .request(devices(&h, Uuid::new_v4()))
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(1520)).await;
    assert!(
        h.control.request(devices(&h, h.member)).await.is_err(),
        "a stalled producer cannot relabel old state as current"
    );
    assert_eq!(h.receiver.current_model_samples(), before);
    tick(&mut h).await;
    while h.receiver.current_model_samples() == before {
        tick(&mut h).await;
    }
    h.control.request(devices(&h, h.member)).await.unwrap();
    stop.send(true).unwrap();
    assert!(h.control.request(devices(&h, h.member)).await.is_err());
    assert!(h.wire().await.is_empty());
    h.finish().await;

    let mut h = Harness::new(rules()).await;
    let _stop = enable(&mut h).await;
    h.actor.transport_failed().await.unwrap();
    assert!(h.control.request(devices(&h, h.member)).await.is_err());
    h.finish().await;
}

#[tokio::test]
async fn partial_keypad_scope_matches_original_projection_and_hides_other_keys() {
    let (cfg, doc) = writer_batch_inputs();
    let mut cfg: Json = serde_json::from_slice(&cfg).unwrap();
    for number in 4..=6 {
        cfg["keypads"][0]["keys"].as_array_mut().unwrap().push(json!({"number":number,"label":format!("Hidden private key {number}"),"led_endpoint":{"id":format!("secret-key-{number}"),"name":format!("Secret {number}"),"enabled":true}}));
    }
    let mut h = Harness::from_inputs(serde_json::to_vec(&cfg).unwrap(), doc).await;
    let _stop = enable(&mut h).await;
    let ids = vec![homesense_core::DeviceId::new("execute-led").unwrap()];
    let Reply::Devices(view) = h
        .control
        .request(Action::Devices {
            controller: h.member,
            revision: 1,
            ids: ids.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let original = crate::management_devices::snapshot_with_max_age(
        &h.actor,
        h.member,
        h.generation.clone(),
        ids,
        &h.feedback,
        5000,
    )
    .await
    .unwrap();
    assert_eq!(view.keypads, original.keypads);
    assert_eq!(view.keypad_endpoints, original.keypad_endpoints);
    assert!(
        view.light_publication.is_none(),
        "unscoped HAP state must not travel in a scoped response"
    );
    assert_eq!(view.devices.len(), 1);
    assert!(view.household_operations.is_empty());
    assert_eq!(view.keypads[0]["settings"]["scope_complete"], false);
    let Reply::Devices(mut complete) = h.control.request(devices(&h, h.member)).await.unwrap()
    else {
        panic!()
    };
    complete.keypads[0]["settings"]["backlight"]["last_written"] =
        json!({"value":"private-setting"});
    complete.keypads[0]["keys"][3]["led"]["last_written"] = json!({"value":"private-led"});
    complete.keypads[0]["keys"][3]["indication"] = json!({"value":"private-indication"});
    crate::management_keypads::scope_current(
        &mut complete.keypads,
        &view.keypad_endpoints,
        h.actor.monotonic_ms(),
    );
    assert_eq!(
        complete.keypads, original.keypads,
        "cached partial scope cannot disclose whole-panel settings or another key's records"
    );
    h.finish().await;
}

#[tokio::test]
async fn shared_model_reads_do_not_delay_real_control_admission() {
    let mut h = Harness::new(rules()).await;
    h.samples(true);
    let _stop = enable(&mut h).await;
    let mut clients = vec![];
    for _ in 0..24 {
        let control = h.control.clone();
        let action_controller = h.member;
        clients.push(tokio::spawn(async move {
            for _ in 0..20 {
                control
                    .request(Action::SceneCatalog {
                        controller: action_controller,
                        revision: 1,
                        scenes: vec!["act".into()],
                    })
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
            }
        }));
    }
    let started = tokio::time::Instant::now();
    h.request(Action::SceneAction {
        controller: h.member,
        revision: 1,
        scene: "act".into(),
        execute: true,
    })
    .await
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "cached fanout must not occupy the control queue"
    );
    for client in clients {
        client.await.unwrap();
    }
    assert_eq!(h.wire().await.len(), 20);
    h.finish().await;
}

#[tokio::test]
async fn slow_hap_accessory_never_holds_the_common_light_producer() {
    let mut h = Harness::new(rules()).await;
    mode_frame(&mut h, 0x15, 1, 40);
    h.samples(true);
    let _stop = enable(&mut h).await;
    let mut identity =
        homesense_hap::storage::IdentityStore::open(h.directory.join("shared-identity"))
            .await
            .unwrap();
    hap::storage::Storage::save_config(&mut identity, &hap::Config::default())
        .await
        .unwrap();
    let mut initial = h.feedback.snapshot();
    initial
        .devices
        .get_mut(&homesense_core::DeviceId::new("light").unwrap())
        .unwrap()
        .observed = Some(homesense_core::Observation::Level(
        homesense_core::Percent::new(0).unwrap(),
    ));
    let mut graph = crate::runtime_hap::RuntimeHap::new_selected_with_control(
        &h.configuration,
        initial,
        &identity,
        h.actor.clone(),
        5000,
        None,
        Some(h.control.clone()),
    )
    .await
    .unwrap();
    let handle = graph
        .lights
        .iter()
        .find(|(binding, _)| binding.device.as_str() == "light")
        .unwrap()
        .1
        .clone();
    let held = handle.lock().await;
    let physical = h.feedback.snapshot();
    let pending = tokio::spawn(async move {
        graph.publish_snapshot(physical).await.unwrap();
        graph
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !pending.is_finished(),
        "fixture must actually block the HAP consumer"
    );
    let before = h.receiver.current_model_samples();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while h.receiver.current_model_samples() == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "slow HAP lock blocked producer"
        );
        tick(&mut h).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let Reply::Devices(view) = h.control.request(devices(&h, h.member)).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        view.devices
            .iter()
            .find(|view| view.id.as_str() == "light")
            .unwrap()
            .reported,
        Some(homesense_core::Observation::Level(
            homesense_core::Percent::new(40).unwrap()
        ))
    );
    drop(held);
    let graph = tokio::time::timeout(Duration::from_millis(500), pending)
        .await
        .unwrap()
        .unwrap();
    let mut accessory = handle.lock().await;
    let service = accessory.get_mut_service(hap::HapType::Lightbulb).unwrap();
    assert_eq!(
        service
            .get_mut_characteristic(hap::HapType::Brightness)
            .unwrap()
            .get_value()
            .await
            .unwrap(),
        json!(40)
    );
    drop(accessory);
    drop(graph);
    assert!(h.wire().await.is_empty());
    h.finish().await;
}

#[tokio::test]
async fn hap_selection_and_management_read_one_produced_selection() {
    let (cfg, doc) = native_inputs();
    let mut h = Harness::from_inputs(cfg, doc).await;
    set_value(&h, "native.selection", 1).await;
    let _stop = enable(&mut h).await;
    let source = crate::scene_selection_hap::Source::new(
        h.actor.clone(),
        h.controller,
        h.document.scene_switch_groups.clone(),
    )
    .await
    .unwrap()
    .with_actions(h.control.clone(), 1, vec![]);
    let Reply::SceneSelectionDetails(expected) = h
        .control
        .request(Action::SceneSelectionDetails {
            controller: h.member,
            revision: 1,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let before = h.receiver.current_model_samples();
    for _ in 0..24 {
        let values = tokio::time::timeout(Duration::from_millis(500), source.sample())
            .await
            .unwrap()
            .unwrap();
        for (id, detail) in &expected {
            assert_eq!(values.get(id), Some(&detail.selected));
        }
    }
    assert_eq!(
        h.receiver.current_model_samples(),
        before,
        "HAP reads cannot start another projection"
    );
    assert_eq!(h.count("command_journal"), 0);
    h.finish().await;
}

#[tokio::test]
async fn protected_operations_skip_catalog_work_and_publish_before_terminal_wakeup() {
    let mut h = Harness::new(rules()).await;
    let (signal, receive) = tokio::sync::watch::channel(());
    let mut owner = crate::current_read_model::Owner::default();
    owner.bind(
        h.controller,
        h.configuration.clone(),
        h.document.clone(),
        receive,
    );
    owner.observation_age(5000);
    while let Some(action) = owner.next_request(false) {
        let reply = h.request(action).await;
        owner.completed(reply);
    }
    assert_eq!(owner.samples, 1);
    let mut terminal = owner.shared.terminal_updates();
    signal.send_replace(());
    assert!(
        !terminal.has_changed().unwrap(),
        "an actor terminal edge cannot wake clients with an old publication"
    );
    let mut reads = 0;
    while let Some(action) = owner.next_request(true) {
        assert!(
            matches!(action, Action::Diagnostics { .. } | Action::Devices { .. }),
            "protected work cannot be followed by expensive scene reads"
        );
        reads += 1;
        let reply = h.request(action).await;
        owner.completed(reply);
    }
    assert_eq!(reads, 2);
    assert_eq!(owner.samples, 2);
    assert!(terminal.has_changed().unwrap());
    terminal.borrow_and_update();
    let Reply::SceneCatalog(rows) = owner
        .shared
        .reply(
            &Action::SceneCatalog {
                controller: h.member,
                revision: 1,
                scenes: vec!["act".into()],
            },
            h.actor.monotonic_ms(),
            &h.configuration,
            1,
        )
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        rows["act"].activity,
        crate::scene_activity::SceneActivity::Unknown
    );
    assert!(!rows["act"].action_gate.available);
    assert_eq!(h.count("command_journal"), 0);
    h.finish().await;
}
