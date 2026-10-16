use super::*;
use crate::admission::{Admission, Options};
use homesense_core::{DeviceId, ExecutionContext};
use homesense_storage::Store;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Default)]
struct Wire {
    replies: VecDeque<u8>,
    writes: Vec<[u8; 5]>,
    times: Vec<Instant>,
    chunk: usize,
    omit: Option<u8>,
    dirty: bool,
    eof: bool,
    switches: usize,
    channels: u8,
}
struct Stream(Arc<Mutex<Wire>>);
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut w = self.0.lock().unwrap();
        if w.eof {
            return Ok(0);
        }
        if w.replies.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = out.len().min(w.replies.len()).min(w.chunk.max(1));
        for b in &mut out[..n] {
            *b = w.replies.pop_front().unwrap()
        }
        Ok(n)
    }
}
impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let(frames,tail)=b.as_chunks::<5>();assert!(tail.is_empty());
        let mut w=self.0.lock().unwrap();w.times.push(Instant::now());
        for &f in frames{w.writes.push(f);
        let channels: Vec<_> = if f[2] == 0xA0 {
            (1..=w.channels).rev().collect()
        } else if f[2] >= 0xA1 {
            vec![f[2] - 0xA0]
        } else {
            vec![f[2]]
        };
        for c in channels {
            if Some(c) == w.omit {
                continue;
            }
            let value = if f[1] as usize <= w.switches {
                0xF1
            } else {
                c * 10
            };
            if w.dirty {
                w.replies.extend([0xAE, 0x70, 0xD0 + c, value, 0xEE]);
                w.replies.extend([0xAE, f[1], 0xD0 + c, 0x65, 0xEE]);
            }
            w.replies.extend([0xAE, f[1], 0xD0 + c, value, 0xEE]);
            if w.dirty {
                w.replies.extend([0xAE, f[1], 0xD0 + c, value, 0xEE]);
            }
        }
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Harness {
    o: ObservedTransport<Stream>,
    a: Admission,
    c: homesense_config::Configuration,
    w: Arc<Mutex<Wire>>,
    dir: std::path::PathBuf,
    controller: Uuid,
    doc: Vec<u8>,
}
impl Harness {
    async fn new(n: usize, switches: usize, profile: &str, channels: u8, interval: u64) -> Self {
        let modules=(0..n.div_ceil(channels as usize)).map(|i|serde_json::json!({"address":format!("{:02X}",i+1),"profile":if i<switches {"r8i8_din_pro_v2_7_7"}else{profile},"verification":{"status":"simulated"}})).collect::<Vec<_>>();
        let devices=(0..n).map(|i|serde_json::json!({"id":format!("lamp{i:03}"),"name":"anonymous","gateway":"bus","address":format!("{:02X}",i/channels as usize+1),"channel":i%channels as usize+1,"enabled":true})).collect::<Vec<_>>();
        let doc=serde_json::to_vec(&serde_json::json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
        let c = homesense_config::Configuration::parse(&doc, ExecutionContext::Simulation).unwrap();
        let dir = std::env::temp_dir().join(format!("module-query-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("runtime.sqlite");
        Store::open(&path)
            .unwrap()
            .publish_configuration(None, &doc, ExecutionContext::Simulation, &[])
            .unwrap();
        let controller = Uuid::new_v4();
        let a = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                write_interval: Duration::from_millis(interval),
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        let w = Arc::new(Mutex::new(Wire {
            chunk: 4096,
            eof: true,
            switches,
            channels,
            ..Default::default()
        }));
        let f = Feedback::new(
            &c,
            ExecutionContext::Simulation,
            Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let t = |epoch| {
            TransportOwner::new(
                a.clone(),
                BTreeMap::from([(
                    "bus".into(),
                    hb_transport::Connection::for_simulation(Stream(w.clone()), epoch),
                )]),
            )
        };
        let mut o = ObservedTransport::new(t(1), f);
        o.receive("bus").await.unwrap();
        o.recover(t(2), 2).unwrap();
        w.lock().unwrap().eof = false;
        Self {
            o,
            a,
            c,
            w,
            dir,
            controller,
            doc,
        }
    }
    async fn sync(&mut self, timeout: u64, age: u64) -> Result<(), String> {
        self.o
            .query_recovery_with_policy(
                &self.c,
                ExecutionContext::Simulation,
                timeout,
                age,
                Synchronization::OutputsAndCalibratedMotors,
            )
            .await
    }
    async fn close(self) {
        drop(self.o);
        self.a.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}
#[tokio::test]
async fn documented_profiles_group_and_decode_split_duplicate_wrong_address_frames() {
    for (profile, n) in [
        ("ad8_din_rs485_v2_2_1", 8),
        ("gt_e4_rl_v3_2_2_2", 4),
        ("gt_d8_rl_v2_2_6_2", 8),
        ("gt_l8_rl_v2_0_7_2", 8),
    ] {
        for chunk in [1, 2, 3, 4, 5, 7, 4096] {
            let mut h = Harness::new(n, 0, profile, n as u8, 20).await;
            {
                let mut w = h.w.lock().unwrap();
                w.chunk = chunk;
                w.dirty = true;
            }
            h.sync(500, 5000).await.unwrap();
            assert_eq!(
                h.w.lock().unwrap().writes,
                vec![[0xAE, 1, 0xA0, 0xF2, 0xEE]]
            );
            for (i, s) in h.o.snapshot().devices.values().enumerate() {
                assert_eq!(s.quality, Quality::Confirmed);
                assert_eq!(
                    s.observed,
                    Some(homesense_core::Observation::Level(
                        homesense_core::Percent::new((i as u8 + 1) * 10).unwrap()
                    ))
                );
            }
            h.close().await;
        }
    }
}
#[tokio::test]
async fn partial_duplicate_feedback_times_out_without_retry_or_defaults() {
    let mut h = Harness::new(4, 0, "gt_e4_rl_v3_2_2_2", 4, 20).await;
    {
        let mut w = h.w.lock().unwrap();
        w.omit = Some(4);
        w.dirty = true;
    }
    assert!(h.sync(30, 5000).await.is_err());
    assert_eq!(h.w.lock().unwrap().writes.len(), 1);
    assert!(
        h.o.snapshot().devices[&DeviceId::new("lamp003").unwrap()]
            .observed
            .is_none()
    );
    h.close().await;
}
#[tokio::test]
async fn unsupported_pro_and_c8_remain_individual() {
    for profile in ["r8i8_din_pro_v2_7_7", "gt_c8_rl_v2_8_2_2"] {
        let mut h = Harness::new(8, 0, profile, 8, 20).await;
        h.w.lock().unwrap().switches = 1;
        h.sync(200, 5000).await.unwrap();
        assert_eq!(
            h.w.lock()
                .unwrap()
                .writes
                .iter()
                .map(|f| f[2])
                .collect::<Vec<_>>(),
            (0xA1..=0xA8).collect::<Vec<_>>()
        );
        h.close().await;
    }
}
#[tokio::test]
async fn reconnect_discards_old_partial_and_requires_all_channels_again() {
    let mut h = Harness::new(4, 0, "gt_e4_rl_v3_2_2_2", 4, 20).await;
    h.sync(100, 5000).await.unwrap();
    {
        let mut w = h.w.lock().unwrap();
        w.eof = true;
        w.replies.clear();
        w.replies.extend([0xAE, 1, 0xD4]);
    }
    h.o.receive("bus").await.unwrap();
    let t = TransportOwner::new(
        h.a.clone(),
        BTreeMap::from([(
            "bus".into(),
            hb_transport::Connection::for_simulation(Stream(h.w.clone()), 3),
        )]),
    );
    h.o.recover(t, 3).unwrap();
    {
        let mut w = h.w.lock().unwrap();
        w.eof = false;
        w.replies.clear();
        w.omit = Some(4);
    }
    assert!(h.sync(30, 5000).await.is_err());
    assert!(
        h.o.snapshot()
            .devices
            .values()
            .all(|s| s.quality != Quality::Confirmed)
    );
    h.close().await;
}
#[tokio::test]
async fn capacity96_documented_modules_at100ms_and_mixed_at20ms_stay_fresh() {
    for (switches, interval, expected) in [(0, 100, 12), (7, 20, 61)] {
        let mut h = Harness::new(96, switches, "ad8_din_rs485_v2_2_1", 8, interval).await;
        let start = Instant::now();
        h.sync(500, 5000).await.unwrap();
        assert_eq!(h.w.lock().unwrap().writes.len(), expected);
        let startup = start.elapsed();
        h.o.configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            500,
            5000,
        )
        .unwrap();
        let start = Instant::now();
        let mut max_age = 0;
        let mut phase_samples = Vec::new();
        while start.elapsed() < Duration::from_secs(8) {
            let tick_start = Instant::now();
            let tick_ms = tick_start.saturating_duration_since(start).as_millis();
            if switches > 0 {
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
            let submit_ms = tick_start.elapsed().as_millis();
            h.o.step().await.unwrap();
            let step_ms = tick_start.elapsed().as_millis();
            h.o.receive("bus").await.unwrap();
            let receive_ms = tick_start.elapsed().as_millis();
            h.o.refresh_outputs_if_due().await.unwrap();
            let refresh_ms = tick_start.elapsed().as_millis();
            let now = h.a.monotonic_ms();
            h.o.expire(now, 5000).unwrap();
            phase_samples.push([tick_ms, submit_ms, step_ms, receive_ms, refresh_ms]);
            for (id,s) in &h.o.snapshot().devices {
                if s.quality != Quality::Confirmed {
                    let wire=h.w.lock().unwrap();
                    eprintln!("CAPACITY_DIAG {}",serde_json::json!({"switches":switches,"interval_ms":interval,"startup_ms":startup.as_millis(),"loop_elapsed_ms":start.elapsed().as_millis(),"now_ms":now,"device":id.as_str(),"quality":format!("{:?}",s.quality),"observed_at_ms":s.observed_at_ms,"age_ms":s.observed_at_ms.map(|at|now.saturating_sub(at)),"phase_columns":["tick_start_ms","after_submit_ms","after_step_ms","after_receive_ms","after_refresh_ms"],"phase_samples":phase_samples,"wire_frames":wire.writes,"wire_relative_ms":wire.times.iter().map(|at|at.saturating_duration_since(wire.times[0]).as_millis()).collect::<Vec<_>>() }));
                }
                assert_eq!(s.quality, Quality::Confirmed);
                max_age = max_age.max(now - s.observed_at_ms.unwrap());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        {
            let wire = h.w.lock().unwrap();
            let queries = wire.writes.iter().filter(|f| f[2] >= 0xA0).count();
            let controls = wire.writes.len() - queries;
            println!(
                "module_capacity96 switches={switches} interval={interval} startup_ms={} max_age_ms={max_age} queries={queries} controls={controls}",
                startup.as_millis()
            );
        }
        h.close().await;
    }
}
#[tokio::test]
async fn infeasible_individual_mapping_is_not_silently_accepted() {
    let mut h = Harness::new(96, 12, "ad8_din_rs485_v2_2_1", 8, 20).await;
    h.sync(500, 5000).await.unwrap();
    // Tiny freshness budget cannot accommodate 96 individual endpoints plus controls.
    assert!(
        h.o.configure_output_refresh(
            &h.c,
            h.controller,
            h.a.task_generation().await.unwrap(),
            ExecutionContext::Simulation,
            100,
            100
        )
        .is_err()
    );
    h.close().await;
}
#[tokio::test]
async fn queued_controls_do_not_starve_refresh_and_frames_keep_wire_spacing() {
    let mut h = Harness::new(8, 0, "gt_d8_rl_v2_2_6_2", 8, 20).await;
    h.sync(500, 5000).await.unwrap();
    h.o.configure_output_refresh(
        &h.c,
        h.controller,
        h.a.task_generation().await.unwrap(),
        ExecutionContext::Simulation,
        500,
        500,
    )
    .unwrap();
    let start = Instant::now();
    let mut queries = 0;
    let mut controls = 0;
    while start.elapsed() < Duration::from_secs(2) {
        if h.a
            .diagnostics(h.controller, h.a.task_generation().await.unwrap())
            .await
            .unwrap()
            .queued
            == 0
        {
            h.a.submit(homesense_hap::requests::Batch {
                controller: h.controller,
                intents: (0..8)
                    .map(|i| homesense_hap::requests::Intent {
                        device: DeviceId::new(format!("lamp{i:03}")).unwrap(),
                        command: homesense_core::Command::SetLevel {
                            level: homesense_core::Percent::new(30).unwrap(),
                            transition: homesense_core::LevelTransition::Immediate,
                        },
                    })
                    .collect(),
                subscriptions: vec![],
            })
            .await
            .unwrap();
        }
        h.o.step().await.unwrap();
        h.o.refresh_outputs_if_due().await.unwrap();
        h.o.receive("bus").await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for f in &h.w.lock().unwrap().writes {
        if f[2] == 0xA0 {
            queries += 1
        } else {
            controls += 1
        }
    }
    assert!(
        h.w.lock()
            .unwrap()
            .times
            .windows(2)
            .all(|p| p[1].duration_since(p[0]) >= Duration::from_millis(20))
    );
    assert!(queries >= 5);
    assert!(controls >= 5);
    println!("control_fairness queries={queries} controls={controls}");
    h.close().await;
}

#[tokio::test]
async fn actual_tcp96_mixed_idle_polling_has_bounded_steady_refresh() {
    // Entirely synthetic verification records, used only on a loopback listener.
    let mut h = Harness::new(96, 7, "ad8_din_rs485_v2_2_1", 8, 20).await;
    let mut doc: serde_json::Value = serde_json::from_slice(&h.doc).unwrap();
    for module in doc["gateways"][0]["modules"].as_array_mut().unwrap() {
        module["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback only, not hardware verification","verified_at_unix_ms":1});
    }
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&doc).unwrap(),
        ExecutionContext::Live,
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket.set_nodelay(true).unwrap();
        let mut frames = Vec::new();
        loop {
            let mut f = [0u8; 5];
            match socket.read_exact(&mut f) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    break;
                }
                Err(e) => panic!("peer {e}"),
            };
            assert_eq!(f[3], 0xF2);
            frames.push(f);
            let channels: Vec<_> = if f[2] == 0xA0 {
                (1..=8).collect()
            } else {
                vec![f[2] - 0xA0]
            };
            let reply: Vec<_> = channels
                .into_iter()
                .flat_map(|c| {
                    [
                        0xAE,
                        f[1],
                        0xD0 + c,
                        if f[1] <= 7 { 0xF1 } else { 30 },
                        0xEE,
                    ]
                })
                .collect();
            socket.write_all(&reply).unwrap();
        }
        frames
    });
    // Feedback boot stays tied to the isolated Admission. Only query/control
    // context changes; no real network endpoint is accepted by this fixture.
    h.o.invalidate();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Live,
        Uuid::parse_str(h.a.boot_id()).unwrap().as_u128(),
        2,
    )
    .unwrap();
    let conn = hb_transport::Connection::connect(
        addr,
        2,
        Duration::from_secs(1),
        Duration::from_millis(100),
    )
    .unwrap();
    let mut o = ObservedTransport::new_recovering(
        TransportOwner::new(h.a.clone(), BTreeMap::from([("bus".into(), conn)])),
        feedback,
        2,
    )
    .unwrap();
    o.query_recovery_with_policy(
        &config,
        ExecutionContext::Live,
        500,
        5000,
        Synchronization::OutputsAndCalibratedMotors,
    )
    .await
    .unwrap();
    o.configure_output_refresh(
        &config,
        h.controller,
        h.a.task_generation().await.unwrap(),
        ExecutionContext::Live,
        500,
        5000,
    )
    .unwrap();
    let start = Instant::now();
    let mut max_age = 0;
    let mut samples = 0;
    while start.elapsed() < Duration::from_secs(12) {
        o.step().await.unwrap();
        o.receive("bus").await.unwrap();
        o.refresh_outputs_if_due().await.unwrap();
        let now = h.a.monotonic_ms();
        o.expire(now, 5000).unwrap();
        for s in o.snapshot().devices.values() {
            assert_eq!(s.quality, Quality::Confirmed);
            max_age = max_age.max(now - s.observed_at_ms.unwrap());
        }
        samples += 1;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(o);
    let frames = peer.join().unwrap();
    assert!(frames.len() > 200);
    assert!(frames.iter().filter(|f| f[1] <= 7).all(|f| f[2] != 0xA0));
    println!(
        "tcp96_mixed samples={samples} queries={} max_age_ms={max_age}",
        frames.len()
    );
    h.close().await;
}

#[tokio::test]
async fn disabled_channels_do_not_become_bindings_and_enabled_tail_is_required() {
    let h = Harness::new(8, 0, "gt_d8_rl_v2_2_6_2", 8, 20).await;
    let mut doc: serde_json::Value = serde_json::from_slice(&h.doc).unwrap();
    for i in 1..7 {
        doc["devices"][i]["enabled"] = false.into();
    }
    let c = homesense_config::Configuration::parse(
        &serde_json::to_vec(&doc).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let f = Feedback::new(
        &c,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.a.boot_id()).unwrap().as_u128(),
        2,
    )
    .unwrap();
    let t = TransportOwner::new(
        h.a.clone(),
        BTreeMap::from([(
            "bus".into(),
            hb_transport::Connection::for_simulation(Stream(h.w.clone()), 2),
        )]),
    );
    let mut o = ObservedTransport::new_recovering(t, f, 2).unwrap();
    o.query_mapping(
        &c,
        ExecutionContext::Simulation,
        200,
        5000,
        None,
        Synchronization::CompleteFeedback,
    )
    .await
    .unwrap();
    assert_eq!(o.snapshot().devices.len(), 2);
    assert_eq!(h.w.lock().unwrap().writes.len(), 1);
    // An old complete snapshot must not substitute for a missing enabled tail.
    h.w.lock().unwrap().omit = Some(8);
    assert!(
        o.query_mapping(
            &c,
            ExecutionContext::Simulation,
            30,
            5000,
            None,
            Synchronization::CompleteFeedback
        )
        .await
        .is_err()
    );
    drop(o);
    h.close().await;
}

#[tokio::test]
async fn wrong_epoch_gateway_and_configuration_cannot_complete_a_module_query() {
    let mut h = Harness::new(4, 0, "gt_e4_rl_v3_2_2_2", 4, 20).await;
    let frame = hb_protocol::RawFrame {
        address: 1,
        command: 0xD1,
        value: 30,
    };
    for (gateway, epoch) in [("bus", 1), ("other", 2)] {
        assert!(
            h.o.feedback
                .apply(&GatewayRead {
                    gateway_id: gateway.into(),
                    transport_epoch: epoch,
                    received_at_ms: h.a.monotonic_ms(),
                    result: hb_transport::Receive::Frames {
                        transport_epoch: epoch,
                        frames: vec![frame]
                    }
                })
                .is_err()
        );
    }
    let mut doc: serde_json::Value = serde_json::from_slice(&h.doc).unwrap();
    doc["revision"] = 2.into();
    let c = homesense_config::Configuration::parse(
        &serde_json::to_vec(&doc).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        h.o.query_mapping(
            &c,
            ExecutionContext::Simulation,
            100,
            5000,
            None,
            Synchronization::CompleteFeedback
        )
        .await
        .is_err()
    );
    assert!(h.w.lock().unwrap().writes.is_empty());
    h.close().await;
}

#[tokio::test]
async fn old_feedback_cannot_pass_a_new_read_or_freshness_gate() {
    let mut h = Harness::new(4, 0, "gt_e4_rl_v3_2_2_2", 4, 20).await;
    h.sync(100, 5000).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    h.o.expire(h.a.monotonic_ms(), 20).unwrap();
    assert!(
        h.o.snapshot()
            .devices
            .values()
            .all(|s| s.quality == Quality::Stale)
    );
    h.w.lock().unwrap().omit = Some(2);
    assert!(
        h.o.refresh(
            &DeviceId::new("lamp000").unwrap(),
            &h.c,
            ExecutionContext::Simulation,
            30,
            5000
        )
        .await
        .is_err()
    );
    // Previous channel2 value and duplicate current-channel frames cannot close
    // this batch. Only one bounded status-read retry; no control replay.
    assert_eq!(h.w.lock().unwrap().writes.len(), 3);
    h.close().await;
}

#[tokio::test]
async fn hidden_supply_is_queried_even_when_not_a_separate_logical_light() {
    let h = Harness::new(16, 1, "ad8_din_rs485_v2_2_1", 8, 20).await;
    let mut doc: serde_json::Value = serde_json::from_slice(&h.doc).unwrap();
    for i in 1..8 {
        doc["devices"][i]["enabled"] = false.into();
    }
    doc["devices"][8]["supply_relay"] = "lamp000".into();
    doc["devices"][8]["default_on_level"] = 100.into();
    let c = homesense_config::Configuration::parse(
        &serde_json::to_vec(&doc).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let f = Feedback::new(
        &c,
        ExecutionContext::Simulation,
        Uuid::parse_str(h.a.boot_id()).unwrap().as_u128(),
        2,
    )
    .unwrap();
    let t = TransportOwner::new(
        h.a.clone(),
        BTreeMap::from([(
            "bus".into(),
            hb_transport::Connection::for_simulation(Stream(h.w.clone()), 2),
        )]),
    );
    let mut o = ObservedTransport::new_recovering(t, f, 2).unwrap();
    o.query_mapping(
        &c,
        ExecutionContext::Simulation,
        200,
        5000,
        None,
        Synchronization::CompleteFeedback,
    )
    .await
    .unwrap();
    assert_eq!(o.snapshot().devices.len(), 9);
    assert_eq!(
        h.w.lock().unwrap().writes,
        (1..=8)
            .map(|channel| [0xAE, 1, 0xA0 + channel, 0xF2, 0xEE])
            .chain(std::iter::once([0xAE, 2, 0xA0, 0xF2, 0xEE]))
            .collect::<Vec<_>>()
    );
    // Disabled relay ports now have truth, but remain absent from logical state
    // and retain the original disabled-binding control gate.
    for address in [1, 2] {
        for channel in 1..=8 {
            assert_eq!(
                o.feedback()
                    .module_truth()
                    .located_state("bus", address, channel)
                    .unwrap()
                    .quality,
                Quality::Confirmed
            );
        }
    }
    for i in 1..8 {
        let id = DeviceId::new(format!("lamp{i:03}")).unwrap();
        assert!(!o.snapshot().devices.contains_key(&id));
        assert!(c.binding(&id).is_none());
    }
    drop(o);
    h.close().await;
}

#[tokio::test]
async fn changed_duplicate_is_latest_observation_not_an_atomic_batch_ack() {
    let mut h = Harness::new(4, 0, "gt_e4_rl_v3_2_2_2", 4, 20).await;
    h.o.feedback
        .apply(&GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 2,
            received_at_ms: h.a.monotonic_ms(),
            result: hb_transport::Receive::Frames {
                transport_epoch: 2,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 1,
                        command: 0xD1,
                        value: 0,
                    },
                    hb_protocol::RawFrame {
                        address: 1,
                        command: 0xD1,
                        value: 100,
                    },
                ],
            },
        })
        .unwrap();
    let id = DeviceId::new("lamp000").unwrap();
    assert_eq!(h.o.feedback.observation_count(&id), Some(2));
    assert_eq!(
        h.o.snapshot().devices[&id].observed,
        Some(homesense_core::Observation::Level(
            homesense_core::Percent::new(100).unwrap()
        ))
    );
    assert!(
        h.o.snapshot().devices[&DeviceId::new("lamp003").unwrap()]
            .observed
            .is_none()
    );
    h.close().await;
}

#[path = "periodic_refresh_tests.rs"]
mod periodic_refresh_tests;
