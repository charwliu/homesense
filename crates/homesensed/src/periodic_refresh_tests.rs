use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Delay replies independently of the command path; no socket or field I/O.
struct DelayedStream {
    wire: Arc<Mutex<Wire>>,
    delay: Arc<AtomicU64>,
    replies: VecDeque<(Instant, Vec<u8>, bool)>,
}
impl Write for DelayedStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // The wire format has no transaction ID. A matching earlier control
        // reply can complete a logical read before the query-caused reply; the
        // pending-slot test separately checks dispatch serialization, while
        // the stress test checks actual observations and minimum wire spacing.
        let before = self.wire.lock().unwrap().replies.len();
        let count = Stream(self.wire.clone()).write(bytes)?;
        let reply = self.wire.lock().unwrap().replies.drain(before..).collect();
        self.replies.push_back((
            Instant::now() + Duration::from_millis(self.delay.load(Ordering::Relaxed)),
            reply,
            bytes[2] >= 0xa0,
        ));
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Read for DelayedStream {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let now = Instant::now();
        while self.replies.front().is_some_and(|(at, _, _)| *at <= now) {
            self.wire
                .lock()
                .unwrap()
                .replies
                .extend(self.replies.pop_front().unwrap().1);
        }
        Stream(self.wire.clone()).read(bytes)
    }
}

