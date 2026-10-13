use super::*;
use crate::{
    admission::{Admission, Options},
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
};
use serde_json::json;
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn document(profile: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"module","address":"15","profile":profile,"verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"module","channel":1,"enabled":true},{"id":"disabled","name":"Disabled fixture","module_id":"module","channel":2,"enabled":false}]})).unwrap()
}
fn configuration(profile: &str) -> Configuration {
    Configuration::parse(&document(profile), ExecutionContext::Simulation).unwrap()
}
type Writes = Arc<Mutex<Vec<(Instant, Vec<u8>)>>>;
struct Wire {
    writes: Writes,
    reply: Vec<u8>,
    fail: bool,
    partial: bool,
}
impl Read for Wire {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.reply.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = output.len().min(self.reply.len());
        output[..n].copy_from_slice(&self.reply[..n]);
        self.reply.drain(..n);
        Ok(n)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail {
            if !self.partial {
                self.partial = true;
                return Ok(2);
            }
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.writes
            .lock()
            .unwrap()
            .push((Instant::now(), bytes.to_vec()));
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
async fn fixture(
    profile: &str,
    options: Options,
    authorized: Arc<std::sync::atomic::AtomicBool>,
) -> (
    std::path::PathBuf,
    Admission,
    Configuration,
    uuid::Uuid,
    crate::admission::TaskGeneration,
) {
    let path = std::env::temp_dir().join(format!("module-query-{}.sqlite", uuid::Uuid::new_v4()));
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_configuration(None, &document(profile), ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..options
        },
        move |id| id == controller && authorized.load(std::sync::atomic::Ordering::Acquire),
    )
    .await
    .unwrap();
    let generation = admission.task_generation().await.unwrap();
    (
        path,
        admission,
        configuration(profile),
        controller,
        generation,
    )
}
fn owner(
    admission: &Admission,
    config: &Configuration,
    generation: &crate::admission::TaskGeneration,
    reply: Vec<u8>,
    fail: bool,
) -> (ObservedTransport<Wire>, Writes) {
    let writes = Arc::new(Mutex::new(vec![]));
    let transport = TransportOwner::new(
        admission.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                Wire {
                    writes: writes.clone(),
                    reply,
                    fail,
                    partial: false,
                },
                generation.epochs.transport,
            ),
        )]
        .into(),
    );
    let feedback = crate::feedback::Feedback::new(
        config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        generation.epochs.transport,
    )
    .unwrap();
    (ObservedTransport::new(transport, feedback), writes)
}
fn clean(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
}
#[test]
fn whitelist_and_individual_fallback_preserve_context_and_channel_checks() {
    for (profile, count) in [
        ("ad8_din_rs485_v2_2_1", 1),
        ("gt_e4_rl_v3_2_2_2", 1),
        ("gt_d8_rl_v2_2_6_2", 1),
        ("gt_l8_rl_v2_0_7_2", 1),
        ("r8i8_din_pro_v2_7_7", 8),
        ("gt_c8_rl_v2_8_2_2", 8),
    ] {
        let config = configuration(profile);
        let whole = plan(
            &config,
            ExecutionContext::Simulation,
            "module",
            &Scope::WholeModule,
        )
        .unwrap();
        assert_eq!(whole.len(), count);
        assert_eq!(whole[0].channel.is_none(), count == 1);
        assert!(
            plan(
                &config,
                ExecutionContext::Live,
                "module",
                &Scope::WholeModule
            )
            .is_err()
        );
        for channel in [0, 17] {
            assert!(
                plan(
                    &config,
                    ExecutionContext::Simulation,
                    "module",
                    &Scope::Channel { channel }
                )
                .is_err()
            );
        }
        assert_eq!(
            plan(
                &config,
                ExecutionContext::Simulation,
                "module",
                &Scope::Channel { channel: 2 }
            )
            .unwrap()
            .len(),
            1
        );
    }
    for profile in ["c4_din_pro_v2_7_7", "ts6_86_rs485_v2_6_5_6key"] {
        assert!(
            plan(
                &configuration(profile),
                ExecutionContext::Simulation,
                "module",
                &Scope::WholeModule
            )
            .is_err()
        );
    }
    assert!(
        plan(
            &configuration("ad8_din_rs485_v2_2_1"),
            ExecutionContext::Simulation,
            "missing",
            &Scope::WholeModule
        )
        .is_err()
    );
}
#[tokio::test]
async fn unbound_and_disabled_reads_use_real_owner_without_creating_control_or_false_feedback() {
    let (path, admission, config, controller, generation) = fixture(
        "ad8_din_rs485_v2_2_1",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    let (mut owner, writes) = owner(
        &admission,
        &config,
        &generation,
        vec![0xae, 0x15, 0xd3, 37, 0xee],
        false,
    );
    let before = owner.snapshot();
    for channel in [2, 3] {
        assert!(crate::management_modules::control_device(&config, "module", channel).is_err());
        let receipt = admission
            .module_query(
                controller,
                generation.clone(),
                "module".into(),
                Scope::Channel { channel },
                true,
            )
            .await
            .unwrap();
        let id = receipt.commands[0].parse().unwrap();
        assert_eq!(
            admission
                .module_query_records(controller, generation.clone(), vec![id])
                .await
                .unwrap()[0]
                .status,
            "accepted"
        );
        owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(
            admission
                .module_query_records(controller, generation.clone(), vec![id])
                .await
                .unwrap()[0]
                .status,
            "written"
        );
        assert_eq!(owner.snapshot(), before);
    }
    owner.receive("bus").await.unwrap();
    let sample = owner
        .feedback()
        .module_truth()
        .module_state("module", 3)
        .unwrap();
    assert_eq!(
        sample.observed,
        Some(homesense_core::Observation::Level(
            homesense_core::Percent::new(37).unwrap()
        ))
    );
    assert_eq!(sample.quality, homesense_state::Quality::Confirmed);
    assert_eq!(owner.snapshot(), before);
    assert_eq!(config.logical_devices().len(), 2);
    assert!(
        admission
            .submit(homesense_hap::requests::Batch {
                controller,
                intents: vec![homesense_hap::requests::Intent {
                    device: DeviceId::new("disabled").unwrap(),
                    command: homesense_core::Command::Switch(true)
                }],
                subscriptions: vec![]
            })
            .await
            .is_err()
    );
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![0xae, 0x15, 0xa2, 0xf2, 0xee],
            vec![0xae, 0x15, 0xa3, 0xf2, 0xee]
        ]
    );
    admission.shutdown().await.unwrap();
    drop(owner);
    clean(&path);
}
#[tokio::test]
async fn fallback_and_control_share_ordinary_priority_and_write_pacing() {
    let (path, admission, config, controller, generation) = fixture(
        "r8i8_din_pro_v2_7_7",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    let (mut owner, writes) = owner(&admission, &config, &generation, vec![], false);
    assert_eq!(
        admission
            .module_query(
                controller,
                generation.clone(),
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .unwrap()
            .commands
            .len(),
        8
    );
    admission
        .submit(homesense_hap::requests::Batch {
            controller,
            intents: vec![homesense_hap::requests::Intent {
                device: DeviceId::new("light").unwrap(),
                command: homesense_core::Command::Switch(true),
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    for _ in 0..9 {
        owner.step().await.unwrap().unwrap().result.unwrap();
    }
    {
        let values = writes.lock().unwrap();
        assert_eq!(values[0].1, vec![0xae, 0x15, 1, 0xf1, 0xee]);
        for (index, (_, bytes)) in values.iter().skip(1).enumerate() {
            assert_eq!(*bytes, vec![0xae, 0x15, 0xa1 + index as u8, 0xf2, 0xee]);
        }
        assert!(
            values
                .windows(2)
                .all(|pair| pair[1].0.duration_since(pair[0].0) >= Duration::from_millis(20))
        );
    }
    admission.shutdown().await.unwrap();
    drop(owner);
    clean(&path);
}
#[tokio::test]
async fn capacity_expiry_and_revoked_authority_reject_read_tickets_without_writes() {
    let authorized = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (path, admission, config, controller, generation) = fixture(
        "r8i8_din_pro_v2_7_7",
        Options {
            command_capacity: 2,
            request_ttl: Duration::from_millis(20),
            ..Default::default()
        },
        authorized.clone(),
    )
    .await;
    let (mut owner, writes) = owner(&admission, &config, &generation, vec![], false);
    assert!(
        admission
            .module_query(
                controller,
                generation.clone(),
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .is_err()
    );
    let receipt = admission
        .module_query(
            controller,
            generation.clone(),
            "module".into(),
            Scope::Channel { channel: 3 },
            true,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(
        admission
            .module_query_records(
                controller,
                generation.clone(),
                vec![receipt.commands[0].parse().unwrap()]
            )
            .await
            .unwrap()[0]
            .status,
        "cancelled"
    );
    admission
        .module_query(
            controller,
            generation.clone(),
            "module".into(),
            Scope::Channel { channel: 3 },
            true,
        )
        .await
        .unwrap();
    authorized.store(false, std::sync::atomic::Ordering::Release);
    assert!(owner.step().await.unwrap().is_none());
    assert!(writes.lock().unwrap().is_empty());
    admission.shutdown().await.unwrap();
    drop(owner);
    clean(&path);
}
#[tokio::test]
async fn short_write_is_uncertain_and_restart_never_replays_read_ticket() {
    let (path, admission, config, controller, generation) = fixture(
        "ad8_din_rs485_v2_2_1",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    let (mut owner, _) = owner(&admission, &config, &generation, vec![], true);
    let receipt = admission
        .module_query(
            controller,
            generation.clone(),
            "module".into(),
            Scope::WholeModule,
            true,
        )
        .await
        .unwrap();
    let id = receipt.commands[0].parse().unwrap();
    assert!(owner.step().await.unwrap().unwrap().result.is_err());
    assert_eq!(
        homesense_storage::Store::open(&path)
            .unwrap()
            .command_status(&receipt.boot, id)
            .unwrap(),
        Some(homesense_storage::journal::Status::Uncertain)
    );
    assert!(
        admission
            .module_query(
                controller,
                generation.clone(),
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .is_err()
    );
    admission.shutdown().await.unwrap();
    drop(owner);
    let next = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    assert!(
        next.module_query_records(controller, generation, vec![id])
            .await
            .is_err()
    );
    assert!(next.dispatch().await.unwrap().is_none());
    next.shutdown().await.unwrap();
    clean(&path);
}
#[tokio::test]
async fn caller_deadline_and_stale_transport_or_configuration_scope_never_enqueue_reads() {
    let (path, admission, _config, controller, generation) = fixture(
        "ad8_din_rs485_v2_2_1",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    assert!(
        admission
            .module_query_until(
                controller,
                generation.clone(),
                "module".into(),
                Scope::WholeModule,
                true,
                Instant::now() - Duration::from_millis(1)
            )
            .await
            .is_err()
    );
    for configuration in [false, true] {
        let mut stale = generation.clone();
        if configuration {
            stale.epochs.configuration += 1;
        } else {
            stale.epochs.transport += 1;
        }
        assert!(
            admission
                .module_query(controller, stale, "module".into(), Scope::WholeModule, true)
                .await
                .is_err()
        );
    }
    let (foreign_path, foreign, _, foreign_controller, foreign_generation) = fixture(
        "ad8_din_rs485_v2_2_1",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    assert!(
        admission
            .module_query(
                controller,
                foreign_generation,
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .is_err()
    );
    assert!(
        admission
            .module_query(
                foreign_controller,
                generation,
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .is_err()
    );
    assert!(admission.dispatch().await.unwrap().is_none());
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    admission.shutdown().await.unwrap();
    foreign.shutdown().await.unwrap();
    drop(sql);
    clean(&path);
    clean(&foreign_path);
}
#[tokio::test]
async fn replacement_publication_cancels_queued_reads_and_never_writes_old_locator() {
    let (path, admission, config, controller, generation) = fixture(
        "ad8_din_rs485_v2_2_1",
        Options::default(),
        Arc::new(true.into()),
    )
    .await;
    let (mut owner, writes) = owner(&admission, &config, &generation, vec![], false);
    let receipt = admission
        .module_query(
            controller,
            generation.clone(),
            "module".into(),
            Scope::WholeModule,
            true,
        )
        .await
        .unwrap();
    let mut changed: serde_json::Value =
        serde_json::from_slice(&document("ad8_din_rs485_v2_2_1")).unwrap();
    changed["revision"] = 2.into();
    changed["gateways"][0]["modules"][0]["address"] = "61".into();
    admission
        .begin_publication(
            serde_json::to_vec(&changed).unwrap(),
            1,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(admission.commit_publication().await.unwrap(), 2);
    assert!(
        admission
            .module_query(
                controller,
                generation,
                "module".into(),
                Scope::WholeModule,
                true
            )
            .await
            .is_err()
    );
    assert!(owner.step().await.unwrap().is_none());
    assert!(writes.lock().unwrap().is_empty());
    assert_eq!(
        homesense_storage::Store::open(&path)
            .unwrap()
            .command_status(&receipt.boot, receipt.commands[0].parse().unwrap())
            .unwrap(),
        Some(homesense_storage::journal::Status::Cancelled)
    );
    admission.shutdown().await.unwrap();
    drop(owner);
    clean(&path);
}
