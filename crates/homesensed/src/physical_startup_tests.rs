//! Registry-wide synthetic reads through the actual connection/feedback owner.
use super::*;
use crate::{
    admission::{Admission, Options},
    transport_owner::TransportOwner,
};
use homesense_core::{ExecutionContext, Observation, Percent};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Bus {
    replies: VecDeque<u8>,
    writes: Vec<[u8; 5]>,
    eof: bool,
    omit: Option<(u8, u8)>,
    value: u8,
    switch: bool,
}
struct Stream(Arc<Mutex<Bus>>);
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut bus = self.0.lock().unwrap();
        if bus.eof {
            return Ok(0);
        }
        if bus.replies.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = out.len().min(bus.replies.len());
        for byte in &mut out[..n] {
            *byte = bus.replies.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let frame: [u8; 5] = bytes.try_into().unwrap();
        let mut bus = self.0.lock().unwrap();
        bus.writes.push(frame);
        if (1..=8).contains(&frame[2]) || (0xb1..=0xb8).contains(&frame[2]) {
            // This synthetic control has no unsolicited acknowledgement.
            bus.value=frame[3];return Ok(bytes.len());
        }
        let channels = if frame[2] == 0xa0 {
            (1..=8).collect::<Vec<_>>()
        } else {
            vec![frame[2] - 0xa0]
        };
        for channel in channels {
            if bus.omit == Some((frame[1], channel)) {
                continue;
            }
            let value = if bus.switch && frame[1] == 2 {
                0xf1
            } else {
                bus.value
            };
            bus.replies
                .extend([0xae, frame[1], 0xd0 + channel, value, 0xee]);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Fixture {
    owner: ObservedTransport<Stream>,
    admission: Admission,
    config: homesense_config::Configuration,
    bus: Arc<Mutex<Bus>>,
    controller: uuid::Uuid,
    dir: std::path::PathBuf,
}
impl Fixture {
    async fn new(switch: bool, aliases: bool) -> Self {
        let mut doc = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"bound","address":"01","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},{"id":"unbound","address":"02","profile":if switch{"r8i8_din_pro_v2_7_7"}else{"ad8_din_rs485_v2_2_1"},"verification":{"status":"simulated"}},{"id":"panel","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"}},{"id":"motor","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[]});
        if aliases {
            doc["devices"] = json!([{"id":"light","name":"Synthetic","module_id":"bound","channel":1,"enabled":true},{"id":"disabled","name":"Disabled","module_id":"bound","channel":2,"enabled":false}]);
        }
        let bytes = serde_json::to_vec(&doc).unwrap();
        let config =
            homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
        let dir = std::env::temp_dir().join(format!("physical-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("runtime.sqlite");
        homesense_storage::Store::open(&path)
            .unwrap()
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        let controller = uuid::Uuid::new_v4();
        let admission = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        let bus = Arc::new(Mutex::new(Bus {
            replies: VecDeque::new(),
            writes: vec![],
            eof: true,
            omit: None,
            value: 30,
            switch,
        }));
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            uuid::Uuid::parse_str(admission.boot_id())
                .unwrap()
                .as_u128(),
            1,
        )
        .unwrap();
        let transport = TransportOwner::new(
            admission.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Stream(bus.clone()), 1),
            )]
            .into(),
        );
        let mut owner = ObservedTransport::new(transport, feedback);
        owner.receive("bus").await.unwrap();
        let transport = TransportOwner::new(
            admission.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Stream(bus.clone()), 2),
            )]
            .into(),
        );
        owner.recover(transport, 2).unwrap();
        bus.lock().unwrap().eof = false;
        Self {
            owner,
            admission,
            config,
            bus,
            controller,
            dir,
        }
    }
    async fn sync(&mut self, timeout: u64, age: u64) -> Result<(), String> {
        self.owner
            .query_recovery_with_policy(
                &self.config,
                ExecutionContext::Simulation,
                timeout,
                age,
                Synchronization::OutputsAndCalibratedMotors,
            )
            .await
    }
    fn assert_ports(&self, value: u8) {
        for module in ["bound", "unbound"] {
            for channel in 1..=8 {
                let state = self
                    .owner
                    .feedback()
                    .module_truth()
                    .module_state(module, channel)
                    .unwrap();
                assert_eq!(state.quality, Quality::Confirmed);
                if module == "bound" || !self.bus.lock().unwrap().switch {
                    assert_eq!(
                        state.observed,
                        Some(Observation::Level(Percent::new(value).unwrap()))
                    );
                }
            }
        }
        for module in ["panel", "motor"] {
            let state = self
                .owner
                .feedback()
                .module_truth()
                .module_state(module, 1)
                .unwrap();
            assert_eq!(state.quality, Quality::Unknown);
            assert!(state.observed.is_none());
        }
    }
    async fn close(self) {
        drop(self.owner);
        self.admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}
