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
        Self::with_config(include_bytes!("../../../config/simulation.json"))
    }
    fn with_config(document: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("homesense-actor-{}.sqlite", Uuid::new_v4()));
        let mut store = Store::open(&path).unwrap();
        store
            .publish_configuration(None, document, ExecutionContext::Simulation, &[])
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

#[tokio::test]
async fn ad8_acceptance_journals_compiled_ramp_not_immediate_target() {
    let document = std::str::from_utf8(include_bytes!("../../../config/simulation.json"))
        .unwrap()
        .replace("gt_d8_rl_v2_2_6_2", "ad8_din_rs485_v2_2_1");
    let db = Database::with_config(document.as_bytes());
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    actor.submit(batch(controller)).await.unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let intent: Vec<u8> = connection
        .query_row(
            "SELECT intent FROM command_journal WHERE boot_id=?1 AND command_id=1",
            [actor.boot_id()],
            |r| r.get(0),
        )
        .unwrap();
    // Journal v1 tag 3 is SetLevel/Ramp; tag 2 would be Immediate.
    assert_eq!(intent, vec![1, 3, 30, 0]);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatch_rechecks_principal_and_records_write_not_observation() {
    let db = Database::new();
    let controller = Uuid::new_v4();
    let allowed = Arc::new(AtomicBool::new(true));
    let check = allowed.clone();
    let actor = Admission::open(db.0.clone(), options(), move |id| {
        id == controller && check.load(Ordering::Acquire)
    })
    .await
    .unwrap();
    actor.submit(batch(controller)).await.unwrap();
    allowed.store(false, Ordering::Release);
    assert!(actor.dispatch().await.unwrap().is_none());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), 1).unwrap(),
        Some(Status::Cancelled)
    );
    allowed.store(true, Ordering::Release);
    actor.submit(batch(controller)).await.unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, 2);
    assert_eq!(
        store.command_status(actor.boot_id(), 2).unwrap(),
        Some(Status::Writing)
    );
    assert!(actor.dispatch().await.is_err());
    assert!(actor.finish_write(99, true).await.is_err());
    actor.finish_write(2, true).await.unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), 2).unwrap(),
        Some(Status::Written)
    );
    assert!(actor.finish_write(2, true).await.is_err());
    actor.submit(batch(controller)).await.unwrap();
    assert_eq!(actor.dispatch().await.unwrap().unwrap().command.id, 3);
    actor.finish_write(3, false).await.unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), 3).unwrap(),
        Some(Status::Uncertain)
    );
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_writing_audit_never_hands_frame_to_transport() {
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    actor.submit(batch(controller)).await.unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_writing BEFORE UPDATE ON command_journal WHEN NEW.status='writing' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(actor.dispatch().await.is_err());
    connection
        .execute_batch("DROP TRIGGER fail_writing;")
        .unwrap();
    assert!(actor.dispatch().await.is_err());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn abandoned_dispatch_expires_without_mailbox_traffic_and_rejects_late_success() {
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(
        db.0.clone(),
        Options {
            request_ttl: Duration::from_millis(150),
            ..options()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    actor.submit(batch(controller)).await.unwrap();
    let ticket = actor.dispatch().await.unwrap().unwrap();
    let store = Store::open(&db.0).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .command_status(actor.boot_id(), ticket.command.id)
                .unwrap()
                == Some(Status::Uncertain)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(actor.finish_write(ticket.command.id, true).await.is_err());
    assert!(actor.synchronized(1, 1).await.is_err());
    assert!(actor.submit(batch(controller)).await.is_err());
    assert!(actor.dispatch().await.is_err());
    actor.shutdown().await.unwrap();
}

struct OutputStream {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    fail_after: usize,
}
impl std::io::Read for OutputStream {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for OutputStream {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut bytes = self.bytes.lock().unwrap();
        if bytes.len() >= self.fail_after {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        let n = data.len().min(self.fail_after - bytes.len()).min(2);
        bytes.extend_from_slice(&data[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn transport_owner_routes_and_audits_full_and_partial_frames() {
    use hb_transport::Connection;
    use homesensed::transport_owner::TransportOwner;
    for limit in [5, 2] {
        let db = Database::new();
        let controller = Uuid::new_v4();
        let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
            .await
            .unwrap();
        actor.submit(batch(controller)).await.unwrap();
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stream = OutputStream {
            bytes: bytes.clone(),
            fail_after: limit,
        };
        let mut owner = TransportOwner::new(
            actor.clone(),
            [("simulation".into(), Connection::for_simulation(stream, 1))].into(),
        );
        let outcome = owner.step().await.unwrap().unwrap();
        assert_eq!(outcome.command_id, 1);
        assert_eq!(
            *bytes.lock().unwrap(),
            [0xAE, 0x15, 0x01, 30, 0xEE][..limit]
        );
        let store = Store::open(&db.0).unwrap();
        if limit == 5 {
            assert!(outcome.result.is_ok());
            assert_eq!(
                store.command_status(actor.boot_id(), 1).unwrap(),
                Some(Status::Written)
            );
            assert!(owner.step().await.unwrap().is_none());
        } else {
            assert_eq!(outcome.result.unwrap_err().bytes_written, 2);
            assert_eq!(
                store.command_status(actor.boot_id(), 1).unwrap(),
                Some(Status::Uncertain)
            );
            assert!(owner.step().await.is_err());
        }
        actor.shutdown().await.unwrap();
    }
}

struct InputStream(std::io::Cursor<Vec<u8>>);
impl std::io::Read for InputStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = out.len().min(3);
        std::io::Read::read(&mut self.0, &mut out[..n])
    }
}
impl std::io::Write for InputStream {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        panic!("read-only fixture must not write")
    }
    fn flush(&mut self) -> std::io::Result<()> {
        panic!("read-only fixture must not flush")
    }
}
#[tokio::test]
async fn receive_preserves_gateway_epoch_and_partial_framing_then_stops_on_eof() {
    use hb_transport::{Connection, Receive};
    use homesensed::transport_owner::TransportOwner;
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let stream = InputStream(std::io::Cursor::new(vec![0xAE, 0x15, 0xD1, 30, 0xEE, 0xAE]));
    let mut owner = TransportOwner::new(
        actor.clone(),
        [("simulation".into(), Connection::for_simulation(stream, 7))].into(),
    );
    let first = owner.receive("simulation").await.unwrap();
    assert_eq!(first.gateway_id, "simulation");
    assert_eq!(first.transport_epoch, 7);
    assert!(matches!(first.result,Receive::Frames { frames,.. } if frames.is_empty()));
    let second = owner.receive("simulation").await.unwrap();
    assert!(second.received_at_ms >= first.received_at_ms);
    assert!(
        matches!(second.result,Receive::Frames { transport_epoch:7,frames } if frames==vec![hb_protocol::RawFrame { address:0x15,command:0xD1,value:30 }])
    );
    assert!(matches!(
        owner.receive("simulation").await.unwrap().result,
        Receive::Disconnected {
            discarded_partial_bytes: 1
        }
    ));
    assert!(owner.receive("simulation").await.is_err());
    assert!(actor.submit(batch(controller)).await.is_err());
    assert!(actor.synchronized(1, 1).await.is_err());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn observed_transport_projects_wire_feedback_and_invalidates_snapshot_on_eof() {
    use hb_transport::Connection;
    use homesense_state::Quality;
    use homesensed::{
        feedback::Feedback, observed_transport::ObservedTransport, transport_owner::TransportOwner,
    };
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        controller.as_u128(),
        1,
    )
    .unwrap();
    let stream = InputStream(std::io::Cursor::new(vec![0xAE, 0x15, 0xD1, 30, 0xEE]));
    let transport = TransportOwner::new(
        actor.clone(),
        [("simulation".into(), Connection::for_simulation(stream, 1))].into(),
    );
    let mut owner = ObservedTransport::new(transport, feedback);
    let id = DeviceId::new("simulation:light").unwrap();
    owner.receive("simulation").await.unwrap();
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Unknown);
    owner.receive("simulation").await.unwrap();
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Confirmed);
    let cursor = owner.snapshot().cursor;
    owner.receive("simulation").await.unwrap();
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Stale);
    let events = owner.events_since(cursor).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.change == homesense_state::Change::Quality
                && event.state.quality == Quality::Stale)
    );
    assert!(owner.step().await.is_err());
    actor.shutdown().await.unwrap();
}

