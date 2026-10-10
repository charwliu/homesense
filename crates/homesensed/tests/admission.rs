use hap::server::WriteStatus;
use homesense_core::{Command, DeviceId, ExecutionContext};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::{Store, journal::Status};
use homesensed::admission::{Admission, Options};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("homesense-actor-{}.sqlite", Uuid::new_v4()));
        let mut store = Store::open(&path).unwrap();
        store
            .publish_configuration(
                None,
                include_bytes!("../../../config/simulation.json"),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        Self(path)
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
        let _ = std::fs::remove_file(self.0.with_extension("runtime.lock"));
    }
}
fn batch(controller: Uuid) -> Batch {
    Batch {
        controller,
        intents: vec![Intent {
            device: DeviceId::new("simulation:light").unwrap(),
            command: Command::SetLevel {
                level: homesense_core::Percent::new(30).unwrap(),
                transition: homesense_core::LevelTransition::Immediate,
            },
        }],
        subscriptions: vec![],
    }
}
fn options() -> Options {
    Options {
        context: ExecutionContext::Simulation,
        ..Options::default()
    }
}
#[tokio::test]
async fn async_acceptance_is_durable_and_rechecks_authorization() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let allowed = Arc::new(AtomicBool::new(true));
    let check = allowed.clone();
    let actor = Admission::open(db.0.clone(), options(), move |controller| {
        controller == id && check.load(Ordering::Acquire)
    })
    .await
    .unwrap();
    assert!(
        Admission::open(db.0.clone(), options(), |_| true)
            .await
            .is_err()
    );
    use hap::server::WriteBatchHandler;
    use homesense_hap::{
        requests::{LightBinding, LightRequests},
        write::AsyncLightWriteHandler,
    };
    let requests = LightRequests::new(vec![LightBinding {
        device: DeviceId::new("simulation:light").unwrap(),
        aid: 2,
        on_iid: 10,
        brightness_iid: Some(11),
        default_on_level: None,
    }])
    .unwrap();
    let owner = actor.clone();
    let adapter = AsyncLightWriteHandler::new(
        requests,
        |_: &DeviceId| None,
        move |batch| {
            let owner = owner.clone();
            async move { owner.submit(batch).await.map(|_| ()) }
        },
    );
    adapter
        .write_async(
            id,
            br#"{"characteristics":[{"aid":2,"iid":11,"value":30}]}"#,
        )
        .await
        .unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    allowed.store(false, Ordering::Release);
    assert_eq!(
        actor.submit(batch(id)).await,
        Err(WriteStatus::InsufficientPrivileges)
    );
    assert_eq!(store.command_status(actor.boot_id(), 2).unwrap(), None);
    let boot = actor.boot_id().to_owned();
    actor.shutdown().await.unwrap();
    let reopened = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    assert_eq!(
        store.command_status(&boot, 1).unwrap(),
        Some(Status::Uncertain)
    );
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn storage_error_stops_subsequent_acceptance() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_accept BEFORE INSERT ON command_journal BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        actor.submit(batch(id)).await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    sql.execute_batch("DROP TRIGGER reject_accept;").unwrap();
    assert_eq!(
        actor.submit(batch(id)).await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.command_status(actor.boot_id(), 1).unwrap(), None);
    actor.shutdown().await.unwrap();
}
#[tokio::test]
async fn bounded_mailbox_rejects_overload_and_expired_waiting_request() {
    use hap::futures::poll;
    use std::{
        sync::{Condvar, Mutex},
        task::Poll,
    };
    let db = Database::new();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let wait = gate.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    let actor = Admission::open(
        db.0.clone(),
        Options {
            mailbox_capacity: 1,
            request_ttl: Duration::from_millis(20),
            ..options()
        },
        move |_| {
            notify.notify_one();
            let (lock, cv) = &*wait;
            let mut release = lock.lock().unwrap();
            while !*release {
                release = cv.wait(release).unwrap();
            }
            true
        },
    )
    .await
    .unwrap();
    let id = Uuid::new_v4();
    let mut first = Box::pin(actor.submit(batch(id)));
    assert!(matches!(poll!(first.as_mut()), Poll::Pending));
    entered.notified().await;
    let mut second = Box::pin(actor.submit(batch(id)));
    assert!(matches!(poll!(second.as_mut()), Poll::Pending));
    assert_eq!(
        actor.submit(batch(id)).await,
        Err(WriteStatus::ResourceBusy)
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    assert_eq!(first.await, Err(WriteStatus::ServiceCommunicationFailure));
    assert_eq!(second.await, Err(WriteStatus::ServiceCommunicationFailure));
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(actor.boot_id(), 1)
            .unwrap(),
        None
    );
    actor.shutdown().await.unwrap();
}
