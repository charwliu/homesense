//! Completed manual timelines must not exhaust a persistent service's slots.
use super::*;

async fn manual(h: &mut Harness) -> u64 {
    match h
        .request(Action::SceneWrites {
            controller: h.member,
            revision: 1,
            scenes: vec![("later".into(), Some(true))],
        })
        .await
        .expect("manual timeline admission")
    {
        Reply::ScenesAccepted(runs) => {
            assert_eq!(runs.len(), 1);
            runs[0]
        }
        other => panic!("unexpected reply: {other:?}"),
    }
}

async fn delivered(h: &mut Harness) -> crate::scene_scheduler::SceneResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let (outcomes, mut results) = h.router.poll(&mut h.scheduler, None).await.unwrap();
        assert!(outcomes.is_empty());
        if !results.is_empty() {
            assert_eq!(results.len(), 1);
            return results.remove(0);
        }
        h.wire().await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal result stalled"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn completed_manual_timelines_can_exceed_capacity_without_losing_history() {
    let mut r = rules();
    r["rules"] = json!([]);
    let mut h = Harness::new(r).await;
    h.scheduler =
        SceneScheduler::new(h.actor.clone(), h.controller, 2, h.actor.monotonic_ms()).unwrap();
    h.router = Router::new(h.actor.clone(), h.controller, &h.scheduler, 2).unwrap();
    let mut audits = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let run = manual(&mut h).await;
        let result = delivered(&mut h).await;
        assert_eq!(result.run, run);
        assert_eq!(result.definition, "later");
        assert_eq!(
            result.status,
            homesense_core::scene::Status::CommandsWritten
        );
        assert!(audits.insert(result.audit_id));
    }
    assert_eq!(
        h.count("scene_run"),
        8,
        "durable history must survive slot reuse"
    );
    let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM scene_run WHERE status='commands_written'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        8
    );
    drop(sql);
    assert!(h.scheduler.reserve_slots(2).is_ok());
    assert!(
        h.router
            .poll(&mut h.scheduler, None)
            .await
            .unwrap()
            .1
            .is_empty(),
        "results must be delivered exactly once"
    );
    h.finish().await;
}

#[tokio::test]
async fn pending_manual_timelines_keep_their_slots_until_cancelled_and_delivered() {
    let mut r = rules();
    r["rules"] = json!([]);
    r["timed_scenes"][0]["steps"][0]["offset_ms"] = json!(50_000);
    let mut h = Harness::new(r).await;
    h.scheduler =
        SceneScheduler::new(h.actor.clone(), h.controller, 2, h.actor.monotonic_ms()).unwrap();
    h.router = Router::new(h.actor.clone(), h.controller, &h.scheduler, 2).unwrap();
    let first = manual(&mut h).await;
    // Use a non-overlapping second timeline so replacement cannot retire first.
    let steps = vec![homesense_core::scene::Step {
        delay_ms: 50_000,
        device: homesense_core::DeviceId::new("marker").unwrap(),
        command: homesense_core::Command::Switch(true),
        source: "pending/other".into(),
    }];
    let second = h
        .scheduler
        .start(steps, h.actor.monotonic_ms(), 1000)
        .await
        .unwrap();
    assert!(
        h.router
            .poll(&mut h.scheduler, None)
            .await
            .unwrap()
            .1
            .is_empty()
    );
    assert!(
        h.scheduler.reserve_slots(1).is_err(),
        "active capacity must remain bounded"
    );
    assert!(h.wire().await.is_empty());
    h.scheduler.cancel(first).await.unwrap();
    let result = delivered(&mut h).await;
    assert_eq!(result.run, first);
    assert_eq!(result.status, homesense_core::scene::Status::Cancelled);
    assert!(h.scheduler.reserve_slots(1).is_ok());
    assert_eq!(
        h.scheduler.status(second),
        Some(homesense_core::scene::Status::Pending)
    );
    h.scheduler.cancel(second).await.unwrap();
    h.finish().await;
}

#[tokio::test]
async fn explicitly_retired_manual_result_is_delivered_without_double_retirement() {
    let mut r = rules();
    r["rules"] = json!([]);
    let mut h = Harness::new(r).await;
    let run = manual(&mut h).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !h.scheduler.settled_terminal(run) {
        h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
        h.wire().await;
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    h.scheduler.retire(run).unwrap();
    let result = delivered(&mut h).await;
    assert_eq!(result.run, run);
    assert_eq!(
        result.status,
        homesense_core::scene::Status::CommandsWritten
    );
    assert!(
        h.router
            .poll(&mut h.scheduler, None)
            .await
            .unwrap()
            .1
            .is_empty()
    );
    assert_eq!(h.count("scene_run"), 1);
    h.finish().await;
}

#[tokio::test]
async fn undelivered_terminals_and_failed_reservations_preserve_cached_status() {
    let mut r = rules();
    r["rules"] = json!([]);
    let mut h = Harness::new(r).await;
    h.scheduler =
        SceneScheduler::new(h.actor.clone(), h.controller, 2, h.actor.monotonic_ms()).unwrap();
    h.router = Router::new(h.actor.clone(), h.controller, &h.scheduler, 2).unwrap();
    let first = manual(&mut h).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !h.scheduler.settled_terminal(first) {
        h.scheduler.tick(h.actor.monotonic_ms()).await.unwrap();
        h.wire().await;
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let second = h
        .scheduler
        .start(
            vec![homesense_core::scene::Step {
                delay_ms: 50_000,
                device: homesense_core::DeviceId::new("marker").unwrap(),
                command: homesense_core::Command::Switch(true),
                source: "pending/other".into(),
            }],
            h.actor.monotonic_ms(),
            1000,
        )
        .await
        .unwrap();
    assert!(
        h.scheduler.reserve_slots(1).is_err(),
        "undelivered terminal still owns capacity"
    );
    assert_eq!(delivered(&mut h).await.run, first);
    assert!(
        h.scheduler.reserve_slots(2).is_err(),
        "active run prevents this larger reservation"
    );
    assert_eq!(
        h.scheduler.status(first),
        Some(homesense_core::scene::Status::CommandsWritten),
        "failed reservation must preserve cached status"
    );
    assert!(
        h.scheduler.reserve_slots(1).is_ok(),
        "a delivered terminal can now be reclaimed"
    );
    assert_eq!(h.scheduler.status(first), None);
    assert_eq!(
        h.scheduler.status(second),
        Some(homesense_core::scene::Status::Pending)
    );
    assert_eq!(
        h.count("scene_run"),
        2,
        "reclamation must not erase durable receipts"
    );
    h.scheduler.cancel(second).await.unwrap();
    h.finish().await;
}