#[tokio::test]
async fn sparse_and_alias_free_modules_read_all_ports_without_creating_devices_or_controls() {
    for aliases in [true, false] {
        let mut f = Fixture::new(false, aliases).await;
        f.sync(100, 5000).await.unwrap();
        f.assert_ports(30);
        assert_eq!(f.owner.snapshot().devices.len(), usize::from(aliases));
        assert_eq!(
            f.bus.lock().unwrap().writes,
            vec![[0xae, 1, 0xa0, 0xf2, 0xee], [0xae, 2, 0xa0, 0xf2, 0xee]]
        );
        assert!(crate::management_modules::control_device(&f.config, "bound", 2).is_err());
        assert!(crate::management_modules::control_device(&f.config, "unbound", 1).is_err());
        f.admission.task_generation().await.unwrap();
        f.close().await;
    }
}
#[tokio::test]
async fn omitted_unbound_port_prevents_startup_readiness_without_retry_or_defaults() {
    let mut f = Fixture::new(false, true).await;
    f.bus.lock().unwrap().omit = Some((2, 8));
    assert!(f.sync(30, 5000).await.is_err());
    assert!(f.admission.task_generation().await.is_err());
    let state = f
        .owner
        .feedback()
        .module_truth()
        .module_state("unbound", 8)
        .unwrap();
    assert_eq!(state.quality, Quality::Unknown);
    assert!(state.observed.is_none());
    assert_eq!(f.bus.lock().unwrap().writes.len(), 2);
    f.close().await;
}
#[tokio::test]
async fn periodic_scope_refreshes_alias_free_modules_and_uses_real_physical_capacity() {
    let mut f = Fixture::new(false, true).await;
    f.sync(100, 5000).await.unwrap();
    let generation = f.admission.task_generation().await.unwrap();
    assert!(
        f.owner
            .configure_output_refresh(
                &f.config,
                f.controller,
                generation.clone(),
                ExecutionContext::Simulation,
                100,
                70
            )
            .is_err()
    );
    f.owner
        .configure_output_refresh(
            &f.config,
            f.controller,
            generation,
            ExecutionContext::Simulation,
            100,
            400,
        )
        .unwrap();
    f.bus.lock().unwrap().value = 37;
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(105)).await;
        f.owner.refresh_outputs_if_due().await.unwrap();
        // Query admission does not imply receipt. Pump actual feedback while
        // retaining the original 100ms query bound and all state assertions.
        tokio::time::timeout(Duration::from_millis(100), async {
            while f.owner.output_query_pending() {
                f.owner.receive("bus").await.unwrap();
                f.owner.refresh_outputs_if_due().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    f.assert_ports(37);
    assert_eq!(f.owner.snapshot().devices.len(), 1);
    assert_eq!(f.bus.lock().unwrap().writes.len(), 4);
    f.close().await;
}
#[tokio::test]
async fn fallback_startup_reads_every_unbound_switch_port_and_preflights_capacity_before_io() {
    let mut f = Fixture::new(true, true).await;
    assert!(f.sync(100, 200).await.is_err());
    assert!(f.bus.lock().unwrap().writes.is_empty());
    f.sync(100, 5000).await.unwrap();
    f.assert_ports(30);
    let expected = std::iter::once([0xae, 1, 0xa0, 0xf2, 0xee])
        .chain((1..=8).map(|channel| [0xae, 2, 0xa0 + channel, 0xf2, 0xee]))
        .collect::<Vec<_>>();
    assert_eq!(f.bus.lock().unwrap().writes, expected);
    assert_eq!(f.owner.snapshot().devices.len(), 1);
    f.close().await;
}
#[tokio::test]
async fn reconnect_requires_every_port_from_the_new_epoch_before_reopening_admission() {
    let mut f = Fixture::new(false, true).await;
    f.sync(100, 5000).await.unwrap();
    f.bus.lock().unwrap().eof = true;
    f.owner.receive("bus").await.unwrap();
    let transport = TransportOwner::new(
        f.admission.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(Stream(f.bus.clone()), 3),
        )]
        .into(),
    );
    f.owner.recover(transport, 3).unwrap();
    assert!(f.owner.synchronize_recovery(1, 5000).await.is_err());
    {
        let mut bus = f.bus.lock().unwrap();
        bus.eof = false;
        bus.value = 40;
    }
    f.sync(100, 5000).await.unwrap();
    f.assert_ports(40);
    assert_eq!(f.owner.snapshot().devices.len(), 1);
    assert_eq!(f.bus.lock().unwrap().writes.len(), 4);
    f.close().await;
}

