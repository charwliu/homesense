use super::*;
use crate::{admission::Options, transport_owner::GatewayRead};
use homesense_core::{ExecutionContext, Observation, Percent};
use homesense_state::Quality;
use homesense_storage::Store;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    path: PathBuf,
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    feedback: Feedback,
    id: DeviceId,
}
impl Fixture {
    async fn new(combined: bool, time: &'static AtomicU64) -> Self {
        Self::with_policy(combined,time,homesense_config::RefreshPolicy {idle_interval_ms:1000,active_interval_ms:100,backoff_ms:[200,500],quiet_rounds_per_step:2,round_budget_ms:1000,observation_grace_ms:3000}).await
    }
    async fn with_policy(combined:bool,time:&'static AtomicU64,policy:homesense_config::RefreshPolicy)->Self {
        time.store(0, Ordering::Release);
        let path = std::env::temp_dir().join(format!("device-read-age-{}.sqlite", Uuid::new_v4()));
        let mut document = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"fixture","endpoint":"127.0.0.1:1","modules":[{"id":"dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]});
        if combined {
            document["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"id":"switch","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            document["devices"][0]["supply"] = serde_json::json!({"module_id":"switch","channel":1});
            document["devices"][0]["default_on_level"] = 100.into();
            document["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","name":"Synthetic supply","module_id":"switch","channel":1,"enabled":true}));
        }
        // Preserve the original 5s boundary checks with an explicit tight policy.
        document["refresh_policy"]=serde_json::to_value(policy).unwrap();
        let bytes = serde_json::to_vec(&document).unwrap();
        Store::open(&path)
            .unwrap()
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        let configuration =
            homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
        let controller = Uuid::new_v4();
        let admission = Admission::open_with_test_time(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
            time,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let generation = admission.task_generation().await.unwrap();
        let feedback = Feedback::new(
            &configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut fixture = Self {
            path,
            admission,
            controller,
            generation,
            feedback,
            id: DeviceId::new("light").unwrap(),
        };
        fixture.observe(0x15, 37, 0);
        if combined {
            fixture.observe(3, 0xF1, 0);
        }
        fixture
    }
    fn observe(&mut self, address: u8, value: u8, at: u64) {
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "fixture".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address,
                        command: 0xD1,
                        value,
                    }],
                },
            })
            .unwrap();
    }
    async fn view(&self, age: u64) -> DeviceSnapshot {
        snapshot_with_max_age(
            &self.admission,
            self.controller,
            self.generation.clone(),
            vec![self.id.clone()],
            &self.feedback,
            age,
        )
        .await
        .unwrap()
    }
    async fn close(self) {
        self.admission.shutdown().await.unwrap();
        let sql = rusqlite::Connection::open(&self.path).unwrap();
        assert_eq!(
            sql.query_row("SELECT count(*) FROM command_journal", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(sql);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        let _ = std::fs::remove_file(self.path.with_extension("runtime.lock"));
    }
}

#[tokio::test]
async fn default_device_read_expires_at_boundary_without_mutating_feedback() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let fixture = Fixture::new(false, &TIME).await;
    let original = fixture.feedback.snapshot();
    TIME.store(4999, Ordering::Release);
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Confirmed
    );
    TIME.store(5000, Ordering::Release);
    let state = snapshot(
        &fixture.admission,
        fixture.controller,
        fixture.generation.clone(),
        vec![fixture.id.clone()],
        &fixture.feedback,
    )
    .await
    .unwrap();
    assert_eq!(state.devices[0].state.quality, Quality::Stale);
    assert_eq!(
        state.devices[0].state.observed,
        Some(Observation::Level(Percent::new(37).unwrap()))
    );
    assert_eq!(state.devices[0].state.observed_at_ms, Some(0));
    assert_eq!(fixture.feedback.snapshot(), original);
    let modules = crate::management_modules::snapshot(
        &fixture.admission,
        fixture.controller,
        fixture.generation.clone(),
        &fixture.feedback,
        5000,
        0,
        12,
    )
    .await
    .unwrap();
    assert_eq!(modules.modules[0].slots[0].quality, "stale");
    fixture.close().await;
}

#[tokio::test]
async fn read_respects_shorter_policy_and_rejects_invalid_or_future_age() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture = Fixture::new(false, &TIME).await;
    TIME.store(1233, Ordering::Release);
    assert_eq!(
        fixture.view(1234).await.devices[0].state.quality,
        Quality::Confirmed
    );
    TIME.store(1234, Ordering::Release);
    assert_eq!(
        fixture.view(1234).await.devices[0].state.quality,
        Quality::Stale
    );
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Confirmed
    );
    for bound in [0, 5001] {
        assert!(
            snapshot_with_max_age(
                &fixture.admission,
                fixture.controller,
                fixture.generation.clone(),
                vec![fixture.id.clone()],
                &fixture.feedback,
                bound
            )
            .await
            .is_err()
        );
    }
    fixture.observe(0x15, 42, 2000);
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Stale
    );
    fixture.close().await;
}

#[tokio::test]
async fn fresh_dimmer_does_not_hide_expired_supply_and_readback_restores_quality() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture = Fixture::new(true, &TIME).await;
    TIME.store(4900, Ordering::Release);
    fixture.observe(0x15, 41, 4900);
    TIME.store(5000, Ordering::Release);
    let original = fixture.feedback.snapshot();
    let view = fixture.view(5000).await;
    assert_eq!(view.devices.len(), 1);
    assert_eq!(view.devices[0].state.quality, Quality::Stale);
    assert_eq!(view.devices[0].state.observed_at_ms, Some(0));
    assert_eq!(fixture.feedback.snapshot(), original);
    fixture.observe(3, 0xF1, 5000);
    let view = fixture.view(5000).await;
    assert_eq!(view.devices[0].state.quality, Quality::Confirmed);
    assert_eq!(
        view.devices[0].state.observed,
        Some(Observation::Level(Percent::new(41).unwrap()))
    );
    fixture.close().await;
}

#[tokio::test]
async fn three_hour_idle_retains_last_observation_and_still_expires_after_the_policy_margin() {
    static TIME:AtomicU64=AtomicU64::new(0);
    let policy=homesense_config::RefreshPolicy::default();
    let mut fixture=Fixture::with_policy(false,&TIME,policy).await;
    fixture.feedback.set_managed_max_age_ms(policy.observation_max_age_ms()).unwrap();
    fixture.observe(0x15,30,0);
    TIME.store(policy.idle_interval_ms,Ordering::Release);
    let view=snapshot(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback).await.unwrap();
    assert_eq!(view.devices[0].state.quality,Quality::Confirmed);
    assert_eq!(view.devices[0].state.observed_at_ms,Some(0),"never restamp cached feedback");
    TIME.store(policy.observation_max_age_ms(),Ordering::Release);
    let view=snapshot(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback).await.unwrap();
    assert_eq!(view.devices[0].state.quality,Quality::Stale);
    assert_eq!(fixture.feedback.snapshot().devices[&fixture.id].observed_at_ms,Some(0));
    fixture.close().await;
}
