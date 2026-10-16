//! Real decode/receive paths only; no enabled physical execution rules.
use super::*;
use crate::admission::{Admission, Options};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
#[derive(Default)]
struct Bus {
    writes: Vec<[u8; 5]>,
    replies: VecDeque<u8>,
}
#[derive(Clone)]
struct Stream(Arc<Mutex<Bus>>);
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut b = self.0.lock().unwrap();
        if b.replies.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = out.len().min(b.replies.len());
        for x in &mut out[..n] {
            *x = b.replies.pop_front().unwrap()
        }
        Ok(n)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let frame: [u8; 5] = bytes.try_into().unwrap();
        let mut b = self.0.lock().unwrap();
        b.writes.push(frame);
        assert_eq!(frame[1], 3, "no panel queries or outputs");
        assert!((0xa1..=0xa8).contains(&frame[2]), "only R8 singleton reads");
        b.replies.extend([0xae, 3, frame[2] + 0x30, 0xf1, 0xee]);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn run(independent: bool, strict: bool) {
    let gateways = if independent {
        vec!["bus", "other"]
    } else {
        vec!["bus"]
    };
    let configs=gateways.iter().map(|id|serde_json::json!({"id":id,"endpoint":if *id=="bus"{"127.0.0.1:1"}else{"127.0.0.1:2"},"modules":[{"id":format!("{id}-r8"),"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":format!("{id}-ts6"),"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]})).collect::<Vec<_>>();
    let devices=gateways.iter().map(|id|serde_json::json!({"id":format!("{id}-light"),"name":"Synthetic light","module_id":format!("{id}-r8"),"channel":1,"enabled":true})).collect::<Vec<_>>();
    let raw = serde_json::to_vec(
        &serde_json::json!({"schema_version":2,"revision":1,"gateways":configs,"devices":devices}),
    )
    .unwrap();
    let c = homesense_config::Configuration::parse(&raw, ExecutionContext::Simulation).unwrap();
    let dir = std::env::temp_dir().join(format!("panel-activity-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("runtime.sqlite");
    Store::open(&path)
        .unwrap()
        .publish_configuration(None, &raw, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let a = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    a.transport_failed().await.unwrap();
    let buses = gateways
        .iter()
        .map(|id| (id.to_string(), Arc::new(Mutex::new(Bus::default()))))
        .collect::<BTreeMap<_, _>>();
    let conns = buses
        .iter()
        .map(|(id, bus)| {
            (
                id.clone(),
                hb_transport::Connection::for_simulation(Stream(bus.clone()), 2),
            )
        })
        .collect();
    let f = Feedback::new(
        &c,
        ExecutionContext::Simulation,
        Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
        2,
    )
    .unwrap();
    let mut o =
        ObservedTransport::new_recovering(TransportOwner::new(a.clone(), conns), f, 2).unwrap();
    let age = c.refresh_policy().observation_max_age_ms();
    o.query_recovery_with_policy(
        &c,
        ExecutionContext::Simulation,
        500,
        age,
        Synchronization::OutputsAndKeypadEvents,
    )
    .await
    .unwrap();
    if independent {
        o.enable_independent(
            &c,
            Arc::new(|_, _| Err(std::io::ErrorKind::NotConnected.into())),
        )
        .await
        .unwrap();
    }
    o.configure_output_refresh(
        &c,
        controller,
        a.task_generation().await.unwrap(),
        ExecutionContext::Simulation,
        500,
        age,
    )
    .unwrap();
    let count = || {
        buses
            .values()
            .map(|b| b.lock().unwrap().writes.len())
            .sum::<usize>()
    };
    let baseline = count();
    // Manual LED echo, invalid port/value and unknown module are never activity.
    buses["bus"].lock().unwrap().replies.extend([
        0xae, 0x41, 1, 0xf1, 0xee, 0xae, 0x41, 0xd7, 0xf1, 0xee, 0xae, 0x41, 0xd1, 0x65, 0xee,
        0xae, 0x70, 0xd1, 0xf1, 0xee,
    ]);
    for _ in 0..20 {
        if independent {
            o.local_tick().await.unwrap();
            o.start_local_reads().unwrap();
        } else {
            o.receive("bus").await.unwrap();
            o.refresh_outputs_if_due().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(count(), baseline);
    if strict {
        let target = homesense_core::DeviceId::new("bus-light").unwrap();
        let other = homesense_core::DeviceId::new("other-light").unwrap();
        let old = o.snapshot().devices[&other].observed_at_ms;
        o.configure_consumer_refresh(&[(target, 600)].into())
            .unwrap();
        let end = std::time::Instant::now() + Duration::from_millis(1000);
        while std::time::Instant::now() < end {
            o.local_tick().await.unwrap();
            o.start_local_reads().unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        {
            let writes = buses["bus"].lock().unwrap();
            let reads = &writes.writes[8..];
            assert!(reads.len() >= 3);
            assert!(
                reads.iter().all(|f| *f == [0xae, 3, 0xa1, 0xf2, 0xee]),
                "only explicit consumer port, never other seven R8 ports"
            );
        }
        assert_eq!(buses["other"].lock().unwrap().writes.len(), 8);
        assert_eq!(
            o.snapshot().devices[&other].observed_at_ms,
            old,
            "other gateway receipt age stays original"
        );
        drop(o);
        a.shutdown().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    let before = o
        .feedback
        .module_truth()
        .port_observation_count("bus", 0x41, 1)
        .unwrap();
    buses["bus"].lock().unwrap().replies.extend([
        0xae, 0x41, 0xd1, 0xf1, 0xee, 0xae, 0x41, 0xd1, 0xf2, 0xee, 0xae, 0x41, 0xd2, 0xf1, 0xee,
        0xae, 0x41, 0xd2, 0xf2, 0xee,
    ]);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while count() < baseline + gateways.len() * 8 || o.output_query_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "normal output round never accelerated"
        );
        if independent {
            o.local_tick().await.unwrap();
            o.start_local_reads().unwrap();
        } else {
            o.receive_pending_output().await.unwrap();
            o.receive("bus").await.unwrap();
            o.refresh_outputs_if_due().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        o.feedback
            .module_truth()
            .port_observation_count("bus", 0x41, 1),
        Some(before + 2)
    );
    assert_eq!(
        count(),
        baseline + gateways.len() * 8,
        "press/release burst coalesces one ordinary round"
    );
    for _ in 0..20 {
        if independent {
            o.local_tick().await.unwrap();
            o.start_local_reads().unwrap();
        } else {
            o.receive("bus").await.unwrap();
            o.refresh_outputs_if_due().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        count(),
        baseline + gateways.len() * 8,
        "solicited output replies cannot start more rounds"
    );
    drop(o);
    a.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn registered_ts6_activity_accelerates_single_gateway_normal_round_only() {
    run(false, false).await;
}
#[tokio::test]
async fn registered_ts6_activity_accelerates_independent_gateway_normal_round_only() {
    run(true, false).await;
}

#[tokio::test]
async fn strict_consumer_on_independent_bus_never_refreshes_unrelated_ports_or_gateway() {
    run(true, true).await;
}