#[tokio::test]
async fn adaptive_idle_preserves_actual_times_and_unsolicited_feedback_starts_a_complete_fast_round() {
    let mut f=Fixture::new(false,true).await;
    let age=f.config.refresh_policy().observation_max_age_ms();
    f.sync(100,age).await.unwrap();
    f.owner.configure_output_refresh(&f.config,f.controller,f.admission.task_generation().await.unwrap(),ExecutionContext::Simulation,100,age).unwrap();
    assert!(f.owner.take_completed_refresh_round().is_some(),"startup qualifies initial indicator phase");
    let before=f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed_at_ms;
    let writes=f.bus.lock().unwrap().writes.len();
    tokio::time::sleep(Duration::from_millis(60)).await;
    f.owner.refresh_outputs_if_due().await.unwrap();
    assert_eq!(f.bus.lock().unwrap().writes.len(),writes,"no query during the expected three-hour idle");
    assert_eq!(f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed_at_ms,before,"waiting never renews feedback");
    f.bus.lock().unwrap().replies.extend([0xae,2,0xd3,40,0xee]);
    f.owner.receive("bus").await.unwrap();
    assert_eq!(f.owner.feedback().module_truth().module_state("unbound",3).unwrap().observed,Some(Observation::Level(Percent::new(40).unwrap())));
    assert_eq!(f.owner.feedback().effective_max_age_ms(age),age,"do not shorten old-port allowance before the full fast round");
    let deadline=tokio::time::Instant::now()+Duration::from_secs(2);
    let round=loop {
        assert!(tokio::time::Instant::now()<deadline,"active round did not complete");
        f.owner.refresh_outputs_if_due().await.unwrap();
        f.owner.receive("bus").await.unwrap();
        f.owner.receive_pending_output().await.unwrap();
        if let Some(round)=f.owner.take_completed_refresh_round(){break round;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(round.generation.epochs.configuration,1);
    assert_eq!(f.bus.lock().unwrap().writes.len(),writes+2,"each AD8 queried exactly once; replies are not activity");
    assert_eq!(f.owner.feedback().effective_max_age_ms(age),25_000);
    tokio::time::sleep(Duration::from_millis(60)).await;
    f.owner.refresh_outputs_if_due().await.unwrap();
    assert_eq!(f.bus.lock().unwrap().writes.len(),writes+2,"ordinary replies leave the next round delayed");
    f.close().await;
}

#[tokio::test]
async fn failed_adaptive_round_does_not_qualify_background_leds_but_preserves_healthy_ports() {
    let mut f=Fixture::new(false,true).await;let age=f.config.refresh_policy().observation_max_age_ms();
    f.sync(100,age).await.unwrap();
    f.owner.configure_output_refresh(&f.config,f.controller,f.admission.task_generation().await.unwrap(),ExecutionContext::Simulation,30,age).unwrap();
    f.owner.take_completed_refresh_round();
    f.bus.lock().unwrap().replies.extend([0xae,2,0xd3,40,0xee]);f.owner.receive("bus").await.unwrap();
    f.bus.lock().unwrap().omit=Some((1,4));
    let deadline=tokio::time::Instant::now()+Duration::from_secs(2);
    loop {assert!(tokio::time::Instant::now()<deadline);f.owner.refresh_outputs_if_due().await.unwrap();f.owner.receive("bus").await.unwrap();if f.owner.feedback().module_truth().located_state("bus",1,4).unwrap().quality==homesense_state::Quality::Unknown{break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    assert!(f.owner.take_completed_refresh_round().is_none());
    for channel in 1..=8 {
        assert_eq!(f.owner.feedback().module_truth().located_state("bus",1,channel).unwrap().quality,
            if channel==4 {homesense_state::Quality::Unknown}else{homesense_state::Quality::Confirmed},
            "even an unbound missing port prevents round qualification without discarding its healthy siblings");
    }
    assert!(f.admission.diagnostics(f.controller,f.admission.task_generation().await.unwrap()).await.unwrap().ready());
    f.close().await;
}

#[tokio::test]
async fn direct_and_ramp_writes_during_three_hour_idle_confirm_with_their_respective_delay() {
    for level in [100,60] {
    let mut f=Fixture::new(false,true).await;let age=f.config.refresh_policy().observation_max_age_ms();
    f.sync(100,age).await.unwrap();
    f.owner.configure_output_refresh(&f.config,f.controller,f.admission.task_generation().await.unwrap(),ExecutionContext::Simulation,100,age).unwrap();
    f.owner.take_completed_refresh_round();
    let before=f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed_at_ms;
    f.admission.submit(homesense_hap::requests::Batch {controller:f.controller,intents:vec![homesense_hap::requests::Intent {device:homesense_core::DeviceId::new("light").unwrap(),command:homesense_core::Command::SetLevel {level:Percent::new(level).unwrap(),transition:homesense_core::LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();
    let write_started_at=f.admission.monotonic_ms();
    tokio::time::timeout(Duration::from_secs(1),async {loop {if f.owner.step_after_refresh_read().await.unwrap().is_some(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    assert_eq!(f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed_at_ms,before,"Written is not readback");
    tokio::time::timeout(Duration::from_secs(if level==100 {1}else{7}),async {loop {
        f.owner.refresh_outputs_if_due().await.unwrap();f.owner.receive("bus").await.unwrap();
        if f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed==Some(Observation::Level(Percent::new(level).unwrap())) {break;}
        tokio::time::sleep(Duration::from_millis(5)).await;
    }}).await.unwrap();
    if level==60 {assert!(f.admission.monotonic_ms()>=write_started_at+6_000,"AD8 intermediate target waits six seconds after writing before confirmation");}
    assert_eq!(f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed,Some(Observation::Level(Percent::new(level).unwrap())));
    assert!(f.owner.feedback().module_truth().module_state("bound",1).unwrap().observed_at_ms>before);
    assert_eq!(f.bus.lock().unwrap().writes.last().unwrap()[1],1,"changed module read first in the ordinary round");
    f.close().await;
    }
}

#[tokio::test]
async fn eof_during_adaptive_idle_invalidates_immediately_without_waiting_for_the_poll() {
    let mut f=Fixture::new(false,true).await;let age=f.config.refresh_policy().observation_max_age_ms();
    f.sync(100,age).await.unwrap();
    f.owner.configure_output_refresh(&f.config,f.controller,f.admission.task_generation().await.unwrap(),ExecutionContext::Simulation,100,age).unwrap();
    f.owner.take_completed_refresh_round();f.bus.lock().unwrap().eof=true;
    assert!(matches!(f.owner.receive("bus").await.unwrap().result,hb_transport::Receive::Disconnected {..}));
    assert!(f.owner.take_completed_refresh_round().is_none());
    assert!(f.owner.feedback().snapshot().devices.values().all(|s|s.quality!=homesense_state::Quality::Confirmed));
    f.close().await;
}