#[tokio::test]
async fn pending_reply_defers_other_writes_but_not_request_admission() {
    let (h, mut owner, delay) = delayed_fixture(8, 0, 20).await;
    delay.store(120, Ordering::Relaxed);
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            500,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(251)).await;
    owner.refresh_outputs_if_due().await.unwrap();
    let before_count = owner
        .feedback()
        .module_truth()
        .port_observation_count("bus", 1, 1)
        .unwrap();
    let ids =
        h.a.submit(homesense_hap::requests::Batch {
            controller: h.controller,
            intents: vec![homesense_hap::requests::Intent {
                device: DeviceId::new("lamp000").unwrap(),
                command: homesense_core::Command::SetLevel {
                    level: homesense_core::Percent::new(30).unwrap(),
                    transition: homesense_core::LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    assert!(owner.step().await.unwrap().is_none());
    owner.refresh_outputs_if_due().await.unwrap();
    assert_eq!(h.w.lock().unwrap().writes.len(), 2);
    tokio::time::timeout(Duration::from_secs(1), async {
        while owner
            .feedback()
            .module_truth()
            .port_observation_count("bus", 1, 1)
            .unwrap()
            == before_count
        {
            owner.receive("bus").await.unwrap();
            owner.refresh_outputs_if_due().await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let outcome = owner.step().await.unwrap().unwrap();
    assert_eq!(outcome.command_id, ids[0]);
    assert!(outcome.result.is_ok());
    assert_eq!(h.w.lock().unwrap().writes.len(), 3);
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn incomplete_periodic_module_retries_once_without_renewing_missing_port() {
    let (h, mut owner, _) = delayed_fixture(8, 0, 20).await;
    let missing = DeviceId::new("lamp007").unwrap();
    let before = owner.snapshot().devices[&missing].observed_at_ms;
    h.w.lock().unwrap().omit = Some(8);
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            60,
            400,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            owner.receive("bus").await.unwrap();
            owner.refresh_outputs_if_due().await.unwrap();
            if owner.snapshot().devices[&missing].quality==Quality::Unknown {break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.w.lock().unwrap().writes.len(), 3);
    assert_eq!(owner.snapshot().devices[&missing].observed_at_ms, before);
    assert_eq!(owner.snapshot().devices[&missing].quality,Quality::Unknown);
    assert!(owner.snapshot().devices.iter().filter(|(id,_)|**id!=missing).all(|(_,state)|state.quality==Quality::Confirmed),"a missing port cannot invalidate healthy siblings");
    assert!(h.a.diagnostics(h.controller,h.a.task_generation().await.unwrap()).await.unwrap().ready(),"missing feedback alone is not a transport failure");
    assert!(!owner.output_query_pending());
    for _ in 0..4 {owner.refresh_outputs_if_due().await.unwrap();}
    assert_eq!(h.w.lock().unwrap().writes.len(),3,"a missing module must yield during its bounded backoff");
    h.w.lock().unwrap().replies.extend([0xae,1,0xd8,80,0xee]);
    owner.receive("bus").await.unwrap();
    assert_eq!(owner.snapshot().devices[&missing].quality,Quality::Confirmed,"a genuine later reply can restore only that port");
    assert!(owner.snapshot().devices[&missing].observed_at_ms>before);
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn first_partial_reply_is_not_lost_when_the_bounded_retry_is_silent() {
    let(h,mut owner,_)=delayed_fixture(8,0,20).await;
    h.w.lock().unwrap().omit=Some(8);
    owner.configure_output_refresh(&h.c,h.controller,h.a.task_generation().await.unwrap(),ExecutionContext::Simulation,60,400).unwrap();
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            owner.receive("bus").await.unwrap();owner.refresh_outputs_if_due().await.unwrap();
            if h.w.lock().unwrap().writes.len()==2 {h.w.lock().unwrap().silent=true;}
            if owner.feedback().module_truth().located_state("bus",1,8).unwrap().quality==Quality::Unknown {break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    assert_eq!(h.w.lock().unwrap().writes.len(),3);
    for channel in 1..=8 {
        assert_eq!(owner.feedback().module_truth().located_state("bus",1,channel).unwrap().quality,
            if channel==8 {Quality::Unknown}else{Quality::Confirmed},"the first query's genuine replies remain evidence when its retry is silent");
    }
    assert!(h.a.diagnostics(h.controller,h.a.task_generation().await.unwrap()).await.unwrap().ready());
    drop(owner);h.close().await;
}

#[tokio::test]
async fn delayed_96_port_mixed_polling_preserves_freshness_spacing_and_control_progress() {
    let (h, mut owner, delay) = delayed_fixture(96, 7, 20).await;
    delay.store(25, Ordering::Relaxed);
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            5000,
        )
        .unwrap();
    let started = Instant::now();
    let mut max_age = 0;
    let mut written = 0;
    while started.elapsed() < Duration::from_secs(8) {
        if h.a
            .diagnostics(h.controller, h.a.task_generation().await.unwrap())
            .await
            .unwrap()
            .queued
            == 0
        {
            h.a.submit(homesense_hap::requests::Batch {
                controller: h.controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: DeviceId::new("lamp000").unwrap(),
                    command: homesense_core::Command::Switch(true),
                }],
                subscriptions: vec![],
            })
            .await
            .unwrap();
        }
        if let Some(outcome) = owner.step().await.unwrap() {
            assert!(outcome.result.is_ok());
            written += 1;
        }
        owner.receive("bus").await.unwrap();
        owner.refresh_outputs_if_due().await.unwrap();
        let now = h.a.monotonic_ms();
        owner.expire(now, 5000).unwrap();
        for (id, state) in &owner.snapshot().devices {
            assert_eq!(
                state.quality,
                Quality::Confirmed,
                "{id:?}: now={now} observed={:?} age={:?} controls={written} queries={:?}",
                state.observed_at_ms,
                state.observed_at_ms.map(|at| now.saturating_sub(at)),
                h.w.lock().unwrap().writes.iter().filter(|frame| frame[2] >= 0xa0).collect::<Vec<_>>()
            );
            max_age = max_age.max(now - state.observed_at_ms.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    {
        let wire = h.w.lock().unwrap();
        assert!(wire.writes.len() > 100);
        assert!(written > 20);
        assert!(
            wire.writes
                .iter()
                .filter(|f| f[1] <= 7 && f[2] >= 0xa0)
                .all(|f| (0xa1..=0xa8).contains(&f[2]))
        );
        assert!(
            wire.times
                .windows(2)
                .all(|at| at[1].duration_since(at[0]) >= Duration::from_millis(20))
        );
        println!(
            "delayed96 reply_delay_ms=25 max_age_ms={max_age} controls={written} total_frames={}",
            wire.writes.len()
        );
    }
    drop(owner);
    h.close().await;
}
async fn delayed_fixture(
    n: usize,
    switches: usize,
    gap: u64,
) -> (Harness, ObservedTransport<DelayedStream>, Arc<AtomicU64>) {
    let h = Harness::new(n, switches, "ad8_din_rs485_v2_2_1", 8, gap).await;
    let delay = Arc::new(AtomicU64::new(0));
    let feedback = Feedback::new(
        &h.c,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.a.boot_id()).unwrap().as_u128(),
        2,
    )
    .unwrap();
    let stream = DelayedStream {
        wire: h.w.clone(),
        delay: delay.clone(),
        replies: VecDeque::new(),
    };
    let mut owner = ObservedTransport::new_recovering(
        TransportOwner::new(
            h.a.clone(),
            BTreeMap::from([(
                "bus".into(),
                hb_transport::Connection::for_simulation(stream, 2),
            )]),
        ),
        feedback,
        2,
    )
    .unwrap();
    owner
        .query_recovery_with_policy(
            &h.c,
            ExecutionContext::Simulation,
            500,
            5000,
            Synchronization::OutputsAndCalibratedMotors,
        )
        .await
        .unwrap();
    (h, owner, delay)
}
#[tokio::test]
async fn periodic_refresh_returns_before_delayed_response_without_fabricating_state() {
    let (h, mut owner, delay) = delayed_fixture(96, 7, 20).await;
    let before = owner
        .feedback()
        .module_truth()
        .port_observation_count("bus", 1, 1)
        .unwrap();
    let observed_at = owner.snapshot().devices[&DeviceId::new("lamp000").unwrap()].observed_at_ms;
    delay.store(120, Ordering::Relaxed);
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            5000,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(45)).await;
    let start = Instant::now();
    owner.refresh_outputs_if_due().await.unwrap();
    let elapsed = start.elapsed();
    // The old implementation blocks for and consumes this reply. Starting a
    // periodic query must neither wait for it nor invent a newer observation.
    assert_eq!(
        owner
            .feedback()
            .module_truth()
            .port_observation_count("bus", 1, 1),
        Some(before)
    );
    assert_eq!(
        owner.snapshot().devices[&DeviceId::new("lamp000").unwrap()].observed_at_ms,
        observed_at
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            owner.receive("bus").await.unwrap();
            owner.refresh_outputs_if_due().await.unwrap();
            if owner
                .feedback()
                .module_truth()
                .port_observation_count("bus", 1, 1)
                .unwrap()
                > before
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    println!(
        "periodic_query_admission_ms={} injected_reply_delay_ms=120",
        elapsed.as_millis()
    );
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn disconnect_discards_pending_plan_and_requires_fresh_recovery() {
    let (h, mut owner, delay) = delayed_fixture(8, 0, 20).await;
    delay.store(120, Ordering::Relaxed);
    let generation = h.a.task_generation().await.unwrap();
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            generation.clone(),
            ExecutionContext::Simulation,
            500,
            500,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(251)).await;
    owner.refresh_outputs_if_due().await.unwrap();
    assert!(owner.output_query_pending());
    // A reconfiguration cannot discard an unresolved read on the same stream.
    assert!(
        owner
            .configure_output_refresh(
                &h.c,
                h.controller,
                generation.clone(),
                ExecutionContext::Simulation,
                500,
                500
            )
            .is_err()
    );
    h.w.lock().unwrap().eof = true;
    let read = owner.receive("bus").await.unwrap();
    assert!(matches!(
        read.result,
        hb_transport::Receive::Disconnected { .. }
    ));
    assert!(!owner.output_query_pending());
    assert!(
        owner
            .snapshot()
            .devices
            .values()
            .all(|s| s.quality != Quality::Confirmed)
    );
    let t = TransportOwner::new(
        h.a.clone(),
        BTreeMap::from([(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                DelayedStream {
                    wire: h.w.clone(),
                    delay: Arc::new(AtomicU64::new(0)),
                    replies: VecDeque::new(),
                },
                3,
            ),
        )]),
    );
    owner.recover(t, 3).unwrap();
    assert!(
        owner
            .configure_output_refresh(
                &h.c,
                h.controller,
                generation,
                ExecutionContext::Simulation,
                500,
                500
            )
            .is_err()
    );
    {
        let mut wire = h.w.lock().unwrap();
        wire.eof = false;
        wire.replies.clear();
        wire.omit = Some(8);
    }
    assert!(
        owner
            .query_recovery_with_policy(
                &h.c,
                ExecutionContext::Simulation,
                60,
                500,
                Synchronization::OutputsAndCalibratedMotors
            )
            .await
            .is_err()
    );
    assert!(!owner.output_query_pending());
    assert!(
        owner
            .snapshot()
            .devices
            .values()
            .all(|s| s.quality != Quality::Confirmed)
    );
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn split_periodic_reply_refreshes_only_received_ports_and_holds_dispatch() {
    let (h, mut owner, delay) = delayed_fixture(8, 0, 20).await;
    delay.store(100, Ordering::Relaxed);
    h.w.lock().unwrap().chunk = 5;
    let before = owner
        .feedback()
        .module_truth()
        .port_observation_count("bus", 1, 1)
        .unwrap();
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            500,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(251)).await;
    owner.refresh_outputs_if_due().await.unwrap();
    tokio::time::sleep(Duration::from_millis(110)).await;
    // Replies are ordered 8..1; one frame is consumed per receive call.
    for channel in (1..=8).rev() {
        assert!(owner.output_query_pending());
        assert!(owner.step().await.unwrap().is_none());
        owner.refresh_outputs_if_due().await.unwrap();
        assert!(
            owner
                .feedback()
                .module_truth()
                .port_observation_count("bus", 1, channel)
                .unwrap()
                > before
        );
        if channel > 1 {
            assert_eq!(
                owner
                    .feedback()
                    .module_truth()
                    .port_observation_count("bus", 1, 1),
                Some(before)
            );
            assert!(owner.output_query_pending());
        }
    }
    assert!(!owner.output_query_pending());
    assert_eq!(h.w.lock().unwrap().writes.len(), 2);
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn periodic_schedule_prioritizes_oldest_actual_feedback_over_registry_order() {
    let (h, mut owner, _) = delayed_fixture(16, 0, 20).await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    {
        let mut wire = h.w.lock().unwrap();
        for channel in 1..=8 {
            wire.replies
                .extend([0xae, 1, 0xd0 + channel, channel * 10, 0xee]);
        }
    }
    owner.receive("bus").await.unwrap();
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            1000,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(251)).await;
    owner.refresh_outputs_if_due().await.unwrap();
    assert_eq!(
        h.w.lock().unwrap().writes.last(),
        Some(&[0xae, 2, 0xa0, 0xf2, 0xee])
    );
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn buffered_query_reply_allows_accepted_control_in_same_dispatch_turn() {
    let (h, mut owner, _) = delayed_fixture(8, 0, 20).await;
    owner
        .configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            500,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(251)).await;
    owner.refresh_outputs_if_due().await.unwrap();
    assert!(owner.output_query_pending());
    let ids =
        h.a.submit(homesense_hap::requests::Batch {
            controller: h.controller,
            intents: vec![homesense_hap::requests::Intent {
                device: DeviceId::new("lamp000").unwrap(),
                command: homesense_core::Command::SetLevel {
                    level: homesense_core::Percent::new(30).unwrap(),
                    transition: homesense_core::LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    let outcome = owner
        .step()
        .await
        .unwrap()
        .expect("buffered reply must not consume an empty dispatch turn");
    assert_eq!(outcome.command_id, ids[0]);
    assert!(outcome.result.is_ok());
    assert!(!owner.output_query_pending());
    assert_eq!(h.w.lock().unwrap().writes.len(), 3);
    drop(owner);
    h.close().await;
}
#[tokio::test]
async fn explicit_consumer_refreshes_only_its_port_without_shortening_default_idle_round(){
    let (h,mut owner,_)=delayed_fixture(8,1,20).await;let age=h.c.refresh_policy().observation_max_age_ms();
    owner.configure_output_refresh(&h.c,h.controller,h.a.task_generation().await.unwrap(),ExecutionContext::Simulation,500,age).unwrap();
    let baseline=h.w.lock().unwrap().writes.len();let old=owner.snapshot().devices[&DeviceId::new("lamp001").unwrap()].observed_at_ms;
    let end=Instant::now()+Duration::from_millis(350);while Instant::now()<end{owner.receive("bus").await.unwrap();owner.refresh_outputs_if_due().await.unwrap();tokio::time::sleep(Duration::from_millis(5)).await;}
    assert_eq!(h.w.lock().unwrap().writes.len(),baseline,"no configured short consumer: default hours-long idle must remain");
    owner.configure_consumer_refresh(&[(DeviceId::new("lamp000").unwrap(),600)].into()).unwrap();
    let end=Instant::now()+Duration::from_millis(1000);while Instant::now()<end{owner.receive_pending_output().await.unwrap();owner.refresh_outputs_if_due().await.unwrap();tokio::time::sleep(Duration::from_millis(5)).await;}
    {let wire=h.w.lock().unwrap();let queries=&wire.writes[baseline..];assert!(queries.len()>=3,"strict port must refresh independently");assert!(queries.iter().all(|q|*q==[0xae,1,0xa1,0xf2,0xee]),"unrelated seven ports must keep long idle: {queries:?}");}
    assert_eq!(owner.snapshot().devices[&DeviceId::new("lamp001").unwrap()].observed_at_ms,old,"unrequested read cannot renew another port");drop(owner);h.close().await;
}


#[tokio::test]
async fn postwrite_read_is_not_starved_by_continuously_due_consumer_queries() {
    let (h, mut owner, _) = delayed_fixture(96, 7, 20).await;
    owner.configure_output_refresh(&h.c,h.controller,h.a.task_generation().await.unwrap(),
        ExecutionContext::Simulation,500,h.c.refresh_policy().observation_max_age_ms()).unwrap();
    let requirements=(56..64).map(|n|(DeviceId::new(format!("lamp{n:03}")).unwrap(),1)).collect();
    owner.configure_consumer_refresh(&requirements).unwrap();
    h.a.submit(homesense_hap::requests::Batch {controller:h.controller,
        intents:vec![homesense_hap::requests::Intent {device:DeviceId::new("lamp003").unwrap(),command:homesense_core::Command::Switch(true)}],subscriptions:vec![]}).await.unwrap();
    assert!(owner.step().await.unwrap().unwrap().result.is_ok());
    let baseline=h.w.lock().unwrap().writes.len();
    let started=Instant::now();
    while started.elapsed()<Duration::from_millis(400) {
        owner.receive_pending_output().await.unwrap();
        owner.step_after_refresh_read().await.unwrap();
        owner.refresh_outputs_if_due().await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let writes=h.w.lock().unwrap().writes[baseline..].to_vec();
    let queries=writes.iter().filter(|f|f[2]>=0xa0).collect::<Vec<_>>();
    assert!(queries.iter().take(2).any(|f|f[1]==1 && f[2]==0xa4),
        "post-write supply read must run within two query slots despite overdue consumers: {queries:?}");
    assert!(queries.iter().any(|f|f[1]==8 && f[2]==0xa0),"consumer refresh must also progress");
    drop(owner);h.close().await;
}
