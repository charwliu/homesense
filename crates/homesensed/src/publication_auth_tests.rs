//! Real API -> draft -> Publisher queue -> Admission barrier, synthetic SQLite.
//! The extra root credential deliberately shares the source's controller UUID.
use super::*;
use crate::{
    admission::{Admission, Options},
    web_api::{Api, Runtime},
    web_auth::{
        CredentialRequest, CredentialUpdate, Grant, IssuedCredential, Role, SessionSecret, Tokens,
    },
    web_drafts::{Draft, Drafts},
    web_receipts::Receipts,
};
use homesense_core::ExecutionContext;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Bytes};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};
use uuid::Uuid;
const ROOT: &str = "synthetic-publication-root-01234567890123456789";
struct Fixture {
    directory: PathBuf,
    actor: Admission,
    controller: Uuid,
    api: Arc<Api>,
    publisher: Publisher,
    receiver: Receiver,
    tokens: Arc<Tokens>,
    source: IssuedCredential,
    session: Option<SessionSecret>,
    drafts: [Uuid; 2],
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn documents(revision: u64) -> (Value, Value) {
    (
        json!({"schema_version":2,"revision":revision,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"curtain","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"relay","channel":1,"enabled":true},{"id":"motor","name":"Synthetic motor","module_id":"curtain","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"Synthetic","verified_at_unix_ms":1}}]}),
        json!({"schema_version":1,"configuration_revision":revision,"variables":[],"rules":[]}),
    )
}
impl Fixture {
    async fn new(cookie: bool) -> Self {
        let directory = std::env::temp_dir().join(format!("publication-source-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let controller = Uuid::new_v4();
        let (devices, rules) = documents(1);
        homesense_storage::Store::open(directory.join("runtime.sqlite"))
            .unwrap()
            .publish_automation(
                None,
                &serde_json::to_vec(&devices).unwrap(),
                &serde_json::to_vec(&rules).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let actor = Admission::open(
            directory.join("runtime.sqlite"),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let (control, _control_rx) =
            crate::automation_control::channel(controller, actor.task_generation().await.unwrap());
        let tokens = Arc::new(Tokens::default());
        let devices: BTreeSet<String> = ["light".into(), "motor".into()].into();
        tokens
            .insert(
                Uuid::new_v4(),
                ROOT,
                Grant {
                    controller,
                    role: Role::Administrator,
                    devices: devices.clone(),
                    scenes: BTreeSet::new(),
                },
            )
            .unwrap();
        let source = tokens
            .issue(
                CredentialRequest {
                    role: Role::Administrator,
                    devices,
                    scenes: BTreeSet::new(),
                },
                Some(&format!("Bearer {ROOT}")),
                None,
                None,
            )
            .unwrap();
        assert_eq!(source.grant.controller, controller);
        let session = cookie.then(|| tokens.login(&format!("Bearer {}", source.secret)).unwrap());
        let mut store = Drafts::open(&directory.join("drafts.sqlite")).unwrap();
        let drafts = [Uuid::new_v4(), Uuid::new_v4()];
        for (id, revision) in drafts.into_iter().zip([2, 3]) {
            let (devices, automation) = documents(revision);
            store
                .save(
                    controller,
                    &Draft {
                        id,
                        expected_revision: 1,
                        revision,
                        devices,
                        automation,
                    },
                    ExecutionContext::Simulation,
                )
                .unwrap();
        }
        let (publisher, receiver) = channel();
        let api = Arc::new(Api {
            daily_backup: Default::default(),
            hap_bridges: Default::default(),
            hap_status: Default::default(),
            backup_source: None,
            publisher: Some(publisher.clone()),
            origin: "http://127.0.0.1:8088".into(),
            tokens: tokens.clone(),
            runtime: RwLock::new(Some(Runtime {
                control,
                revision: 1,
                boot: actor.boot_id().into(),
                scenes: Default::default(),
            })),
            drafts: Some((ExecutionContext::Simulation, Mutex::new(store))),
            receipts: Mutex::new(Receipts::open(&directory.join("receipts.sqlite")).unwrap()),
        });
        Self {
            directory,
            actor,
            controller,
            api,
            publisher,
            receiver,
            tokens,
            source,
            session,
            drafts,
        }
    }
    fn call(&self, root: bool) -> tokio::task::JoinHandle<Response<Full<Bytes>>> {
        let id = self.drafts[usize::from(root)];
        let body = json!({"request_id":Uuid::new_v4().to_string(),"boot":self.actor.boot_id()});
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/config/activations/{id}"))
            .header("host", "127.0.0.1:8088")
            .header("origin", &self.api.origin)
            .header("content-type", "application/json");
        if root {
            request = request.header("authorization", format!("Bearer {ROOT}"));
        } else if let Some(session) = &self.session {
            request = request
                .header("cookie", format!("homesense_session={}", session.cookie))
                .header("x-csrf-token", &session.csrf);
        } else {
            request = request.header("authorization", format!("Bearer {}", self.source.secret));
        }
        let request = request.body(Bytes::from(body.to_string())).unwrap();
        let api = self.api.clone();
        tokio::spawn(async move { api.handle(request).await })
    }
    async fn queued(&self, count: usize) {
        let end = tokio::time::Instant::now() + Duration::from_secs(3);
        while self.publisher.sender.capacity() != 2 - count {
            assert!(
                tokio::time::Instant::now() < end,
                "actual HTTP publication did not reach queue"
            );
            tokio::task::yield_now().await;
        }
    }
    fn change(&self, kind: &str) {
        let root = format!("Bearer {ROOT}");
        match kind {
            "revoke" => self
                .tokens
                .revoke_managed(self.source.id, Some(&root), None, None)
                .unwrap(),
            "logout" => self.tokens.logout(&self.session.as_ref().unwrap().cookie),
            _ => {
                self.tokens
                    .update_grant(
                        self.source.id,
                        CredentialUpdate {
                            expected: self.source.grant.clone(),
                            role: if kind == "role" {
                                Role::Member
                            } else {
                                Role::Administrator
                            },
                            devices: if kind == "scope" {
                                ["light".into()].into()
                            } else {
                                self.source.grant.devices.clone()
                            },
                            scenes: BTreeSet::new(),
                        },
                        Some(&root),
                        None,
                        None,
                    )
                    .unwrap();
            }
        }
        assert!(
            self.tokens.authenticate(&root).is_some(),
            "independent root sharing controller must stay valid"
        );
    }
    fn source_stop_admission(&self) -> Admission {
        let lease = if let Some(session) = &self.session {
            self.tokens
                .module_off_lease(None, Some(&session.cookie), Some(&session.csrf))
        } else {
            self.tokens.module_off_lease(
                Some(&format!("Bearer {}", self.source.secret)),
                None,
                None,
            )
        }
        .unwrap();
        let action = crate::automation_control::Action::DeviceCommand {
            controller: self.controller,
            revision: 1,
            device: homesense_core::DeviceId::new("motor").unwrap(),
            action: crate::management_devices::DeviceAction::Stop,
        };
        let source = crate::source_auth::Source::capture(lease, &action, None).unwrap();
        self.actor.with_source(Some(source))
    }
    fn active(&self) -> u64 {
        homesense_storage::Store::open(self.directory.join("runtime.sqlite"))
            .unwrap()
            .active()
            .unwrap()
            .unwrap()
            .revision
    }
    async fn publishing(&self) -> bool {
        self.actor.publication_in_progress().await.unwrap()
    }
    async fn root_commit(&mut self, task: tokio::task::JoinHandle<Response<Full<Bytes>>>) -> Value {
        let pending = self.receiver.receive().await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(pending.devices()).unwrap()["revision"],
            3
        );
        assert!(!pending.abandoned());
        pending
            .begin(&self.actor, Duration::from_secs(3))
            .await
            .unwrap();
        assert!(self.publishing().await);
        let revision = self.actor.commit_publication().await.unwrap();
        assert_eq!(revision, 3);
        pending.complete(Ok(revision));
        let result = body(task).await;
        assert_eq!(result["outcome"], "activated");
        assert_eq!(result["revision"], 3);
        assert_eq!(self.active(), 3);
        result
    }
}
async fn body(task: tokio::task::JoinHandle<Response<Full<Bytes>>>) -> Value {
    let response = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!([200, 202].contains(&response.status().as_u16()));
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
fn evidence(name: &str, records: &[Value]) {
    if let Ok(root) = std::env::var("PUBLICATION_AUTH_ARTIFACT_ROOT") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            PathBuf::from(root).join(name),
            serde_json::to_vec_pretty(records).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn publication_http_source_changes_before_barrier_must_not_start_or_commit() {
    let mut records = vec![];
    for after_receive in [false, true] {
        for change in ["role", "scope", "revoke", "logout"] {
            let mut h = Fixture::new(change == "logout").await;
            let source = h.call(false);
            h.queued(1).await;
            let pending = if after_receive {
                Some(h.receiver.receive().await.unwrap())
            } else {
                None
            };
            h.change(change);
            let root = h.call(true);
            h.queued(if after_receive { 1 } else { 2 }).await;
            let pending = match pending {
                Some(p) => p,
                None => h.receiver.receive().await.unwrap(),
            };
            let revision = serde_json::from_slice::<Value>(pending.devices()).unwrap()["revision"]
                .as_u64()
                .unwrap();
            let mut entered = false;
            if revision == 2 {
                if !pending.abandoned() {
                    entered = pending
                        .begin(&h.actor, Duration::from_secs(3))
                        .await
                        .is_ok();
                }
                if entered {
                    assert!(h.publishing().await);
                    h.actor.abort_publication().await.unwrap();
                }
                pending.complete(Err("invalid source must not publish".into()));
                assert_eq!(h.active(), 1);
                h.root_commit(root).await;
            } else {
                assert_eq!(
                    revision, 3,
                    "only the reviewed source and root requests are queued"
                );
                pending
                    .begin(&h.actor, Duration::from_secs(3))
                    .await
                    .unwrap();
                let committed = h.actor.commit_publication().await.unwrap();
                assert_eq!(committed, 3);
                pending.complete(Ok(committed));
                let root_body = body(root).await;
                assert_eq!(root_body["revision"], 3);
                assert_eq!(root_body["outcome"], "activated");
            }
            let source_body = body(source).await;
            assert_ne!(source_body["outcome"], "activated");
            assert_eq!(h.active(), 3);
            assert!(!h.publishing().await);
            records.push(json!({"after_receive":after_receive,"change":change,"invalid_barrier_started":entered,"fresh_root_own_revision_committed":3}));
            h.actor.clone().shutdown().await.unwrap();
        }
    }
    evidence("publication-source-before-barrier.json", &records);
    assert!(
        records
            .iter()
            .all(|r| r["invalid_barrier_started"] == false),
        "revoked source entered publication barrier: {records:?}"
    );
}
#[tokio::test]
async fn source_changes_after_the_last_queue_check_are_rechecked_by_the_barrier_actor() {
    let mut records = vec![];
    for change in ["role", "scope", "revoke", "logout"] {
        let mut h = Fixture::new(change == "logout").await;
        let source = h.call(false);
        h.queued(1).await;
        let pending = h.receiver.receive().await.unwrap();
        assert!(!pending.abandoned());
        h.change(change);
        // Deliberately do not consult abandoned() again: exercise the actual
        // actor's boundary, not only a coordinator-side queue precheck.
        let result = pending.begin(&h.actor, Duration::from_secs(3)).await;
        let entered = h.publishing().await;
        if entered {
            h.actor.abort_publication().await.unwrap();
        }
        pending.complete(Err("source revoked at actor boundary".into()));
        let source_body = body(source).await;
        assert_ne!(source_body["outcome"], "activated");
        assert_eq!(h.active(), 1);
        let root = h.call(true);
        h.queued(1).await;
        h.root_commit(root).await;
        records.push(json!({"change":change,"actor_rejected":result.is_err(),"invalid_barrier_started":entered,"fresh_root_own_revision_committed":3}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("publication-source-at-actor.json", &records);
    assert!(
        records
            .iter()
            .all(|r| r["actor_rejected"] == true && r["invalid_barrier_started"] == false),
        "actor accepted revoked publication source: {records:?}"
    );
}
struct Wire(Arc<Mutex<Vec<u8>>>);
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn revocation_after_barrier_entry_keeps_stop_drain_and_atomic_switch_complete() {
    let mut records = vec![];
    for source_bound in [false, true] {
        for after_ticket in [false, true] {
            for change in ["role", "scope", "revoke", "logout"] {
                let mut h = Fixture::new(change == "logout").await;
                let bytes = Arc::new(Mutex::new(vec![]));
                let mut owner = crate::transport_owner::TransportOwner::new(
                    h.actor.clone(),
                    [(
                        "bus".into(),
                        hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
                    )]
                    .into(),
                );
                h.actor
                    .submit_motor(
                        h.controller,
                        homesense_core::DeviceId::new("motor").unwrap(),
                        homesense_core::MotorAction::Open,
                    )
                    .await
                    .unwrap();
                assert!(owner.step().await.unwrap().unwrap().result.is_ok());
                assert_eq!(*bytes.lock().unwrap(), [0xae, 0x22, 1, 0xf1, 0xee]);
                bytes.lock().unwrap().clear();
                // Existing STOP only; publication never synthesizes an unrequested motor action.
                let submitter = if source_bound {
                    h.source_stop_admission()
                } else {
                    h.actor.clone()
                };
                let stop_ids = submitter
                    .submit_motor(
                        h.controller,
                        homesense_core::DeviceId::new("motor").unwrap(),
                        homesense_core::MotorAction::Stop,
                    )
                    .await
                    .unwrap();
                assert_eq!(stop_ids.len(), 1);
                let source = h.call(false);
                h.queued(1).await;
                let pending = h.receiver.receive().await.unwrap();
                pending
                    .begin(&h.actor, Duration::from_secs(3))
                    .await
                    .unwrap();
                assert!(h.publishing().await);
                assert!(
                    h.actor.commit_publication().await.is_err(),
                    "cannot commit before the existing queued STOP is drained"
                );
                let stopped: Result<crate::transport_owner::WriteOutcome, String> =
                    if after_ticket {
                        let ticket =
                            h.actor.dispatch().await.unwrap().expect(
                                "valid reserved STOP must obtain a ticket before revocation",
                            );
                        assert_eq!(ticket.command.id, stop_ids[0]);
                        assert!(ticket.command.is_stop());
                        h.change(change);
                        let mut connection =
                            hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1);
                        let result =
                            crate::transport_owner::send_ticket(&ticket, &mut connection, || {
                                h.actor.monotonic_ms()
                            });
                        h.actor
                            .finish_write(ticket.command.id, result.is_ok())
                            .await
                            .unwrap();
                        Ok(crate::transport_owner::WriteOutcome {
                            projection_cancelled: false,
                            managed_away_outputs: None,
                            refresh_schedule:vec![],
                            physical_frames:ticket.frame.physical_frames(),
                            refresh_devices:ticket.refresh_devices.clone(),
                            completed:if result.is_ok(){vec![(ticket.command.id,ticket.command.device_id.clone())]}else{vec![]},
                            device_id: ticket.command.device_id,
                            command_id: ticket.command.id,
                            result,
                        })
                    } else {
                        h.change(change);
                        owner
                            .step()
                            .await
                            .and_then(|v| v.ok_or_else(|| "reserved STOP disappeared".into()))
                    };
                let failure = match &stopped {
                    Err(error) => Some(error.clone()),
                    Ok(outcome) if outcome.result.is_err() => Some(format!("{:?}", outcome.result)),
                    _ => None,
                };
                if let Some(failure) = failure {
                    assert_eq!(
                        h.active(),
                        1,
                        "failed drain must not commit a partial configuration"
                    );
                    pending.complete(Err("reserved STOP did not complete".into()));
                    let response = body(source).await;
                    assert_ne!(response["outcome"], "activated");
                    records.push(json!({"source_bound":source_bound,"after_ticket":after_ticket,"change":change,"completed":false,"failure":failure,"wire":bytes.lock().unwrap().clone(),"active_revision":1}));
                    h.actor.clone().shutdown().await.unwrap();
                    continue;
                }
                let stopped = stopped.unwrap();
                assert_eq!(stopped.command_id, stop_ids[0]);
                assert_eq!(*bytes.lock().unwrap(), [0xae, 0x22, 1, 0xf0, 0xee]);
                assert!(owner.step().await.unwrap().is_none());
                let revision = h.actor.commit_publication().await.unwrap();
                assert_eq!(revision, 2);
                let (devices, _) = documents(2);
                let configuration = homesense_config::Configuration::parse(
                    &serde_json::to_vec(&devices).unwrap(),
                    ExecutionContext::Simulation,
                )
                .unwrap();
                let transport = crate::transport_owner::TransportOwner::new(
                    h.actor.clone(),
                    crate::service::simulation_connections_at(&configuration, 2).unwrap(),
                );
                let (_runtime, _owner) = crate::publication_recovery::rebuild_with_policy(
                    h.actor.clone(),
                    h.controller,
                    transport,
                    2,
                    crate::publication_recovery::Settings {
                        query_timeout_ms: 1000,
                        max_age_ms: 5000,
                        wall_unix_ms: 1_700_000_000_000,
                        calendar_lateness_ms: 1000,
                        scene_capacity: 16,
                    },
                    crate::observed_transport::Synchronization::OutputsAndCalibratedMotors,
                )
                .await
                .unwrap();
                let generation = h.actor.task_generation().await.unwrap();
                assert!(
                    h.actor
                        .diagnostics(h.controller, generation)
                        .await
                        .unwrap()
                        .ready()
                );
                pending.complete(Ok(revision));
                let response = body(source).await;
                assert_eq!(response["outcome"], "activated");
                assert_eq!(response["revision"], 2);
                assert_eq!(h.active(), 2);
                assert!(!h.publishing().await);
                let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
                let unfinished:i64=sql.query_row("SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",[],|r|r.get(0)).unwrap();
                assert_eq!(unfinished, 0);
                drop(sql);
                records.push(json!({"source_bound":source_bound,"after_ticket":after_ticket,"change":change,"completed":true,"stop_written_id":stopped.command_id,"committed_revision":2,"half_switch":false}));
                h.actor.clone().shutdown().await.unwrap();
            }
        }
    }
    evidence("publication-source-after-barrier.json", &records);
    assert!(
        records.iter().all(|r| r["completed"] == true),
        "accepted publication lost reserved STOP drain after source change: {records:?}"
    );
}
#[tokio::test]
async fn revoked_source_before_publication_cannot_adopt_its_queued_stop_without_an_accepted_barrier()
 {
    let mut records = vec![];
    for change in ["role", "scope", "revoke", "logout"] {
        let mut h = Fixture::new(change == "logout").await;
        let bytes = Arc::new(Mutex::new(vec![]));
        let mut owner = crate::transport_owner::TransportOwner::new(
            h.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
            )]
            .into(),
        );
        let ids = h
            .source_stop_admission()
            .submit_motor(
                h.controller,
                homesense_core::DeviceId::new("motor").unwrap(),
                homesense_core::MotorAction::Stop,
            )
            .await
            .unwrap();
        let source = h.call(false);
        h.queued(1).await;
        let pending = h.receiver.receive().await.unwrap();
        h.change(change);
        assert!(
            pending
                .begin(&h.actor, Duration::from_secs(3))
                .await
                .is_err()
        );
        assert!(!h.publishing().await);
        assert_eq!(h.active(), 1);
        pending.complete(Err("source revoked before acceptance".into()));
        let response = body(source).await;
        assert_ne!(response["outcome"], "activated");
        assert!(owner.step().await.unwrap().is_none());
        assert!(bytes.lock().unwrap().is_empty());
        let status = homesense_storage::Store::open(h.directory.join("runtime.sqlite"))
            .unwrap()
            .command_status(h.actor.boot_id(), ids[0])
            .unwrap();
        assert_eq!(status, Some(homesense_storage::journal::Status::Cancelled));
        let root = h.call(true);
        h.queued(1).await;
        h.root_commit(root).await;
        records.push(json!({"change":change,"invalid_stop_bytes":0,"no_barrier_adoption":true,"fresh_root_own_revision":3}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("publication-source-before-barrier-stop.json", &records);
}
