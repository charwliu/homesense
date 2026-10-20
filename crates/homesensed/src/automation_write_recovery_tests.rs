//! A failed local write is not proof of device execution and is never replayed.
use crate::{
    admission::{Admission, Options},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
};
use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent, rule::Budget};
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

struct FaultWire {
    kind: io::ErrorKind,
    prefix: usize,
    writes: Arc<Mutex<Vec<u8>>>,
    reject_audit: Option<std::path::PathBuf>,
}
impl Read for FaultWire {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
impl Write for FaultWire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.prefix == 0 {
            if let Some(path) = self.reject_audit.take() {
                rusqlite::Connection::open(path).unwrap().execute_batch(
                    "CREATE TRIGGER reject_write_fault_audit BEFORE UPDATE ON command_journal BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;"
                ).unwrap();
            }
            return Err(self.kind.into());
        }
        let count = self.prefix.min(bytes.len());
        self.writes
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        self.prefix -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn failed_write(
    led: bool,
    prefix: usize,
    kind: io::ErrorKind,
    recoverable: bool,
    audit_failure: bool,
) {
    let dir = std::env::temp_dir().join(format!("automation-write-recovery-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let database = dir.join("runtime.sqlite");
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation-home.json")).unwrap();
    devices["gateways"][0]["modules"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "address":"41", "profile":"ts6_86_rs485_v2_6_5_6key", "led_mode":"manual",
            "verification":{"status":"simulated"}
        }));
    devices["devices"].as_array_mut().unwrap().push(serde_json::json!({
        "id":"led", "name":"Synthetic LED", "gateway":"simulation", "address":"41", "channel":1, "enabled":true
    }));
    let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let bundle = homesense_storage::Store::open(&database)
        .unwrap()
        .publish_automation(
            None,
            &serde_json::to_vec(&devices).unwrap(),
            rules,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let controller = Uuid::new_v4();
    let actor = Admission::open(
        database.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    let generation = actor.task_generation().await.unwrap();
    let mut runtime = AutomationRuntime::load(
        actor.clone(),
        controller,
        1_800_000_000_000,
        1000,
        8,
        Budget::default(),
    )
    .await
    .unwrap();
    if led {
        actor
            .submit_panel_led(controller, DeviceId::new("led").unwrap(), true)
            .await
            .unwrap();
    } else {
        actor
            .submit(homesense_hap::requests::Batch {
                controller,
                subscriptions: vec![],
                intents: vec![homesense_hap::requests::Intent {
                    device: DeviceId::new("simulation:light").unwrap(),
                    command: Command::SetLevel {
                        level: Percent::new(37).unwrap(),
                        transition: LevelTransition::Immediate,
                    },
                }],
            })
            .await
            .unwrap();
    }
    let writes = Arc::new(Mutex::new(Vec::new()));
    let feedback = Feedback::new(
        &bundle.configuration,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            actor.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(
                    FaultWire {
                        kind,
                        prefix,
                        writes: writes.clone(),
                        reject_audit: audit_failure.then(|| database.clone()),
                    },
                    1,
                ),
            )]
            .into(),
        ),
        feedback,
    );
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        runtime.drive_recoverable(&mut owner, Duration::from_millis(1), 5000, stopped, || {
            Ok(1_800_000_000_000)
        }),
    )
    .await
    .unwrap();
    let state = actor.diagnostics(controller, generation).await.unwrap();
    let expected = if led {
        [0xae, 0x41, 1, 0xf1, 0xee]
    } else {
        [0xae, 0x15, 1, 37, 0xee]
    };
    assert_eq!(
        *writes.lock().unwrap(),
        expected[..prefix],
        "failed write was replayed"
    );
    let sql = rusqlite::Connection::open(&database).unwrap();
    let unsafe_or_written: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing','written')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        unsafe_or_written,
        i64::from(audit_failure),
        "must not claim failed durable audit committed"
    );
    assert_eq!(owner.audited_write_failure_gateway().is_some(), recoverable);
    drop(sql);
    drop(runtime);
    drop(owner);
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    if recoverable {
        assert_eq!(
            outcome.unwrap(),
            super::Exit::Disconnected {
                gateway: "simulation".into()
            }
        );
        assert!(
            !state.stopping,
            "only the supervisor owns recovery or shutdown"
        );
    } else {
        assert!(outcome.is_err(), "non-network failure must remain fatal");
        assert!(
            state.stopping,
            "invariant/permission failure must retain the fail-closed guard"
        );
    }
}

#[tokio::test]
async fn broken_pipe_led_hands_off_without_replay() {
    failed_write(true, 0, io::ErrorKind::BrokenPipe, true, false).await;
}
#[tokio::test]
async fn partial_led_write_hands_off_without_replay() {
    failed_write(true, 2, io::ErrorKind::BrokenPipe, true, false).await;
}
#[tokio::test]
async fn actuator_connection_reset_hands_off_without_replay() {
    failed_write(false, 0, io::ErrorKind::ConnectionReset, true, false).await;
}
#[tokio::test]
async fn led_permission_failure_stays_fatal() {
    failed_write(true, 0, io::ErrorKind::PermissionDenied, false, false).await;
}
#[tokio::test]
async fn led_other_write_failure_stays_fatal() {
    failed_write(true, 0, io::ErrorKind::Other, false, false).await;
}
#[tokio::test]
async fn failed_partial_write_audit_cannot_authorize_recovery() {
    failed_write(true, 2, io::ErrorKind::BrokenPipe, false, true).await;
}
