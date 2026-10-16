//! Exact synthetic cross-writer sequence through timed scene and memory bus.
use super::*;
pub(super) async fn drain_all(h: &Harness) -> Vec<[u8; 5]> {
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut owner = TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(Wire(bytes.clone()), h.generation.epochs.transport),
        )]
        .into(),
    );
    while let Some(out) = owner.step().await.unwrap() {
        out.result.unwrap();
    }
    bytes.lock().unwrap().clone()
}
#[tokio::test]
async fn timed_off_override_then_external_on_reconciles_one_or_many_leds_once() {
    for multi in [false, true] {
        let (config, mut rules) = linked();
        if !multi {
            rules["indicator_links"][0]["leds"] = json!(["key1"]);
        }
        let targets = if multi {
            vec!["key1", "other-led"]
        } else {
            vec!["key1"]
        };
        for id in &targets {
            rules["timed_scenes"][3]["steps"].as_array_mut().unwrap().push(json!({"device":id,"offset_ms":0,"source":"synthetic timed off LED","command":{"kind":"panel_led","on":false}}));
        }
        let mut h = harness(config, rules).await;
        let mut s = Synchronizer::default();
        levels(&mut h, Some(100), Some(0));
        reconcile(&h).await;
        sync(&h, &mut s).await;
        let initial = drain_leds(&h).await;
        assert!(initial[&(0x41, 1)]);
        if multi {
            assert!(initial[&(0x42, 1)]);
        }
        let mut scenes =
            SceneScheduler::new(h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
        scenes
            .start_document(&h.document, "off", h.actor.monotonic_ms())
            .await
            .unwrap();
        let mut off = vec![];
        for _ in 0..8 {
            scenes.tick(h.actor.monotonic_ms()).await.unwrap();
            off.extend(drain_all(&h).await);
            if scenes.publication_impact().is_empty() {
                break;
            }
        }
        assert!(!off.iter().any(|frame|[0x41,0x42].contains(&frame[1])),"timed success LEDs stay Accepted before real Off feedback");
        // The synchronizer deliberately does not see the brief Off interval.
        tokio::time::sleep(Duration::from_millis(2)).await;
        levels(&mut h, Some(0), Some(0));
        // Normal feedback settlement is independent of LED synchronization.
        // This closes the dimmer Off target without delivering an Off LED edge.
        crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();off.extend(drain_all(&h).await);
        assert!(
            off.contains(&[0xae, 0x41, 1, 0xf2, 0xee]),
            "actual timed off frames {off:?}"
        );
        if multi {
            assert!(off.contains(&[0xae, 0x42, 1, 0xf2, 0xee]));
        }
        scenes.tick(h.actor.monotonic_ms()).await.unwrap();
        h.actor.remember_levels(1,h.feedback.snapshot(),5000).await.unwrap();
        levels(&mut h, Some(100), Some(0));
        reconcile(&h).await;
        assert_eq!(h.selection().await.stored.value, Some(Value::Integer(1)));
        sync(&h, &mut s).await;
        let corrected = drain_leds(&h).await;
        assert_eq!(
            corrected.get(&(0x41, 1)),
            Some(&true),
            "other writer's Written green must invalidate obsolete red attempt suppression"
        );
        if multi {
            assert_eq!(corrected.get(&(0x42, 1)), Some(&true));
        }
        for _ in 0..4 {
            levels(&mut h, Some(100), Some(0));
            reconcile(&h).await;
            sync(&h, &mut s).await;
            assert!(
                drain_leds(&h).await.is_empty(),
                "equal polling must not generate repeated output"
            );
        }
        h.close().await;
    }
}

#[tokio::test]
async fn semantic_off_on_without_local_led_journal_is_a_new_physical_cause() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    let before = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    levels(&mut h, Some(0), Some(0));
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    sync(&h, &mut s).await;
    let frames = drain_leds(&h).await;
    assert_eq!(frames.get(&(0x41, 1)), Some(&true));
    assert_eq!(frames.get(&(0x42, 1)), Some(&true));
    sync(&h, &mut s).await;
    assert!(drain_leds(&h).await.is_empty());
    h.close().await;
}
#[tokio::test]
async fn led_only_complete_override_corrects_even_without_lamp_change() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    drain_leds(&h).await;
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            homesense_hap::requests::Batch {
                controller: h.controller,
                intents: ["key1", "other-led"]
                    .into_iter()
                    .map(|id| homesense_hap::requests::Intent {
                        device: homesense_core::DeviceId::new(id).unwrap(),
                        command: Command::PanelLed(false),
                    })
                    .collect(),
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let off = drain_leds(&h).await;
    assert_eq!(off.get(&(0x41, 1)), Some(&false));
    sync(&h, &mut s).await;
    let corrected = drain_leds(&h).await;
    assert_eq!(corrected.get(&(0x41, 1)), Some(&true));
    assert_eq!(corrected.get(&(0x42, 1)), Some(&true));
    sync(&h, &mut s).await;
    assert!(drain_leds(&h).await.is_empty());
    h.close().await;
}
#[tokio::test]
async fn accepted_attempt_is_not_written_and_same_target_is_not_requeued() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    let before = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE status='written'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    levels(&mut h, Some(0), Some(0));
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    assert_eq!(
        h.sql()
            .query_row("SELECT count(*) FROM command_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(
        h.feedback.snapshot().devices[&homesense_core::DeviceId::new("key1").unwrap()]
            .observed
            .is_none()
    );
    drain_leds(&h).await;
    h.close().await;
}
#[tokio::test]
async fn incomplete_write_remains_uncertain_and_never_replays() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    levels(&mut h, Some(100), Some(0));
    reconcile(&h).await;
    sync(&h, &mut s).await;
    let before = h
        .sql()
        .query_row("SELECT count(*) FROM command_journal", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    let ticket = h.actor.dispatch().await.unwrap().unwrap();
    h.actor
        .finish_write(ticket.command.id, false)
        .await
        .unwrap();
    let scenes =
        SceneScheduler::new(h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
    for _ in 0..3 {
        let _ = s
            .tick(
                &h.actor,
                h.controller,
                &h.document,
                &scenes,
                &h.feedback,
                5000,
            )
            .await;
    }
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
                "SELECT count(*) FROM command_journal WHERE status='written'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    h.close().await;
}
