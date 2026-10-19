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
    startup_hold: bool,
    release_at: Option<std::time::Instant>,
    dimmer_level: u8,
    dimmer_target: Option<(u8,std::time::Instant)>,
    dimmer_reads: usize,
    relay_written_at:Option<std::time::Instant>,
    dimmer_written_at:Option<std::time::Instant>,
    dimmer_query_times:Vec<std::time::Instant>,
}
#[derive(Clone)]
struct Stream(Arc<Mutex<Bus>>);
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut b = self.0.lock().unwrap();
        if b.release_at.is_some_and(|at| std::time::Instant::now() >= at) {
            b.replies.extend([0xae, 0x41, 0xd1, 0xf2, 0xee]);
            b.release_at = None;
        }
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
        if b.startup_hold && b.writes.is_empty() {
            b.replies.extend([0xae, 0x41, 0xd1, 0xf1, 0xee]);
            b.release_at = Some(std::time::Instant::now() + Duration::from_millis(142));
        } else if b.startup_hold {
            assert!(b.release_at.is_none(), "startup must receive release before next query");
        }
        b.writes.push(frame);
        if frame[1]==0x15 {
            if frame[2]==0xa0 {
                b.dimmer_reads+=1;
                b.dimmer_query_times.push(std::time::Instant::now());
                if let Some((target,at))=b.dimmer_target && std::time::Instant::now()>=at && b.dimmer_reads>=2 {
                    b.dimmer_level=target;b.dimmer_target=None;
                }
                let level=b.dimmer_level;
                for ch in 1..=8 {b.replies.extend([0xae,0x15,0xd0+ch,level,0xee]);}
            } else {
                assert!(matches!(frame[2],1|0xb1));
                let now=std::time::Instant::now();
                b.dimmer_written_at=Some(now);b.dimmer_query_times.clear();
                b.dimmer_target=Some((frame[3],now+if frame[2]==0xb1 {Duration::from_millis(6150)} else {Duration::ZERO}));
                b.dimmer_reads=0;
            }
            return Ok(bytes.len());
        }
        assert_eq!(frame[1], 3, "no panel queries or outputs");
        if (0xa1..=0xa8).contains(&frame[2]) {
            b.replies.extend([0xae, 3, frame[2] + 0x30, 0xf1, 0xee]);
        } else {
            b.relay_written_at=Some(std::time::Instant::now());
            assert!((1..=8).contains(&frame[2]) && matches!(frame[3], 0xf1 | 0xf2));
            b.replies.extend([0xae, 3, frame[2] + 0xd0, frame[3], 0xee]);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[derive(Clone,Copy)]
enum SlotWindow { Gap, Locked, Ready }
async fn run(independent: bool, strict: bool, delayed_release: bool, startup_hold: bool, ramp:bool, slot_window:Option<SlotWindow>) {
    let gateways = if independent {
        vec!["bus", "other"]
    } else {
        vec!["bus"]
    };
    let mut configs=gateways.iter().map(|id|serde_json::json!({"id":id,"endpoint":if *id=="bus"{"127.0.0.1:1"}else{"127.0.0.1:2"},"modules":[{"id":format!("{id}-r8"),"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":format!("{id}-ts6"),"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]})).collect::<Vec<_>>();
    let mut devices=gateways.iter().map(|id|serde_json::json!({"id":format!("{id}-light"),"name":"Synthetic light","module_id":format!("{id}-r8"),"channel":1,"enabled":true})).collect::<Vec<_>>();
    if ramp {
        for (id,gateway) in gateways.iter().zip(&mut configs) {
            gateway["modules"].as_array_mut().unwrap().push(serde_json::json!({"id":format!("{id}-ad8"),"address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}));
            devices.push(serde_json::json!({"id":format!("{id}-dimmer"),"name":"Synthetic delayed dimmer","module_id":format!("{id}-ad8"),"channel":1,"enabled":true}));
        }
    }
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
        .map(|id| (id.to_string(), Arc::new(Mutex::new(Bus { startup_hold, dimmer_level:60, ..Default::default() }))))
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
    if startup_hold {
        assert_eq!(buses["bus"].lock().unwrap().writes.len(), 8);
        assert_eq!(o.feedback.module_truth().received_port_state("bus", 0x41, 1).unwrap().observed,
            Some(homesense_core::Observation::Button { pressed: false }));
        drop(o);a.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();return;
    }
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
    if let Some(window) = slot_window {
        let baseline = buses["bus"].lock().unwrap().writes.len();
        assert_eq!(baseline, 8);
        o.configure_consumer_refresh(&[(homesense_core::DeviceId::new("bus-light").unwrap(), 1)].into()).unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        // Do not accidentally test the previous startup reply's turnaround
        // instead of the newly selected query's write-slot wait.
        o.transport.finish_reply_turnaround().await;
        let mut slot = a.write_slot.clone().lock_owned().await;
        *slot = if matches!(window,SlotWindow::Ready) {None} else {Some(tokio::time::Instant::now() + Duration::from_millis(250))};
        let held_slot = if matches!(window,SlotWindow::Locked) {Some(slot)} else {drop(slot); None};
        // Arrived after the previous receive; it must get a decode opportunity
        // instead of waiting behind a newly due query's pacing wait.
        buses["bus"].lock().unwrap().replies.extend([0xae,0x41,0xd1,0xf1,0xee]);
        let attempt = tokio::time::timeout(Duration::from_millis(100), async {
            if independent {o.local_tick().await} else {
                o.refresh_outputs_if_due().await.map_err(|(_, error)| error)
            }
        }).await;
        drop(held_slot);
        let query_sent = buses["bus"].lock().unwrap().writes.len() != baseline;
        if attempt.is_err() || query_sent {
            drop(o);a.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
            panic!("new refresh query blocked the receive opportunity or sent before buffered press: timed_out={}, query_sent={query_sent}", attempt.is_err());
        }
        attempt.unwrap().unwrap();
        // Make the slot ready, then prove the actual decoded press now gates it.
        *a.write_slot.clone().lock_owned().await = None;
        let until = std::time::Instant::now() + Duration::from_millis(120);
        while std::time::Instant::now() < until {
            if independent {o.start_local_reads().unwrap();o.local_tick().await.unwrap();}
            else {o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(o.feedback.module_truth().received_port_state("bus",0x41,1).unwrap().observed,
            Some(homesense_core::Observation::Button {pressed:true}));
        assert_eq!(buses["bus"].lock().unwrap().writes.len(),baseline,"ready slot still waits for the real release");
        buses["bus"].lock().unwrap().replies.extend([0xae,0x41,0xd1,0xf2,0xee]);
        let until = std::time::Instant::now() + Duration::from_millis(50);
        while std::time::Instant::now() < until {
            if independent {o.start_local_reads().unwrap();o.local_tick().await.unwrap();}
            else {o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(o.feedback.module_truth().received_port_state("bus",0x41,1).unwrap().observed,
            Some(homesense_core::Observation::Button {pressed:false}));
        assert_eq!(buses["bus"].lock().unwrap().writes.len(),baseline,"periodic queries still wait300s after release");
        drop(o);a.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();return;
    }
    if ramp {
        let baseline=buses["bus"].lock().unwrap().writes.len();assert_eq!(baseline,9);
        buses["bus"].lock().unwrap().replies.extend([0xae,0x41,0xd1,0xf1,0xee,0xae,0x41,0xd1,0xf2,0xee]);
        let until=std::time::Instant::now()+Duration::from_millis(30);
        while std::time::Instant::now()<until {
            if independent {o.local_tick().await.unwrap();o.start_local_reads().unwrap();}
            else {o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        a.submit(homesense_hap::requests::Batch {controller,intents:vec![homesense_hap::requests::Intent {
            device:homesense_core::DeviceId::new("bus-dimmer").unwrap(),command:homesense_core::Command::SetLevel {
                level:homesense_core::Percent::new(17).unwrap(),transition:homesense_core::LevelTransition::Immediate,
            },
        }],subscriptions:vec![]}).await.unwrap();
        let mut relay_requested=false;
        let until=std::time::Instant::now()+Duration::from_millis(6800);
        while std::time::Instant::now()<until {
            if independent {o.local_tick().await.unwrap();}
            else {o.receive_pending_output().await.unwrap();o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            if let Some(write)=o.step().await.unwrap(){
                write.result.unwrap();
                if !relay_requested {
                    a.submit(homesense_hap::requests::Batch {controller,intents:vec![homesense_hap::requests::Intent {
                        device:homesense_core::DeviceId::new("bus-light").unwrap(),command:homesense_core::Command::Switch(false),
                    }],subscriptions:vec![]}).await.unwrap();relay_requested=true;
                }
            }
            if independent{o.start_local_reads().unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(o.feedback.module_truth().received_port_state("bus",0x15,1).unwrap().observed,
            Some(homesense_core::Observation::Level(homesense_core::Percent::new(17).unwrap())),
            "a first intermediate reply cannot postpone final brightness confirmation for five minutes");
        {let state=buses["bus"].lock().unwrap();assert!(state.dimmer_reads>=2);
        let written=state.dimmer_written_at.unwrap();
        assert!(state.dimmer_query_times.iter().all(|at|at.duration_since(written)<Duration::from_secs(1)),"direct AD8 confirmation must not wait six seconds: {:?}",state.dimmer_query_times.iter().map(|at|at.duration_since(written)).collect::<Vec<_>>());
        assert!(state.relay_written_at.unwrap().duration_since(written)<Duration::from_secs(1),"direct confirmation must not block another control");
        assert!(state.writes[baseline..].iter().all(|frame|frame[1]==0x15 || (frame[1]==3 && matches!(frame[2],1|0xa1))),"confirmation must not restart unrelated periodic R8 reads");}
        let count=buses["bus"].lock().unwrap().writes.len();
        let until=std::time::Instant::now()+Duration::from_millis(100);
        while std::time::Instant::now()<until {
            if independent{o.local_tick().await.unwrap();o.start_local_reads().unwrap();}
            else{o.receive_pending_output().await.unwrap();o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(buses["bus"].lock().unwrap().writes.len(),count,"confirmation queries stop at the observed target");
        drop(o);a.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();return;
    }
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
    // A real short press can arrive in a different read from its release.
    // The field trace lost F2 when a newly admitted query followed F1; the
    // receive-only comparison delivered F2 142 ms later.
    if delayed_release {
        if independent {
            // An explicitly due consumer on the other bus must still progress;
            // ordinary background polling now waits at least five minutes.
            o.configure_consumer_refresh(&[(homesense_core::DeviceId::new("other-light").unwrap(),1)].into()).unwrap();
        }
        buses["bus"].lock().unwrap().replies.extend([0xae, 0x41, 0xd1, 0xf1, 0xee]);
        let end = std::time::Instant::now() + Duration::from_millis(160);
        while std::time::Instant::now() < end {
            if independent {
                o.local_tick().await.unwrap();
                o.start_local_reads().unwrap();
            } else {
                o.receive("bus").await.unwrap();
                o.refresh_outputs_if_due().await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(buses["bus"].lock().unwrap().writes.len(), 8,
            "new polling must leave the pressed bus quiet until release");
        if independent {
            assert!(buses["other"].lock().unwrap().writes.len() > 8,
                "a pressed key cannot stall an independent bus");
        }
    }
    if delayed_release {
        // A release ends receive protection, but ordinary/strict periodic reads
        // still wait five minutes. It must not be mistaken for a load receipt.
        a.module_query(controller,a.task_generation().await.unwrap(),"bus-r8".into(),
            crate::module_queries::Scope::Channel {channel:8},true).await.unwrap();
        a.submit(homesense_hap::requests::Batch { controller,
            intents: vec![homesense_hap::requests::Intent {
                device: homesense_core::DeviceId::new("bus-light").unwrap(),
                command: homesense_core::Command::Switch(true),
            }], subscriptions: vec![],
        }).await.unwrap();
        let end = std::time::Instant::now() + Duration::from_millis(50);
        while std::time::Instant::now() < end {
            if independent { o.local_tick().await.unwrap(); }
            else { o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap(); }
            if let Some(write) = o.step().await.unwrap() { write.result.unwrap(); }
            if independent { o.start_local_reads().unwrap(); }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(&buses["bus"].lock().unwrap().writes[8..], &[[0xae,3,1,0xf1,0xee]],
            "control writes proceed while pressed, confirmation query waits for real release");
        buses["bus"].lock().unwrap().replies.extend([0xae, 0x41, 0xd1, 0xf2, 0xee]);
        o.configure_consumer_refresh(&[(homesense_core::DeviceId::new("bus-light").unwrap(), 1)].into()).unwrap();
        let end = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < end {
            if independent {
                o.local_tick().await.unwrap();
            } else {
                o.receive_pending_output().await.unwrap();
                o.receive("bus").await.unwrap();
                o.refresh_outputs_if_due().await.unwrap();
            }
            if let Some(write) = o.step().await.unwrap() { write.result.unwrap(); }
            if independent { o.start_local_reads().unwrap(); }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let sent=buses["bus"].lock().unwrap().writes[8..].to_vec();
        assert_eq!(sent.first(),Some(&[0xae,3,1,0xf1,0xee]));
        let queries=sent.iter().skip(1).copied().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(queries,[[0xae,3,0xa1,0xf2,0xee],[0xae,3,0xa8,0xf2,0xee]].into());
        assert_eq!(sent.len(),3,"one actual write, one confirmation and one explicitly requested query; no periodic reads");
    } else {
        // Module feedback updates canonical state but is not panel/App activity
        // and therefore cannot pull the next background scan into this cycle.
        buses["bus"].lock().unwrap().replies.extend([0xae,3,0xd1,0xf2,0xee]);
        for _ in 0..50 {
            if independent {o.local_tick().await.unwrap();o.start_local_reads().unwrap();}
            else {o.receive_pending_output().await.unwrap();o.receive("bus").await.unwrap();o.refresh_outputs_if_due().await.unwrap();}
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(count(),baseline,"unsolicited load feedback cannot start a background round");
        assert_eq!(o.feedback.module_truth().received_port_state("bus",3,1).unwrap().observed,
            Some(homesense_core::Observation::Switch(false)),"state still updates without polling");
    }
    drop(o);
    a.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn unsolicited_load_feedback_updates_single_gateway_without_restarting_round() {
    run(false, false, false, false, false, None).await;
}
#[tokio::test]
async fn unsolicited_load_feedback_updates_independent_gateway_without_restarting_round() {
    run(true, false, false, false, false, None).await;
}

#[tokio::test]
async fn strict_consumer_on_independent_bus_never_refreshes_unrelated_ports_or_gateway() {
    run(true, true, false, false, false, None).await;
}

#[tokio::test]
async fn short_press_defers_single_bus_queries_until_real_release() {
    run(false, false, true, false, false, None).await;
}
#[tokio::test]
async fn short_press_defers_its_local_bus_without_stalling_other_gateway() {
    run(true, false, true, false, false, None).await;
}

#[tokio::test]
async fn startup_queries_also_wait_for_the_received_press_release() {
    run(false, false, false, true, false, None).await;
}

#[tokio::test]
async fn keypad_quiet_period_keeps_single_bus_dimmer_confirmation_until_final_feedback() {
    run(false,false,false,false,true, None).await;
}
#[tokio::test]
async fn keypad_quiet_period_keeps_local_bus_dimmer_confirmation_until_final_feedback() {
    run(true,false,false,false,true, None).await;
}

#[tokio::test]
async fn due_query_receives_buffered_press_before_future_write_slot_single_bus() {
    run(false,false,false,false,false,Some(SlotWindow::Gap)).await;
}
#[tokio::test]
async fn due_query_receives_buffered_press_before_locked_write_slot_single_bus() {
    run(false,false,false,false,false,Some(SlotWindow::Locked)).await;
}
#[tokio::test]
async fn due_query_receives_buffered_press_before_future_write_slot_local_bus() {
    run(true,false,false,false,false,Some(SlotWindow::Gap)).await;
}
#[tokio::test]
async fn due_query_receives_buffered_press_before_locked_write_slot_local_bus() {
    run(true,false,false,false,false,Some(SlotWindow::Locked)).await;
}

#[tokio::test]
async fn due_query_receives_buffered_press_before_ready_write_slot_single_bus() {
    run(false,false,false,false,false,Some(SlotWindow::Ready)).await;
}
#[tokio::test]
async fn due_query_receives_buffered_press_before_ready_write_slot_local_bus() {
    run(true,false,false,false,false,Some(SlotWindow::Ready)).await;
}