struct CancelReadStream {
    first: bool,
    started: Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Receiver<()>,
    dropped: Arc<tokio::sync::Notify>,
}
impl Drop for CancelReadStream {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}
impl std::io::Read for CancelReadStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let value = if self.first {
            self.first = false;
            30
        } else {
            self.started.notify_one();
            self.release
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
            70
        };
        out[..5].copy_from_slice(&[0xAE, 0x15, 0xD1, value, 0xEE]);
        Ok(5)
    }
}
impl std::io::Write for CancelReadStream {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        panic!("unexpected write")
    }
    fn flush(&mut self) -> std::io::Result<()> {
        panic!("unexpected flush")
    }
}
#[tokio::test]
async fn cancelling_blocked_read_emits_stale_and_never_projects_late_bytes() {
    use homesense_state::{Change, Quality};
    use homesensed::{
        feedback::Feedback, observed_transport::ObservedTransport, transport_owner::TransportOwner,
    };
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        controller.as_u128(),
        1,
    )
    .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let (release, wait) = std::sync::mpsc::channel();
    let stream = CancelReadStream {
        first: true,
        started: started.clone(),
        release: wait,
        dropped: dropped.clone(),
    };
    let transport = TransportOwner::new(
        actor.clone(),
        [(
            "simulation".into(),
            hb_transport::Connection::for_simulation(stream, 1),
        )]
        .into(),
    );
    let mut owner = ObservedTransport::new(transport, feedback);
    owner.receive("simulation").await.unwrap();
    let id = DeviceId::new("simulation:light").unwrap();
    let cursor = owner.snapshot().cursor;
    {
        let future = owner.receive("simulation");
        tokio::pin!(future);
        tokio::select! {
            _=started.notified()=>{},
            result=&mut future=>panic!("read completed before release: {}",result.is_ok()),
            _=tokio::time::sleep(Duration::from_secs(2))=>panic!("read never started"),
        }
    }
    assert_eq!(owner.snapshot().devices[&id].quality, Quality::Stale);
    assert_eq!(
        owner.events_since(cursor).unwrap()[0].change,
        Change::Quality
    );
    assert!(actor.submit(batch(controller)).await.is_err());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), dropped.notified())
        .await
        .unwrap();
    assert_eq!(
        owner.snapshot().devices[&id].observed,
        Some(homesense_core::Observation::Level(
            homesense_core::Percent::new(30).unwrap()
        ))
    );
    assert!(owner.receive("simulation").await.is_err());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_runtime_hap_accepts_durably_and_reads_only_published_feedback() {
    use hap::{HapType, storage::Storage};
    use homesensed::{
        feedback::Feedback, observed_transport::ObservedTransport, runtime_hap::RuntimeHap,
        transport_owner::TransportOwner,
    };
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let directory = std::env::temp_dir().join(format!("homesense-runtime-hap-{}", Uuid::new_v4()));
    let mut store = homesense_hap::storage::IdentityStore::open(&directory)
        .await
        .unwrap();
    store.save_config(&hap::Config::default()).await.unwrap();
    let mut bridge = RuntimeHap::new(&config, feedback.snapshot(), &store, actor.clone(), 5000)
        .await
        .unwrap();
    let (binding, handle) = bridge.lights[0].clone();
    let request=hap::serde_json::to_vec(&hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.brightness_iid.unwrap(),"value":30}]})).unwrap();
    bridge
        .write_handler
        .write_async(controller, &request)
        .await
        .unwrap();
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(actor.boot_id(), 1)
            .unwrap(),
        Some(Status::Accepted)
    );
    {
        let mut light = handle.lock().await;
        assert!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .is_err()
        );
    }
    let stream = InputStream(std::io::Cursor::new(vec![0xAE, 0x15, 0xD1, 30, 0xEE]));
    let transport = TransportOwner::new(
        actor.clone(),
        [(
            "simulation".into(),
            hb_transport::Connection::for_simulation(stream, 1),
        )]
        .into(),
    );
    let mut owner = ObservedTransport::new(transport, feedback);
    owner.receive("simulation").await.unwrap();
    owner.receive("simulation").await.unwrap();
    bridge.publish(&owner).await.unwrap();
    {
        let mut light = handle.lock().await;
        assert_eq!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            hap::serde_json::json!(30)
        );
    }
    // A matching external observation is not an execution acknowledgment.
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(actor.boot_id(), 1)
            .unwrap(),
        Some(Status::Accepted)
    );
    owner.receive("simulation").await.unwrap();
    bridge.publish(&owner).await.unwrap();
    {
        let mut light = handle.lock().await;
        assert!(
            light
                .get_mut_service(HapType::Lightbulb)
                .unwrap()
                .get_mut_characteristic(HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .is_err()
        );
    }
    actor.shutdown().await.unwrap();
    drop(bridge);
    drop(handle);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cancelled_graph_installation_stops_durable_admission() {
    use hap::{server::Server, storage::Storage};
    use homesensed::{feedback::Feedback, runtime_hap::RuntimeHap};
    let db = Database::new();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let directory =
        std::env::temp_dir().join(format!("homesense-install-cancel-{}", Uuid::new_v4()));
    let mut store = homesense_hap::storage::IdentityStore::open(&directory)
        .await
        .unwrap();
    let settings = hap::Config {
        host: "127.0.0.1".parse().unwrap(),
        ..Default::default()
    };
    store.save_config(&settings).await.unwrap();
    let graph = RuntimeHap::new(&config, feedback.snapshot(), &store, actor.clone(), 5000)
        .await
        .unwrap();
    let server = hap::server::IpServer::new_loopback(settings, store.clone())
        .await
        .unwrap();
    let storage = server.storage_pointer();
    let lock = storage.lock().await;
    // Force cancellation during persistent graph installation, before it can
    // publish the handles. This server never starts a listener.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), graph.install(&server))
            .await
            .is_err()
    );
    drop(lock);
    assert!(actor.submit(batch(controller)).await.is_err());
    assert_eq!(
        Store::open(&db.0)
            .unwrap()
            .command_status(actor.boot_id(), 1)
            .unwrap(),
        None
    );
    actor.shutdown().await.unwrap();
    drop(graph);
    drop(storage);
    drop(server);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn transport_failure_durably_cancels_queue_and_marks_inflight_uncertain() {
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let controller = Uuid::new_v4();
    let first = actor.submit(batch(controller)).await.unwrap()[0];
    let ticket = actor.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, first);
    let second = actor.submit(batch(controller)).await.unwrap()[0];
    actor.transport_failed().await.unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), first).unwrap(),
        Some(Status::Uncertain)
    );
    assert_eq!(
        store.command_status(actor.boot_id(), second).unwrap(),
        Some(Status::Cancelled)
    );
    assert!(actor.finish_write(first, true).await.is_err());
    assert!(actor.dispatch().await.is_err());
    assert!(actor.submit(batch(controller)).await.is_err());
    actor.transport_failed().await.unwrap(); // Repeated fault notification is idempotent.
    drop(store);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_transport_audit_rolls_back_and_keeps_admission_closed() {
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let controller = Uuid::new_v4();
    let first = actor.submit(batch(controller)).await.unwrap()[0];
    actor.dispatch().await.unwrap().unwrap();
    let second = actor.submit(batch(controller)).await.unwrap()[0];
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_fault_audit BEFORE UPDATE ON command_journal WHEN NEW.status='uncertain' BEGIN SELECT RAISE(ABORT, 'injected fault audit failure'); END;").unwrap();
    assert_eq!(
        actor.transport_failed().await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), first).unwrap(),
        Some(Status::Writing)
    );
    assert_eq!(
        store.command_status(actor.boot_id(), second).unwrap(),
        Some(Status::Accepted)
    );
    assert!(actor.dispatch().await.is_err());
    assert!(actor.submit(batch(controller)).await.is_err());
    assert!(actor.synchronized(1, 1).await.is_err());
    sql.execute_batch("DROP TRIGGER reject_fault_audit;")
        .unwrap();
    actor.transport_failed().await.unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), first).unwrap(),
        Some(Status::Uncertain)
    );
    assert_eq!(
        store.command_status(actor.boot_id(), second).unwrap(),
        Some(Status::Cancelled)
    );
    drop(sql);
    drop(store);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn incomplete_write_and_lease_expiry_cancel_waiting_commands() {
    for expires in [false, true] {
        let db = Database::new();
        let mut settings = options();
        settings.request_ttl = Duration::from_millis(100);
        let actor = Admission::open(db.0.clone(), settings, |_| true)
            .await
            .unwrap();
        let controller = Uuid::new_v4();
        let first = actor.submit(batch(controller)).await.unwrap()[0];
        actor.dispatch().await.unwrap().unwrap();
        let second = actor.submit(batch(controller)).await.unwrap()[0];
        if expires {
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(actor.finish_write(first, true).await.is_err());
        } else {
            actor.finish_write(first, false).await.unwrap();
        }
        let store = Store::open(&db.0).unwrap();
        assert_eq!(
            store.command_status(actor.boot_id(), first).unwrap(),
            Some(Status::Uncertain)
        );
        assert_eq!(
            store.command_status(actor.boot_id(), second).unwrap(),
            Some(Status::Cancelled)
        );
        assert!(actor.dispatch().await.is_err());
        drop(store);
        actor.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn synchronized_recovery_accepts_only_new_work_in_a_fresh_generation() {
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let controller = Uuid::new_v4();
    assert!(actor.recover_synchronized(2, 1).await.is_err());
    let old = actor.submit(batch(controller)).await.unwrap()[0];
    actor.transport_failed().await.unwrap();
    assert!(actor.recover_synchronized(1, 1).await.is_err());
    assert!(actor.recover_synchronized(2, 99).await.is_err());
    actor.recover_synchronized(2, 1).await.unwrap();
    assert!(actor.dispatch().await.unwrap().is_none());
    let new = actor.submit(batch(controller)).await.unwrap()[0];
    assert!(new > old);
    let ticket = actor.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, new);
    assert_eq!(ticket.command.epochs.transport, 2);
    actor.finish_write(new, true).await.unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), old).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        store.command_status(actor.boot_id(), new).unwrap(),
        Some(Status::Written)
    );
    drop(store);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_rejects_changed_configuration_even_with_same_revision() {
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    actor.transport_failed().await.unwrap();
    let store = Store::open(&db.0).unwrap();
    let original = store.active().unwrap().unwrap();
    let mut changed = original.payload.clone();
    changed.push(b' '); // Still valid JSON with the same revision, but not the loaded snapshot.
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    // Fault injection only: normal writes are already blocked by the immutable
    // revision trigger. Simulate an externally damaged same-revision snapshot.
    sql.execute_batch("DROP TRIGGER immutable_configuration_update;")
        .unwrap();
    sql.execute(
        "UPDATE configuration_revision SET payload=?1 WHERE revision=?2",
        rusqlite::params![changed, i64::try_from(original.revision).unwrap()],
    )
    .unwrap();
    assert!(
        actor
            .recover_synchronized(2, original.revision)
            .await
            .is_err()
    );
    assert!(actor.submit(batch(Uuid::new_v4())).await.is_err());
    sql.execute(
        "UPDATE configuration_revision SET payload=?1 WHERE revision=?2",
        rusqlite::params![original.payload, i64::try_from(original.revision).unwrap()],
    )
    .unwrap();
    actor
        .recover_synchronized(2, original.revision)
        .await
        .unwrap();
    assert!(actor.dispatch().await.unwrap().is_none());
    drop(sql);
    drop(store);
    actor.shutdown().await.unwrap();
}

