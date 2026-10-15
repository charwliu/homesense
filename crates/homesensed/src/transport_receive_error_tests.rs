//! Deterministic I/O fault versus cancellation/audit-failure ownership checks.
//! Trusted recovery here tests actor lifetime only; full wire sync is probed separately.
use super::*;
use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent};
use homesense_hap::requests::{Batch, Intent};
use std::{io, path::PathBuf};
use uuid::Uuid;

async fn actor() -> (PathBuf, Admission, Uuid) {
    let directory = std::env::temp_dir().join(format!("hs-receive-guard-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.sqlite");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_configuration(
            None,
            include_bytes!("../../../config/simulation.json"),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        path,
        crate::admission::Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    (directory, admission, controller)
}
struct Reset(io::ErrorKind);
impl Read for Reset {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(self.0.into())
    }
}
impl Write for Reset {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
async fn reset(audit_failure: bool) -> (bool, bool, bool) {
    reset_kind(audit_failure, io::ErrorKind::ConnectionReset).await
}
async fn reset_kind(audit_failure: bool, kind: io::ErrorKind) -> (bool, bool, bool) {
    let (directory, admission, controller) = actor().await;
    let generation = admission.task_generation().await.unwrap();
    let db = if audit_failure {
        admission
            .submit(Batch {
                controller,
                intents: vec![Intent {
                    device: DeviceId::new("simulation:light").unwrap(),
                    command: Command::SetLevel {
                        level: Percent::new(30).unwrap(),
                        transition: LevelTransition::Immediate,
                    },
                }],
                subscriptions: vec![],
            })
            .await
            .unwrap();
        let c = rusqlite::Connection::open(directory.join("runtime.sqlite")).unwrap();
        c.execute_batch("CREATE TRIGGER deny_fault_audit BEFORE UPDATE ON command_journal BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;").unwrap();
        Some(c)
    } else {
        None
    };
    let mut owner = TransportOwner::new(
        admission.clone(),
        [("simulation".into(), Connection::for_simulation(Reset(kind), 1))].into(),
    );
    let error = owner.receive("simulation").await.err().unwrap();
    assert!(error.starts_with("receive:"), "{error}");
    let state = admission.diagnostics(controller, generation).await.unwrap();
    if let Some(c) = db {
        c.execute_batch("DROP TRIGGER deny_fault_audit").unwrap();
    }
    let recovered = admission.recover_synchronized(2, 1).await.is_ok();
    let result = (state.failed, state.stopping, recovered);
    drop(owner);
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    result
}
#[tokio::test]
async fn audited_connection_reset_preserves_actor_for_explicit_fresh_recovery() {
    let (failed, stopping, recovered) = reset(false).await;
    assert!(failed, "I/O failure must keep control gated");
    assert!(
        !stopping,
        "completed fault audit is not future cancellation"
    );
    assert!(
        recovered,
        "trusted fresh supervisor recovery must remain possible"
    );
}
#[tokio::test]
async fn failed_fault_audit_remains_terminal_even_if_storage_recovers() {
    let (failed, stopping, recovered) = reset(true).await;
    assert!(failed && stopping);
    assert!(!recovered, "failed durable audit cannot reopen the actor");
}
struct BlockedRead {
    started: Option<tokio::sync::oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}
impl Read for BlockedRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if let Some(started) = self.started.take() {
            let _ = started.send(());
        }
        self.release
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(io::Error::other)?;
        Err(io::ErrorKind::ConnectionReset.into())
    }
}
impl Write for BlockedRead {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn cancelling_a_pending_receive_still_permanently_closes_actor() {
    let (directory, admission, controller) = actor().await;
    let generation = admission.task_generation().await.unwrap();
    let (began, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let mut owner = TransportOwner::new(
        admission.clone(),
        [(
            "simulation".into(),
            Connection::for_simulation(
                BlockedRead {
                    started: Some(began),
                    release: blocked,
                },
                1,
            ),
        )]
        .into(),
    );
    let task = tokio::spawn(async move { owner.receive("simulation").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    release.send(()).unwrap();
    let state = admission.diagnostics(controller, generation).await.unwrap();
    let recovered = admission.recover_synchronized(2, 1).await.is_ok();
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(state.stopping);
    assert!(!recovered, "real cancellation remains terminal");
}

#[tokio::test]
async fn non_network_read_errors_remain_terminal_after_successful_audit() {
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData, io::ErrorKind::Other] {
        let (failed, stopping, recovered) = reset_kind(false, kind).await;
        assert!(failed && stopping, "non-network error must close actor: {kind:?}");
        assert!(!recovered, "non-network error cannot be reopened: {kind:?}");
    }
}
