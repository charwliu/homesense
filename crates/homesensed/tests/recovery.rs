use homesense_core::{DeviceId, ExecutionContext, Observation, Percent};
use homesense_state::Quality;
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
};
use std::collections::{BTreeMap, VecDeque};
use uuid::Uuid;

struct ReplyStream {
    input: VecDeque<u8>,
    level: u8,
}
impl ReplyStream {
    fn new(level: u8, initial: bool) -> Self {
        Self {
            input: if initial {
                vec![0xAE, 0x15, 0xD1, level, 0xEE].into()
            } else {
                VecDeque::new()
            },
            level,
        }
    }
}
impl std::io::Read for ReplyStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let count = out.len().min(self.input.len());
        for byte in &mut out[..count] {
            *byte = self.input.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl std::io::Write for ReplyStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes == [0xAE, 0x15, 0xA1, 0xF2, 0xEE] {
            self.input.extend([0xAE, 0x15, 0xD1, self.level, 0xEE]);
        } else {
            assert_eq!(bytes, [0xAE, 0x15, 0x01, 30, 0xEE]);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn recovery_retains_stale_state_until_fresh_generation_feedback() {
    let root = std::env::temp_dir().join(format!("homesense-recovery-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let document = include_bytes!("../../../config/simulation-home.json");
    let config =
        homesense_config::Configuration::parse(document, ExecutionContext::Simulation).unwrap();
    let mut store = Store::open(&path).unwrap();
    store
        .publish_configuration(None, document, ExecutionContext::Simulation, &[])
        .unwrap();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let connection = |gateway: &str, epoch, level| {
        TransportOwner::new(
            admission.clone(),
            BTreeMap::from([(
                gateway.to_owned(),
                hb_transport::Connection::for_simulation(
                    ReplyStream::new(level, epoch == 1),
                    epoch,
                ),
            )]),
        )
    };
    let mut owner = ObservedTransport::new(connection("simulation", 1, 27), feedback);
    let id = DeviceId::new("simulation:light").unwrap();
    owner.receive("simulation").await.unwrap();
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Confirmed);
    assert!(owner.recover(connection("simulation", 2, 42), 2).is_err());
    owner.receive("simulation").await.unwrap(); // EOF invalidates the old generation.
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Stale);
    assert!(owner.recover(connection("wrong", 2, 42), 2).is_err());
    assert!(owner.recover(connection("simulation", 1, 42), 1).is_err());
    assert!(owner.recover(connection("simulation", 3, 42), 2).is_err());
    owner.recover(connection("simulation", 2, 42), 2).unwrap();
    let mut next: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    next["revision"] = 2.into();
    let next = homesense_config::Configuration::parse(
        &hap::serde_json::to_vec(&next).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        owner
            .query_recovery(&next, ExecutionContext::Simulation, 1000, 1000)
            .await
            .unwrap_err()
            .contains("revision does not match")
    );
    assert!(
        owner
            .synchronize_recovery(2, 1000)
            .await
            .unwrap_err()
            .contains("revision does not match")
    );

    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Stale);
    assert!(owner.synchronize_recovery(1, 1000).await.is_err());
    owner
        .query_recovery(&config, ExecutionContext::Simulation, 1000, 1000)
        .await
        .unwrap();
    let snapshot = owner.snapshot();
    assert_eq!(snapshot.devices[&id].quality, Quality::Confirmed);
    assert_eq!(
        snapshot.devices[&id].observed,
        Some(Observation::Level(Percent::new(42).unwrap()))
    );
    assert!(owner.synchronize_recovery(99, 1000).await.is_err());
    assert!(
        owner
            .synchronize_recovery(config.revision(), 1000)
            .await
            .is_err()
    );
    let ids = admission
        .submit(homesense_hap::requests::Batch {
            controller: Uuid::new_v4(),
            intents: vec![homesense_hap::requests::Intent {
                device: id,
                command: homesense_core::Command::SetLevel {
                    level: Percent::new(30).unwrap(),
                    transition: homesense_core::LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    let sent = owner.step().await.unwrap().unwrap();
    assert_eq!(sent.command_id, ids[0]);
    assert!(sent.result.is_ok());
    assert_eq!(
        store.command_status(admission.boot_id(), ids[0]).unwrap(),
        Some(homesense_storage::journal::Status::Written)
    );
    drop(owner);
    admission.shutdown().await.unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