fn motor_document(calibrated: bool) -> Vec<u8> {
    let mut doc = hap::serde_json::json!({
        "schema_version":1,"revision":1,
        "gateways":[{"id":"motor-test","endpoint":"127.0.0.1:50007","modules":[{
            "address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}
        }]}],
        "devices":[{"id":"motor","name":"Isolated fixture","gateway":"motor-test","address":"22","channel":2,"enabled":true}]
    });
    if calibrated {
        doc["devices"][0]["motor_calibration"] = hap::serde_json::json!({
            "open_direction":"down","stop_verified":true,"evidence":"offline fixture only","verified_at_unix_ms":1
        });
    }
    hap::serde_json::to_vec(&doc).unwrap()
}

#[tokio::test]
async fn calibrated_motor_uses_durable_owner_and_stop_cancels_pending_motion() {
    use homesense_core::MotorAction as A;
    let db = Database::with_config(&motor_document(true));
    let controller = Uuid::new_v4();
    let permitted = Arc::new(AtomicBool::new(true));
    let check = permitted.clone();
    let actor = Admission::open(db.0.clone(), options(), move |id| {
        id == controller && check.load(Ordering::Acquire)
    })
    .await
    .unwrap();
    let device = DeviceId::new("motor").unwrap();
    let opened = actor
        .submit_motor(controller, device.clone(), A::Open)
        .await
        .unwrap()[0];
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), opened).unwrap(),
        Some(Status::Accepted)
    );
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "motor-test".into(),
            hb_transport::Connection::for_simulation(
                OutputStream {
                    bytes: bytes.clone(),
                    fail_after: 100,
                },
                1,
            ),
        )]
        .into(),
    );
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(*bytes.lock().unwrap(), [0xAE, 0x22, 2, 0xF2, 0xEE]);
    assert_eq!(
        store.command_status(actor.boot_id(), opened).unwrap(),
        Some(Status::Written)
    );
    let close = actor
        .submit_motor(controller, device.clone(), A::Close)
        .await
        .unwrap()[0];
    let stop = actor
        .submit_motor(controller, device.clone(), A::Stop)
        .await
        .unwrap()[0];
    let outcome = owner.step().await.unwrap().unwrap();
    assert_eq!(outcome.command_id, stop);
    outcome.result.unwrap();
    assert_eq!(&bytes.lock().unwrap()[5..], &[0xAE, 0x22, 2, 0xF0, 0xEE]);
    assert_eq!(
        store.command_status(actor.boot_id(), close).unwrap(),
        Some(Status::Cancelled)
    );
    assert!(owner.step().await.unwrap().is_none());
    let revoked = actor
        .submit_motor(controller, device, A::Open)
        .await
        .unwrap()[0];
    permitted.store(false, Ordering::Release);
    assert!(owner.step().await.unwrap().is_none());
    assert_eq!(bytes.lock().unwrap().len(), 10);
    assert_eq!(
        store.command_status(actor.boot_id(), revoked).unwrap(),
        Some(Status::Cancelled)
    );
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn motor_entry_rejects_missing_calibration_and_raw_light_entry_bypass() {
    for calibrated in [false, true] {
        let db = Database::with_config(&motor_document(calibrated));
        let controller = Uuid::new_v4();
        let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
            .await
            .unwrap();
        let device = DeviceId::new("motor").unwrap();
        assert!(
            actor
                .submit_motor(
                    Uuid::new_v4(),
                    device.clone(),
                    homesense_core::MotorAction::Stop
                )
                .await
                .is_err()
        );
        if !calibrated {
            for action in [
                homesense_core::MotorAction::Open,
                homesense_core::MotorAction::Close,
                homesense_core::MotorAction::Stop,
            ] {
                assert!(
                    actor
                        .submit_motor(controller, device.clone(), action)
                        .await
                        .is_err()
                );
            }
        }
        assert!(
            actor
                .submit(Batch {
                    controller,
                    intents: vec![Intent {
                        device,
                        command: Command::Motor(homesense_core::MotorMotion::Up)
                    }],
                    subscriptions: vec![]
                })
                .await
                .is_err()
        );
        assert!(actor.dispatch().await.unwrap().is_none());
        actor.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn panel_led_entry_requires_manual_mode_and_uses_audited_transport() {
    for mode in ["unknown", "self_reset", "manual"] {
        let document = hap::serde_json::json!({
            "schema_version":1,"revision":1,
            "gateways":[{"id":"panel-test","endpoint":"127.0.0.1:50007","modules":[{
                "address":"41","profile":"ts6_86_rs485_v2_6_5_6key",
                "verification":{"status":"simulated"},"led_mode":mode
            }]}],
            "devices":[{"id":"key","name":"Offline key fixture","gateway":"panel-test","address":"41","channel":6,"enabled":true}]
        });
        let db = Database::with_config(&hap::serde_json::to_vec(&document).unwrap());
        let controller = Uuid::new_v4();
        let permitted = Arc::new(AtomicBool::new(true));
        let check = permitted.clone();
        let actor = Admission::open(db.0.clone(), options(), move |id| {
            id == controller && check.load(Ordering::Acquire)
        })
        .await
        .unwrap();
        let device = DeviceId::new("key").unwrap();
        assert!(
            actor
                .submit_panel_led(Uuid::new_v4(), device.clone(), true)
                .await
                .is_err()
        );
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut owner = homesensed::transport_owner::TransportOwner::new(
            actor.clone(),
            [(
                "panel-test".into(),
                hb_transport::Connection::for_simulation(
                    OutputStream {
                        bytes: bytes.clone(),
                        fail_after: 100,
                    },
                    1,
                ),
            )]
            .into(),
        );
        for active in [true, false] {
            let result = actor
                .submit_panel_led(controller, device.clone(), active)
                .await;
            if mode != "manual" {
                assert!(result.is_err());
                assert!(owner.step().await.unwrap().is_none());
            } else {
                let id = result.unwrap()[0];
                let store = Store::open(&db.0).unwrap();
                assert_eq!(
                    store.command_status(actor.boot_id(), id).unwrap(),
                    Some(Status::Accepted)
                );
                owner.step().await.unwrap().unwrap().result.unwrap();
                assert_eq!(
                    store.command_status(actor.boot_id(), id).unwrap(),
                    Some(Status::Written)
                );
            }
        }
        assert_eq!(
            *bytes.lock().unwrap(),
            if mode == "manual" {
                vec![0xAE, 0x41, 6, 0xF1, 0xEE, 0xAE, 0x41, 6, 0xF2, 0xEE]
            } else {
                vec![]
            }
        );
        if mode == "manual" {
            let revoked = actor
                .submit_panel_led(controller, device.clone(), true)
                .await
                .unwrap()[0];
            permitted.store(false, Ordering::Release);
            assert!(owner.step().await.unwrap().is_none());
            assert_eq!(bytes.lock().unwrap().len(), 10);
            assert_eq!(
                Store::open(&db.0)
                    .unwrap()
                    .command_status(actor.boot_id(), revoked)
                    .unwrap(),
                Some(Status::Cancelled)
            );
            permitted.store(true, Ordering::Release);
        }
        // The light entry cannot bypass the typed LED entry or its mode gate.
        assert!(
            actor
                .submit(Batch {
                    controller,
                    intents: vec![Intent {
                        device,
                        command: Command::PanelLed(true)
                    }],
                    subscriptions: vec![]
                })
                .await
                .is_err()
        );
        actor.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn partial_motor_write_is_uncertain_and_restart_never_replays_motion() {
    use homesense_core::MotorAction as A;
    let db = Database::with_config(&motor_document(true));
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    let boot = actor.boot_id().to_owned();
    let device = DeviceId::new("motor").unwrap();
    let first = actor
        .submit_motor(controller, device.clone(), A::Open)
        .await
        .unwrap()[0];
    let pending = actor
        .submit_motor(controller, device.clone(), A::Close)
        .await
        .unwrap()[0];
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "motor-test".into(),
            hb_transport::Connection::for_simulation(
                OutputStream {
                    bytes: bytes.clone(),
                    fail_after: 2,
                },
                1,
            ),
        )]
        .into(),
    );
    assert_eq!(
        owner
            .step()
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap_err()
            .bytes_written,
        2
    );
    assert_eq!(*bytes.lock().unwrap(), [0xAE, 0x22]);
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(&boot, first).unwrap(),
        Some(Status::Uncertain)
    );
    assert_eq!(
        store.command_status(&boot, pending).unwrap(),
        Some(Status::Cancelled)
    );
    assert!(
        actor
            .submit_motor(controller, device.clone(), A::Stop)
            .await
            .is_err()
    );
    assert!(owner.step().await.is_err());
    actor.shutdown().await.unwrap();
    drop(owner);
    let restarted = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    assert_ne!(restarted.boot_id(), boot);
    assert!(restarted.dispatch().await.unwrap().is_none());
    assert_eq!(
        store.command_status(&boot, first).unwrap(),
        Some(Status::Uncertain)
    );
    assert_eq!(
        store.command_status(&boot, pending).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(bytes.lock().unwrap().len(), 2);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_light_admits_both_outputs_and_blocks_direct_supply_writes() {
    let db = combined_database();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |who| who == id)
        .await
        .unwrap();
    assert_eq!(actor.submit(batch(id)).await.unwrap(), vec![1, 2]);
    let store = Store::open(&db.0).unwrap();
    for command in [1, 2] {
        assert_eq!(
            store.command_status(actor.boot_id(), command).unwrap(),
            Some(Status::Accepted)
        );
    }
    let mut direct = batch(id);
    direct.intents[0].device = DeviceId::new("relay").unwrap();
    direct.intents[0].command = Command::Switch(true);
    assert_eq!(
        actor.submit(direct).await,
        Err(WriteStatus::InvalidValueInRequest)
    );
    actor.transport_failed().await.unwrap();
    for command in [1, 2] {
        assert_eq!(
            store.command_status(actor.boot_id(), command).unwrap(),
            Some(Status::Cancelled)
        );
    }
    actor.shutdown().await.unwrap();
}

fn combined_database() -> Database {
    Database::with_config(br#"{
      "schema_version":1,"revision":1,
      "gateways":[{"id":"simulation","endpoint":"127.0.0.1:50007","modules":[
        {"address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
        {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],
      "devices":[
        {"id":"simulation:light","name":"Light","gateway":"simulation","address":"15","channel":1,"enabled":true,"default_on_level":100,"supply_relay":"relay"},
        {"id":"relay","name":"Relay","gateway":"simulation","address":"03","channel":4,"enabled":true}]}
    "#)
}

#[tokio::test]
async fn combined_wire_order_and_each_partial_write_boundary_are_audited() {
    for fail_after in [2, 7, 100] {
        let db = combined_database();
        let controller = Uuid::new_v4();
        let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
            .await
            .unwrap();
        let ids = actor.submit(batch(controller)).await.unwrap();
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut owner = homesensed::transport_owner::TransportOwner::new(
            actor.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(
                    OutputStream {
                        bytes: bytes.clone(),
                        fail_after,
                    },
                    1,
                ),
            )]
            .into(),
        );
        let first = owner.step().await.unwrap().unwrap();
        if fail_after == 2 {
            assert!(first.result.is_err());
        } else {
            first.result.unwrap();
            let second = owner.step().await.unwrap().unwrap();
            assert_eq!(second.result.is_ok(), fail_after == 100);
        }
        let expected = [0xAE, 0x15, 0xB1, 30, 0xEE, 0xAE, 0x03, 0x04, 0xF1, 0xEE];
        assert_eq!(
            bytes.lock().unwrap().as_slice(),
            &expected[..fail_after.min(10)]
        );
        let store = Store::open(&db.0).unwrap();
        let statuses = match fail_after {
            2 => [Status::Uncertain, Status::Cancelled],
            7 => [Status::Written, Status::Uncertain],
            _ => [Status::Written, Status::Written],
        };
        for (id, status) in ids.into_iter().zip(statuses) {
            assert_eq!(
                store.command_status(actor.boot_id(), id).unwrap(),
                Some(status)
            );
        }
        actor.shutdown().await.unwrap();
        drop(owner);
        let restarted = Admission::open(db.0.clone(), options(), |_| true)
            .await
            .unwrap();
        assert!(restarted.dispatch().await.unwrap().is_none());
        restarted.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn combined_repeated_full_brightness_never_moves_supply_before_dimmer() {
    let db = combined_database();
    let controller = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |id| id == controller)
        .await
        .unwrap();
    for _ in 0..2 {
        let mut request = batch(controller);
        request.intents[0].command = Command::SetLevel {
            level: homesense_core::Percent::new(100).unwrap(),
            transition: homesense_core::LevelTransition::Immediate,
        };
        actor.submit(request).await.unwrap();
    }
    let first = actor.dispatch().await.unwrap().unwrap();
    assert!(
        matches!(first.command.command, Command::SetLevel { .. }),
        "relay overtook dimmer: {:?}",
        first.command.command
    );
    actor.finish_write(first.command.id, true).await.unwrap();
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_memory_accepts_only_current_boot_confirmed_fresh_nonzero_feedback() {
    use homesense_core::{Observation, Percent};
    use homesense_state::Quality;
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut snapshot = homesensed::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    let id = DeviceId::new("simulation:light").unwrap();
    {
        let state = snapshot.devices.get_mut(&id).unwrap();
        state.observed = Some(Observation::Level(Percent::new(27).unwrap()));
        state.quality = Quality::Confirmed;
        state.observed_at_ms = Some(0);
    }
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        27
    );
    for quality in [Quality::Unknown, Quality::Stale] {
        let mut bad = snapshot.clone();
        let state = bad.devices.get_mut(&id).unwrap();
        state.quality = quality;
        state.observed = Some(Observation::Level(Percent::new(90).unwrap()));
        assert_eq!(
            actor.remember_levels(1, bad, 5000).await.unwrap()[&id].get(),
            27
        );
    }
    let mut future = snapshot.clone();
    future.devices.get_mut(&id).unwrap().observed_at_ms = Some(u64::MAX);
    assert_eq!(
        actor.remember_levels(1, future, 5000).await.unwrap()[&id].get(),
        27
    );
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(0).unwrap()));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        27
    );
    snapshot.cursor.boot_id = 0;
    assert!(actor.remember_levels(1, snapshot, 5000).await.is_err());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn combined_restore_ignores_ramp_and_off_supply_until_confirmed_target() {
    use homesense_core::{Observation, Percent};
    use homesense_state::Quality;
    let db = combined_database();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let config = Store::open(&db.0)
        .unwrap()
        .active_configuration(ExecutionContext::Simulation)
        .unwrap()
        .unwrap();
    let mut snapshot = homesensed::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    let id = DeviceId::new("simulation:light").unwrap();
    let relay = DeviceId::new("relay").unwrap();
    for state in snapshot.devices.values_mut() {
        state.quality = Quality::Confirmed;
        state.observed_at_ms = Some(0);
    }
    snapshot.devices.get_mut(&relay).unwrap().observed = Some(Observation::Switch(true));
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(50).unwrap()));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        50
    );
    actor.submit(batch(Uuid::new_v4())).await.unwrap();
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(36).unwrap()));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        50
    );
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(30).unwrap()));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        30
    );
    actor
        .submit(Batch {
            controller: Uuid::new_v4(),
            intents: vec![Intent {
                device: id.clone(),
                command: Command::Switch(false),
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(36).unwrap()));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        30
    );
    snapshot.devices.get_mut(&relay).unwrap().observed = Some(Observation::Switch(false));
    assert_eq!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .unwrap()[&id]
            .get(),
        30
    );
    snapshot.devices.get_mut(&id).unwrap().observed =
        Some(Observation::Level(Percent::new(100).unwrap()));
    assert_eq!(
        actor.remember_levels(1, snapshot, 5000).await.unwrap()[&id].get(),
        30
    );
    actor.shutdown().await.unwrap();
    assert_eq!(
        Store::open(&db.0).unwrap().light_restore_levels(1).unwrap()[&id].get(),
        30
    );
}

#[tokio::test]
async fn delayed_work_keeps_original_generation_across_recovery_and_restart() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let original = actor.task_generation().await.unwrap();
    actor.transport_failed().await.unwrap();
    assert!(actor.task_generation().await.is_err());
    actor.recover_synchronized(2, 1).await.unwrap();
    assert_eq!(
        actor.submit_delayed(original, batch(id)).await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let fresh = actor.task_generation().await.unwrap();
    assert_eq!(
        actor
            .submit_delayed(fresh.clone(), batch(Uuid::new_v4()))
            .await,
        Err(WriteStatus::InsufficientPrivileges)
    );
    assert_eq!(
        actor
            .submit_delayed(fresh.clone(), batch(id))
            .await
            .unwrap(),
        vec![1]
    );
    let old_boot = actor.boot_id().to_owned();
    actor.shutdown().await.unwrap();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    assert_eq!(
        actor.submit_delayed(fresh, batch(id)).await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.command_status(actor.boot_id(), 1).unwrap(), None);
    assert_eq!(
        store.command_status(&old_boot, 1).unwrap(),
        Some(Status::Uncertain)
    );
    let fresh = actor.task_generation().await.unwrap();
    assert_eq!(
        actor.submit_delayed(fresh, batch(id)).await.unwrap(),
        vec![1]
    );
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn delayed_actions_are_bounded_cancelled_and_never_replayed_after_recovery() {
    use homesensed::delayed_actions::DelayedActions;
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let run = DelayedActions::new(actor.clone(), 1).unwrap();
    let pending = run
        .schedule(Duration::from_secs(60), batch(id))
        .await
        .unwrap();
    assert!(matches!(
        run.schedule(Duration::ZERO, batch(id)).await,
        Err(WriteStatus::ResourceBusy)
    ));
    drop(pending);
    let pending = run.schedule(Duration::ZERO, batch(id)).await.unwrap();
    run.cancel();
    assert_eq!(
        pending.wait().await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    assert!(run.schedule(Duration::ZERO, batch(id)).await.is_err());
    let run = DelayedActions::new(actor.clone(), 1).unwrap();
    let pending = run.schedule(Duration::ZERO, batch(id)).await.unwrap();
    actor.transport_failed().await.unwrap();
    actor.recover_synchronized(2, 1).await.unwrap();
    assert_eq!(
        pending.wait().await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let pending = run.schedule(Duration::ZERO, batch(id)).await.unwrap();
    drop(run);
    assert_eq!(
        pending.wait().await,
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.command_status(actor.boot_id(), 1).unwrap(), None);
    let run = DelayedActions::new(actor.clone(), 1).unwrap();
    let started = std::time::Instant::now();
    let pending = run
        .schedule(Duration::from_millis(20), batch(id))
        .await
        .unwrap();
    assert_eq!(pending.wait().await.unwrap(), vec![1]);
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert_eq!(
        store.command_status(actor.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    // Cancellation after admission does not pretend the command was cancelled.
    run.cancel();
    assert_eq!(
        store.command_status(actor.boot_id(), 1).unwrap(),
        Some(Status::Accepted)
    );
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_wakes_an_already_waiting_timer_without_waiting_for_deadline() {
    use homesensed::delayed_actions::DelayedActions;
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let run = DelayedActions::new(actor.clone(), 1).unwrap();
    let pending = run
        .schedule(Duration::from_secs(60), batch(id))
        .await
        .unwrap();
    let mut waiting = Box::pin(pending.wait());
    assert!(hap::futures::poll!(waiting.as_mut()).is_pending());
    run.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap(),
        Err(WriteStatus::ServiceCommunicationFailure)
    );
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn lifecycle_changes_wake_long_delays_without_replay() {
    use homesensed::delayed_actions::DelayedActions;
    for failure in ["transport", "storage", "shutdown"] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
            .await
            .unwrap();
        let run = DelayedActions::new(actor.clone(), 1).unwrap();
        let pending = run
            .schedule(Duration::from_secs(3600), batch(id))
            .await
            .unwrap();
        let mut waiting = Box::pin(pending.wait());
        assert!(hap::futures::poll!(waiting.as_mut()).is_pending());
        match failure {
            "transport" => {
                actor.transport_failed().await.unwrap();
                // Even if watch coalesces fault and recovery, the new epoch differs.
                actor.recover_synchronized(2, 1).await.unwrap();
            }
            "storage" => {
                let sql = rusqlite::Connection::open(&db.0).unwrap();
                sql.execute_batch("CREATE TRIGGER reject_timer_test BEFORE INSERT ON command_journal BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
                assert_eq!(
                    actor.submit(batch(id)).await,
                    Err(WriteStatus::ServiceCommunicationFailure)
                );
            }
            _ => actor.clone().shutdown().await.unwrap(),
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap(),
            Err(WriteStatus::ServiceCommunicationFailure),
            "{failure}"
        );
        let store = Store::open(&db.0).unwrap();
        assert_eq!(store.command_status(actor.boot_id(), 1).unwrap(), None);
        if failure != "shutdown" {
            actor.shutdown().await.unwrap();
        }
    }
}

fn next_simulation_document() -> Vec<u8> {
    let mut doc: hap::serde_json::Value =
        hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    doc["revision"] = 2.into();
    doc["devices"][0]["channel"] = 2.into();
    hap::serde_json::to_vec(&doc).unwrap()
}

#[tokio::test]
async fn publication_cancels_old_work_waits_for_frame_and_installs_mapping_after_sync() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let generation = actor.task_generation().await.unwrap();
    let first = actor.submit(batch(id)).await.unwrap()[0];
    let ticket = actor.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, first);
    let second = actor.submit(batch(id)).await.unwrap()[0];
    actor
        .begin_publication(next_simulation_document(), 1, Duration::from_secs(3))
        .await
        .unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(
        store.command_status(actor.boot_id(), second).unwrap(),
        Some(Status::Cancelled)
    );
    assert_eq!(
        actor.commit_publication().await,
        Err(WriteStatus::ResourceBusy)
    );
    assert!(actor.submit(batch(id)).await.is_err());
    actor.finish_write(first, true).await.unwrap();
    assert_eq!(actor.commit_publication().await.unwrap(), 2);
    assert_eq!(store.active().unwrap().unwrap().revision, 2);
    assert_eq!(store.cancellation_count(2).unwrap(), 1);
    assert!(actor.dispatch().await.unwrap().is_none());
    assert!(actor.submit(batch(id)).await.is_err());
    assert!(actor.synchronized(1, 1).await.is_err());
    assert!(actor.synchronized(2, 2).await.is_err());
    actor.synchronized(1, 2).await.unwrap();
    assert!(actor.submit_delayed(generation, batch(id)).await.is_err());
    actor.submit(batch(id)).await.unwrap();
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "simulation".into(),
            hb_transport::Connection::for_simulation(
                OutputStream {
                    bytes: bytes.clone(),
                    fail_after: 100,
                },
                1,
            ),
        )]
        .into(),
    );
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(bytes.lock().unwrap()[2] & 0x0f, 2);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_drains_stop_against_original_motor_mapping() {
    let doc = motor_document(true);
    let db = Database::with_config(&doc);
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    actor
        .submit_motor(
            id,
            DeviceId::new("motor").unwrap(),
            homesense_core::MotorAction::Stop,
        )
        .await
        .unwrap();
    let mut next: hap::serde_json::Value = hap::serde_json::from_slice(&doc).unwrap();
    next["revision"] = 2.into();
    next["devices"][0]["channel"] = 3.into();
    actor
        .begin_publication(
            hap::serde_json::to_vec(&next).unwrap(),
            1,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(
        actor.commit_publication().await,
        Err(WriteStatus::ResourceBusy)
    );
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut owner = homesensed::transport_owner::TransportOwner::new(
        actor.clone(),
        [(
            "motor-test".into(),
            hb_transport::Connection::for_simulation(
                OutputStream {
                    bytes: bytes.clone(),
                    fail_after: 100,
                },
                1,
            ),
        )]
        .into(),
    );
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(*bytes.lock().unwrap(), [0xae, 0x22, 2, 0xf0, 0xee]);
    assert_eq!(actor.commit_publication().await.unwrap(), 2);
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_publication_keeps_cancelled_work_dead_after_recovery() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let generation = actor.task_generation().await.unwrap();
    let command = actor.submit(batch(id)).await.unwrap()[0];
    actor
        .begin_publication(next_simulation_document(), 1, Duration::from_secs(3))
        .await
        .unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_publication BEFORE INSERT ON configuration_revision BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(actor.commit_publication().await.is_err());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    assert_eq!(
        store.command_status(actor.boot_id(), command).unwrap(),
        Some(Status::Cancelled)
    );
    assert!(actor.submit(batch(id)).await.is_err());
    sql.execute_batch("DROP TRIGGER reject_publication;")
        .unwrap();
    actor.recover_synchronized(2, 1).await.unwrap();
    assert!(actor.submit_delayed(generation, batch(id)).await.is_err());
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_wakes_timers_and_abort_does_not_restore_them() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let actor = Admission::open(db.0.clone(), options(), move |v| v == id)
        .await
        .unwrap();
    let run = homesensed::delayed_actions::DelayedActions::new(actor.clone(), 1).unwrap();
    let waiting = run
        .schedule(Duration::from_secs(3600), batch(id))
        .await
        .unwrap();
    let queued = actor.submit(batch(id)).await.unwrap()[0];
    actor
        .begin_publication(next_simulation_document(), 1, Duration::from_millis(20))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting.wait())
            .await
            .unwrap()
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(actor.commit_publication().await.is_err());
    actor.abort_publication().await.unwrap();
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    assert_eq!(
        store.command_status(actor.boot_id(), queued).unwrap(),
        Some(Status::Cancelled)
    );
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.submit(batch(id)).await.unwrap();
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn old_mapping_feedback_cannot_seed_new_revision_restore_memory() {
    let db = Database::new();
    let actor = Admission::open(db.0.clone(), options(), |_| true)
        .await
        .unwrap();
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let mut snapshot = homesensed::feedback::Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap()
    .snapshot();
    let id = DeviceId::new("simulation:light").unwrap();
    let state = snapshot.devices.get_mut(&id).unwrap();
    state.observed = Some(homesense_core::Observation::Level(
        homesense_core::Percent::new(27).unwrap(),
    ));
    state.quality = homesense_state::Quality::Confirmed;
    state.observed_at_ms = Some(0);
    actor
        .remember_levels(1, snapshot.clone(), 5000)
        .await
        .unwrap();
    actor
        .begin_publication(next_simulation_document(), 1, Duration::from_secs(3))
        .await
        .unwrap();
    assert!(
        actor
            .remember_levels(1, snapshot.clone(), 5000)
            .await
            .is_err()
    );
    actor.commit_publication().await.unwrap();
    actor.synchronized(1, 2).await.unwrap();
    assert!(actor.remember_levels(1, snapshot, 5000).await.is_err());
    let store = Store::open(&db.0).unwrap();
    assert_eq!(store.light_restore_levels(1).unwrap()[&id].get(), 27);
    assert!(store.light_restore_levels(2).unwrap().is_empty());
    actor.shutdown().await.unwrap();
}

#[tokio::test]
async fn automation_publication_validates_before_barrier_and_commits_one_revision() {
    for fail in [false,true] {
        let db=Database::new();
        let id=Uuid::new_v4();
        let actor=Admission::open(db.0.clone(),options(),move|v|v==id).await.unwrap();
        let generation=actor.task_generation().await.unwrap();
        let command=actor.submit(batch(id)).await.unwrap()[0];
        let rules=br#"{"schema_version":1,"configuration_revision":2,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[]}"#.to_vec();
        assert!(actor.begin_automation_publication(next_simulation_document(),b"{}".to_vec(),1,Duration::from_secs(3)).await.is_err());
        assert_eq!(actor.task_generation().await.unwrap(),generation);
        let store=Store::open(&db.0).unwrap();
        assert_eq!(store.command_status(actor.boot_id(),command).unwrap(),Some(Status::Accepted));
        actor.begin_automation_publication(next_simulation_document(),rules.clone(),1,Duration::from_secs(3)).await.unwrap();
        assert_eq!(store.command_status(actor.boot_id(),command).unwrap(),Some(Status::Cancelled));
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        if fail {
            sql.execute_batch("CREATE TRIGGER reject_bundle BEFORE INSERT ON automation_revision BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
            assert!(actor.commit_publication().await.is_err());
            assert_eq!(store.active().unwrap().unwrap().revision,1);
            assert!(store.revision(2).unwrap().is_none());
            assert!(store.automation_payload(2).unwrap().is_none());
            sql.execute_batch("DROP TRIGGER reject_bundle;").unwrap();
            actor.recover_synchronized(2,1).await.unwrap();
        } else {
            assert_eq!(actor.commit_publication().await.unwrap(),2);
            let bundle=store.active_bundle(ExecutionContext::Simulation).unwrap().unwrap();
            assert_eq!(bundle.configuration.revision(),2);
            assert_eq!(bundle.automation.unwrap().revision,2);
            assert_eq!(store.automation_payload(2).unwrap(),Some(rules));
            assert!(store.logical_variable(2,"mode").unwrap().is_some());
            actor.synchronized(1,2).await.unwrap();
            let current=actor.task_generation().await.unwrap();
            let mut candidate:hap::serde_json::Value=hap::serde_json::from_slice(&next_simulation_document()).unwrap();
            candidate["revision"]=3.into();
            assert!(actor.begin_publication(hap::serde_json::to_vec(&candidate).unwrap(),2,Duration::from_secs(3)).await.is_err());
            assert_eq!(actor.task_generation().await.unwrap(),current);
            let incompatible=br#"{"schema_version":1,"configuration_revision":3,"variables":[{"id":"mode","kind":"bool","lifetime":"persistent"}],"rules":[]}"#.to_vec();
            assert!(actor.begin_automation_publication(hap::serde_json::to_vec(&candidate).unwrap(),incompatible,2,Duration::from_secs(3)).await.is_err());
            assert_eq!(actor.task_generation().await.unwrap(),current);

        }
        assert!(actor.submit_delayed(generation,batch(id)).await.is_err());
        assert!(actor.dispatch().await.unwrap().is_none());
        assert_eq!(store.command_status(actor.boot_id(),command).unwrap(),Some(Status::Cancelled));
        actor.shutdown().await.unwrap();
    }
}
