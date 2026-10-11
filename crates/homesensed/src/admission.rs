//! Dedicated bounded admission owner. No transport is dispatched by this actor.
use hap::server::WriteStatus;
use homesense_core::{
    ExecutionContext,
    admission::{Access, RuntimePolicy},
    arbiter::{Arbiter, ArbiterError, BatchCommitError},
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_hap::requests::Batch;
use homesense_storage::{Store, journal::Status};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;
#[derive(Clone, Copy)]
pub struct Options {
    pub context: ExecutionContext,
    pub mailbox_capacity: usize,
    pub command_capacity: usize,
    pub request_ttl: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            context: ExecutionContext::Live,
            mailbox_capacity: 32,
            command_capacity: 128,
            request_ttl: Duration::from_secs(2),
        }
    }
}
/// Issued only after durable Writing. The owner must report the write result;
/// dropping a ticket is never permission to retry it.
pub struct DispatchTicket {
    pub gateway_id: String,
    pub command: QueuedCommand,
    pub frame: hb_transport::PreparedFrame,
}
/// Captured before a delayed task starts. Private fields prevent callers from
/// relabeling old work as belonging to the current runtime generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskGeneration {
    boot: String,
    pub(crate) epochs: Epochs,
}
enum SubmissionScope {
    Generation(TaskGeneration),
    Configuration(u64),
}
enum Submission {
    Automation {
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        manual_fence: Option<u64>,
        feedback: Option<Box<crate::feedback::RuleLease>>,
        audit: Option<homesense_storage::rule_audit::Admission>,
        scene: Option<homesense_storage::scene_run::StepLink>,
    },
    Study {
        batch: Batch,
        expected_mode: Option<u8>,
        next_mode: u8,
    },
    Lights(Batch),
    PanelLed {
        controller: Uuid,
        device: homesense_core::DeviceId,
        active: bool,
    },
    Motor {
        controller: Uuid,
        device: homesense_core::DeviceId,
        action: homesense_core::MotorAction,
    },
}
type RestoreLevels = BTreeMap<homesense_core::DeviceId, homesense_core::Percent>;
struct PendingPublication {
    document: Vec<u8>,
    cancellations: Vec<homesense_core::queue::Cancellation>,
}
pub(crate) enum SceneUpdate {
    Begin { run: String, definition: Option<String> },
    Checkpoint { run: String, expected: u64, status: homesense_storage::scene_run::Status },
}
enum Message {
    FeedbackSettled(Uuid, TaskGeneration, crate::feedback::RuleLease, oneshot::Sender<Result<bool, WriteStatus>>),

    SceneQuery(Uuid, TaskGeneration, homesense_storage::scene_run::Query, oneshot::Sender<Result<Option<homesense_storage::scene_run::Page>, WriteStatus>>),

    SceneUpdate(Uuid, TaskGeneration, SceneUpdate, oneshot::Sender<Result<u64, WriteStatus>>),

    RuleOutcome(
        Uuid,
        TaskGeneration,
        u128,
        Option<String>,
        oneshot::Sender<Result<(), WriteStatus>>,
    ),
    RuleChain(
        Uuid,
        TaskGeneration,
        u128,
        u64,
        usize,
        oneshot::Sender<Result<homesense_storage::rule_audit::Page, WriteStatus>>,
    ),
    InitializeVariables(
        Uuid,
        TaskGeneration,
        BTreeMap<String, homesense_core::rule::Variable>,
        oneshot::Sender<Result<(), WriteStatus>>,
    ),
    Temporary(
        Uuid,
        TaskGeneration,
        String,
        homesense_core::rule::ValueType,
        oneshot::Sender<Result<(), WriteStatus>>,
    ),
    Calendar(
        Uuid,
        TaskGeneration,
        Option<homesense_storage::calendar::Checkpoint>,
        oneshot::Sender<Result<Option<i64>, WriteStatus>>,
    ),
    ManualFence(oneshot::Sender<u64>),
    CommandStatuses(
        Uuid,
        TaskGeneration,
        Vec<u64>,
        oneshot::Sender<Result<Vec<Status>, WriteStatus>>,
    ),
    VariableSnapshot(
        Uuid,
        TaskGeneration,
        Vec<String>,
        oneshot::Sender<
            Result<BTreeMap<String, homesense_storage::variables::Stored>, WriteStatus>,
        >,
    ),
    Variable(
        Uuid,
        TaskGeneration,
        String,
        Option<homesense_core::rule::ValueType>,
        oneshot::Sender<Result<Option<homesense_storage::variables::Stored>, WriteStatus>>,
    ),
    StudyMode(oneshot::Sender<Result<Option<u8>, WriteStatus>>),
    CancelRun(
        Uuid,
        TaskGeneration,
        u64,
        oneshot::Sender<Result<usize, WriteStatus>>,
    ),
    BeginPublication(Vec<u8>, u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
    CommitPublication(oneshot::Sender<Result<u64, WriteStatus>>),
    AbortPublication(oneshot::Sender<Result<(), WriteStatus>>),
    Wake,
    Generation(oneshot::Sender<Result<TaskGeneration, WriteStatus>>),
    Remember(
        u64,
        homesense_state::Snapshot,
        u64,
        oneshot::Sender<Result<RestoreLevels, WriteStatus>>,
    ),
    TransportFailed(oneshot::Sender<Result<(), WriteStatus>>),
    Dispatch(oneshot::Sender<Result<Option<DispatchTicket>, WriteStatus>>),
    Finished(u64, bool, oneshot::Sender<Result<(), WriteStatus>>),
    Shutdown(oneshot::Sender<()>),
    Submit(
        Box<Submission>,
        Option<SubmissionScope>,
        u64,
        oneshot::Sender<Result<Vec<u64>, WriteStatus>>,
    ),
    Ready(u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
    Recover(u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
}
#[derive(Clone)]
pub struct Admission {
    sender: mpsc::SyncSender<Message>,
    boot: String,
    clock: Instant,
    ttl: Duration,
    stop_requested: Arc<AtomicBool>,
    generations: tokio::sync::watch::Receiver<Option<TaskGeneration>>,
}
fn millis(clock: Instant) -> u64 {
    u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX)
}
impl Admission {
    /// Authorization is supplied by the trusted session owner and rechecked on
    /// the worker. Live transport stays unavailable until explicit synchronization.
    pub async fn open(
        path: PathBuf,
        options: Options,
        authorize: impl Fn(Uuid) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        if !(1..=128).contains(&options.mailbox_capacity)
            || !(1..=4096).contains(&options.command_capacity)
            || options.request_ttl.as_millis() == 0
            || options.request_ttl > Duration::from_secs(30)
        {
            return Err("invalid admission bounds".into());
        }
        let (sender, receiver) = mpsc::sync_channel(options.mailbox_capacity);
        let (generation_updates, generations) = tokio::sync::watch::channel(None);
        let (started, start) = oneshot::channel();
        let boot = Uuid::new_v4().to_string();
        let worker_boot = boot.clone();
        let clock = Instant::now();
        let authorize = Arc::new(authorize);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_requested.clone();
        std::thread::Builder::new()
            .name("homesense-admission".into())
            .spawn(move || {
                let initialize = || -> Result<_, String> {
                    let path = path.canonicalize().map_err(|e| e.to_string())?;
                    let lease = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(path.with_extension("runtime.lock"))
                        .map_err(|e| e.to_string())?;
                    lease
                        .try_lock()
                        .map_err(|e| format!("runtime already owned or lock unavailable: {e}"))?;
                    let mut store = Store::open(&path).map_err(|e| e.to_string())?;
                    let configuration = store
                        .active_configuration(options.context)
                        .map_err(|e| e.to_string())?
                        .ok_or("active configuration required")?;
                    store
                        .recover_commands(&worker_boot)
                        .map_err(|e| e.to_string())?;
                    store.recover_scene_runs(&worker_boot).map_err(|e| e.to_string())?;
                    let epochs = Epochs {
                        transport: 1,
                        configuration: configuration.revision(),
                        control: 1,
                    };
                    let arbiter = Arbiter::new(
                        RuntimePolicy {
                            epochs,
                            publishing: false,
                            controller: true,
                            gateway_ready: options.context == ExecutionContext::Simulation,
                            storage_healthy: true,
                            manual_degraded_enabled: false,
                            valid_configuration_loaded: true,
                        },
                        options.command_capacity,
                        2,
                    )
                    .map_err(|e| e.to_string())?;
                    let durable_configuration = store.active().map_err(|e| e.to_string())?.ok_or("active configuration required")?;
                    Ok((lease, store, configuration, durable_configuration, arbiter))
                };
                let (lease, mut store, mut configuration, mut durable_configuration, mut arbiter) = match initialize() {
                    Ok(v) => {
                        let _ = started.send(Ok(()));
                        v
                    }
                    Err(e) => {
                        let _ = started.send(Err(e));
                        return;
                    }
                };
                let mut restore_policy = crate::restore_policy::RestorePolicy::default();
                let mut principals = BTreeMap::<u64, Uuid>::new();
                let mut manual_versions = BTreeMap::new();
                let mut temporary = BTreeMap::<String, homesense_storage::variables::Stored>::new();
                let mut in_flight: Option<u64> = None;
                let mut write_deadline: Option<u64> = None;
                let mut next_id = 1u64;
                let mut failed = false;
                let mut stopped = None;
                let mut publication: Option<PendingPublication> = None;
                loop {
                    let current = if failed || worker_stop.load(Ordering::Acquire)
                        || !arbiter.policy().gateway_ready || arbiter.policy().publishing {
                        None
                    } else {
                        Some(TaskGeneration { boot: worker_boot.clone(), epochs: arbiter.policy().epochs })
                    };
                    generation_updates.send_if_modified(|previous| {
                        if *previous == current { false } else { *previous = current; true }
                    });
                    let pending = match write_deadline {
                        Some(deadline) => receiver.recv_timeout(Duration::from_millis(
                            deadline.saturating_sub(millis(clock)),
                        )),
                        None => receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                    };
                    let now = millis(clock);
                    if worker_stop.load(Ordering::Acquire) && !failed {
                        failed = true;
                        let _ = arbiter.storage_failed();
                    }
                    if let Some(id) =
                        in_flight.filter(|_| write_deadline.is_some_and(|deadline| now >= deadline))
                    {
                        // The old transport may have written some bytes. Never retry or
                        // accept a late success after the dispatch lease expires.
                        let _ = store.fail_transport(&worker_boot, now);
                        principals.clear();
                        let _ = arbiter.finish_write(id, false);
                        let _ = arbiter.storage_failed();
                        in_flight = None;
                        write_deadline = None;
                        failed = true;
                    }
                    let message = match pending {
                        Ok(message) => message,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match message {
                        Message::FeedbackSettled(controller,generation,lease,reply) => {
                            let result = (|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let _guard=lease.lock(Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128(),configuration.revision(),arbiter.policy().epochs.transport,millis(clock)).map_err(|_|WriteStatus::ResourceBusy)?;
                                for (device,observed) in &lease.observations {
                                    // Every combined light read must cover its power supply too.
                                    if configuration.devices().get(device).and_then(|d|d.supply_relay.as_ref()).is_some_and(|relay|!lease.observations.contains_key(relay)) {return Err(WriteStatus::ResourceBusy);}
                                    if !store.feedback_is_settled(&worker_boot,device,*observed).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {return Ok(false);}
                                }
                                Ok(true)
                            })();
                            let _=reply.send(result);
                        }
                        Message::SceneQuery(controller,generation,query,reply) => {
                            let result = if !authorize(controller) {Err(WriteStatus::InsufficientPrivileges)}
                            else if generation.boot != worker_boot || generation.epochs.configuration != configuration.revision() {Err(WriteStatus::ServiceCommunicationFailure)}
                            else {store.query_scene(configuration.revision(), &query).map_err(|error| match error {
                                homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                                _ => WriteStatus::ServiceCommunicationFailure,
                            })};
                            let _=reply.send(result);
                        }
                        Message::SceneUpdate(controller,generation,update,reply) => {
                            let result = if !authorize(controller) { Err(WriteStatus::InsufficientPrivileges) }
                            else if generation.boot != worker_boot || (matches!(&update,SceneUpdate::Begin{..}) && (failed || generation.epochs != arbiter.policy().epochs)) { Err(WriteStatus::ServiceCommunicationFailure) }
                            else {
                                let stored = match update {
                                    SceneUpdate::Begin{run,definition} => store.begin_scene_run(&worker_boot,&run,generation.epochs.configuration,definition.as_deref(),millis(clock)).map(|()|0),
                                    SceneUpdate::Checkpoint{run,expected,status} => store.checkpoint_scene_run(&worker_boot,&run,expected,status,millis(clock)),
                                };
                                stored.map_err(|error| match error {
                                    homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                                    _ => { failed=true;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure }
                                })
                            };
                            let _=reply.send(result);
                        }
                        Message::ManualFence(reply) => { let _ = reply.send(next_id); }
                        Message::RuleOutcome(controller,generation,correlation,error,reply)=>{
                            let result=if !authorize(controller){Err(WriteStatus::InsufficientPrivileges)}
                            else if failed || generation.boot!=worker_boot || generation.epochs.configuration!=configuration.revision(){Err(WriteStatus::ServiceCommunicationFailure)}
                            else{store.finish_rule_chain(&worker_boot,configuration.revision(),correlation,millis(clock),error.as_deref()).map_err(|error|match error{
                                homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                _=>{failed=true;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                            })};
                            let _=reply.send(result);
                        }
                        Message::RuleChain(controller,generation,correlation,after,limit,reply)=>{
                            let result=if !authorize(controller){Err(WriteStatus::InsufficientPrivileges)}
                            else if generation.boot!=worker_boot{Err(WriteStatus::ServiceCommunicationFailure)}
                            else{store.rule_chain(&worker_boot,correlation,after,limit).map_err(|_|WriteStatus::ServiceCommunicationFailure)};
                            let _=reply.send(result);
                        }
                        Message::CommandStatuses(controller, generation, ids, reply) => {
                            let result = (|| {
                                if !authorize(controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                if generation.boot != worker_boot { return Err(WriteStatus::ServiceCommunicationFailure); }
                                ids.into_iter().map(|id| store.command_status(&worker_boot,id)
                                    .map_err(|_|WriteStatus::ServiceCommunicationFailure)?
                                    .ok_or(WriteStatus::ResourceDoesNotExist)).collect()
                            })();
                            let _ = reply.send(result);
                        }

                        Message::Calendar(controller, generation, update, reply) => {
                            let result = if !authorize(controller) { Err(WriteStatus::InsufficientPrivileges) }
                            else if failed || generation.boot != worker_boot || generation.epochs != arbiter.policy().epochs { Err(WriteStatus::ServiceCommunicationFailure) }
                            else { store.calendar_watermark(configuration.revision(), update).map_err(|error| match error {
                                homesense_storage::Error::Conflict => WriteStatus::ResourceBusy,
                                homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                                _ => { failed = true; WriteStatus::ServiceCommunicationFailure }
                            }) };
                            let _ = reply.send(result);
                        }
                        Message::InitializeVariables(controller,generation,definitions,reply)=>{
                            let result=(||{
                                use homesense_core::rule::Lifetime;
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs{return Err(WriteStatus::ServiceCommunicationFailure);}
                                let mut next=temporary.clone();let mut persistent=vec![];
                                for (name,definition) in definitions {
                                    if definition.value.is_some(){return Err(WriteStatus::InvalidValueInRequest);}
                                    match definition.lifetime {
                                        Lifetime::Persistent=>{
                                            if temporary.contains_key(&name){return Err(WriteStatus::InvalidValueInRequest);}
                                            persistent.push((name,definition.value_type));
                                        },
                                        Lifetime::Temporary=>{
                                            if store.logical_variable(configuration.revision(),&name).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.is_some(){return Err(WriteStatus::InvalidValueInRequest);}
                                            if let Some(value)=next.get(&name) {
                                                if value.kind!=definition.value_type{return Err(WriteStatus::InvalidValueInRequest);}
                                            }else{next.insert(name,homesense_storage::variables::Stored{kind:definition.value_type,lifetime:Lifetime::Temporary,value:None,version:0});}
                                        },
                                        Lifetime::FeedbackCache=>return Err(WriteStatus::InvalidValueInRequest),
                                    }
                                }
                                if next.len()>1024{return Err(WriteStatus::InvalidValueInRequest);}
                                store.ensure_logical_variables(configuration.revision(),&persistent).map_err(|error|match error{
                                    homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                    _=>{failed=true;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                                })?;
                                temporary=next;Ok(())
                            })();
                            let _=reply.send(result);
                        }
                        Message::Temporary(controller, generation, name, kind, reply) => {
                            let result=(||{
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                if temporary.len()>=1024 || temporary.contains_key(&name) || store.logical_variable(configuration.revision(),&name).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.is_some() {return Err(WriteStatus::InvalidValueInRequest);}
                                temporary.insert(name,homesense_storage::variables::Stored{kind,lifetime:homesense_core::rule::Lifetime::Temporary,value:None,version:0});
                                Ok(())
                            })();
                            let _=reply.send(result);
                        }
                        Message::VariableSnapshot(controller, generation, names, reply) => {
                            let result = (|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot != worker_boot || generation.epochs != arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let mut values = BTreeMap::new();
                                for name in names {
                                    let value=match temporary.get(&name) {Some(value)=>value.clone(),None=>store.logical_variable(configuration.revision(),&name).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ResourceDoesNotExist)?};
                                    if values.insert(name,value).is_some() {return Err(WriteStatus::InvalidValueInRequest);}
                                }
                                Ok(values)
                            })();
                            let _ = reply.send(result);
                        }
                        Message::Variable(controller, generation, name, define, reply) => {
                            let result = (|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot != worker_boot || generation.epochs != arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                if temporary.contains_key(&name) {return Err(WriteStatus::InvalidValueInRequest);}
                                if let Some(kind) = define {
                                    store.define_logical_variable(configuration.revision(), &name, kind).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                }
                                store.logical_variable(configuration.revision(), &name).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _ = reply.send(result);
                        }
                        Message::StudyMode(reply) => {
                            let result = store.study_mode(configuration.revision()).map_err(|_|WriteStatus::ServiceCommunicationFailure);
                            let _ = reply.send(result);
                        }
                        Message::CancelRun(controller, generation, run, reply) => {
                            let result = if !authorize(controller) { Err(WriteStatus::InsufficientPrivileges) }
                            else if failed || generation.boot != worker_boot || generation.epochs != arbiter.policy().epochs { Err(WriteStatus::ServiceCommunicationFailure) }
                            else {
                                arbiter.cancel_run_committed(run, |items|store.cancel_queued(&worker_boot,items,millis(clock)))
                                    .map(|items| { for item in &items { principals.remove(&item.command.id); } items.len() })
                                    .map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            };
                            let _ = reply.send(result);
                        }
                        Message::Wake => {},
                        Message::BeginPublication(document, expected, deadline, reply) => {
                            if reply.is_closed() { continue; }
                            let result = (|| {
                                if failed || arbiter.policy().publishing || expected != configuration.revision() {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let candidate = homesense_config::Configuration::parse(&document, options.context)
                                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
                                if candidate.revision() <= expected { return Err(WriteStatus::InvalidValueInRequest); }
                                if store.active().map_err(|_| WriteStatus::ServiceCommunicationFailure)?.as_ref() != Some(&durable_configuration) {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let cancelled = arbiter.begin_publication(millis(clock), deadline)
                                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                if store.cancel_queued(&worker_boot, &cancelled, millis(clock)).is_err() {
                                    failed = true;
                                    let _ = arbiter.storage_failed();
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                for c in &cancelled { principals.remove(&c.command.id); }
                                publication = Some(PendingPublication { document, cancellations: cancelled });
                                Ok(())
                            })();
                            let _ = reply.send(result);
                        }
                        Message::AbortPublication(reply) => {
                            let result = (|| {
                                let cancelled = arbiter.abort_before_commit().map_err(|_| WriteStatus::ResourceBusy)?;
                                publication = None;
                                if store.cancel_queued(&worker_boot, &cancelled, millis(clock)).is_err() {
                                    failed = true;
                                    let _ = arbiter.storage_failed();
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                for c in &cancelled { principals.remove(&c.command.id); }
                                Ok(())
                            })();
                            let _ = reply.send(result);
                        }
                        Message::CommitPublication(reply) => {
                            if reply.is_closed() { continue; }
                            let result = (|| {
                                if failed { return Err(WriteStatus::ServiceCommunicationFailure); }
                                let pending = publication.as_ref().ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                arbiter.begin_commit(millis(clock)).map_err(|e| if e == ArbiterError::Busy {
                                    WriteStatus::ResourceBusy
                                } else { WriteStatus::ServiceCommunicationFailure })?;
                                let audit: Vec<_> = pending.cancellations.iter().map(|c| homesense_storage::CancellationAudit {
                                    boot_id: &worker_boot, command_id: c.command.id, reason: c.reason,
                                }).collect();
                                let attempted = store.publish_configuration(Some(configuration.revision()), &pending.document, options.context, &audit);
                                // Read back durable truth even on transaction failure. Never
                                // infer rollback, restore a queue, or retry a physical action.
                                let current = store.active();
                                let committed = current.as_ref().ok().and_then(|v| v.as_ref()).filter(|v| v.payload == pending.document);
                                if let Some(active) = committed {
                                    let next = homesense_config::Configuration::parse(&active.payload, options.context)
                                        .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                    arbiter.record_commit(homesense_core::arbiter::CommitResult::Committed(active.revision))
                                        .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                    configuration = next;
                                    manual_versions.clear();
                                    temporary.clear();
                                    durable_configuration = active.clone();
                                    publication = None;
                                    restore_policy = crate::restore_policy::RestorePolicy::default();
                                    return Ok(configuration.revision());
                                }
                                let rolled_back = attempted.is_err() && current.as_ref().ok().and_then(|v| v.as_ref()) == Some(&durable_configuration);
                                let _ = arbiter.record_commit(if rolled_back {
                                    homesense_core::arbiter::CommitResult::RolledBack
                                } else { homesense_core::arbiter::CommitResult::Unknown });
                                publication = None;
                                failed = true;
                                let _ = arbiter.storage_failed();
                                Err(WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _ = reply.send(result);
                        }

                        Message::Remember(revision, snapshot, max_age, reply) => {
                            let result = (|| {
                                if revision != configuration.revision() || arbiter.policy().publishing {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                if max_age == 0 || snapshot.cursor.boot_id != Uuid::parse_str(&worker_boot).unwrap().as_u128() {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                let mut saved = store.light_restore_levels(configuration.revision()).map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                let levels: Vec<_> = snapshot.devices.iter().filter_map(|(id,state)| {
                                    let (device,module) = configuration.binding(id)?;
                                    if module.kind()!=homesense_core::ModuleKind::Dimmer || state.quality!=homesense_state::Quality::Confirmed || !state.observed_at_ms.and_then(|at| now.checked_sub(at)).is_some_and(|age| age<max_age) { return None; }
                                    let Some(homesense_core::Observation::Level(mut level)) = state.observed else { return None; };
                                    if let Some(relay) = &device.supply_relay {
                                        let supply = snapshot.devices.get(relay)?;
                                        if supply.quality != homesense_state::Quality::Confirmed || !supply.observed_at_ms.and_then(|at| now.checked_sub(at)).is_some_and(|age| age < max_age) { return None; }
                                        match supply.observed {
                                            Some(homesense_core::Observation::Switch(false)) => level = homesense_core::Percent::new(0).unwrap(),
                                            Some(homesense_core::Observation::Switch(true)) => {},
                                            _ => return None,
                                        }
                                    }
                                    match restore_policy.remember(id, level, now) {
                                        true if saved.get(id)!=Some(&level) => Some((id.clone(),level)),
                                        _ => None,
                                    }
                                }).collect();
                                if !levels.is_empty() {
                                    store.remember_light_levels(configuration.revision(), &levels).map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                    saved.extend(levels);
                                }
                                Ok(saved)
                            })();
                            let _ = reply.send(result);
                        }
                        Message::TransportFailed(reply) => {
                            // Disable dispatch before attempting durable audit. A failed
                            // transaction must never reopen control or permit replay.
                            failed = true;
                            if let Some(id) = in_flight.take() {
                                let _ = arbiter.finish_write(id, false);
                            }
                            write_deadline = None;
                            let _ = arbiter.storage_failed();
                            principals.clear();
                            let result = store.fail_transport(&worker_boot, millis(clock))
                                .map_err(|_| WriteStatus::ServiceCommunicationFailure);
                            let _ = reply.send(result);
                        }
                        Message::Shutdown(reply) => {
                            stopped = Some(reply);
                            break;
                        }
                        Message::Recover(epoch, revision, reply) => {
                            if reply.is_closed() { continue; }
                            let result = (|| -> Result<(), WriteStatus> {
                                let failure = WriteStatus::ServiceCommunicationFailure;
                                if !failed || worker_stop.load(Ordering::Acquire)
                                    || in_flight.is_some() || revision != configuration.revision()
                                    || epoch <= arbiter.policy().epochs.transport {
                                    return Err(failure);
                                }
                                let current = store.active().map_err(|_| failure)?;
                                if current.as_ref() != Some(&durable_configuration) { return Err(failure); }
                                store.active_configuration(options.context).map_err(|_| failure)?
                                    .ok_or(failure)?;
                                // Re-run the fault audit before re-enabling dispatch. This
                                // closes records left behind by a transient database error.
                                store.fail_transport(&worker_boot, millis(clock)).map_err(|_| failure)?;
                                arbiter.transport_reconnected(epoch).map_err(|_| failure)?;
                                arbiter.gateway_resynchronized(epoch, revision).map_err(|_| failure)?;
                                arbiter.storage_recovered(revision).map_err(|_| failure)?;
                                principals.clear();
                                failed = false;
                                Ok(())
                            })();
                            let _ = reply.send(result);
                        }
                        Message::Ready(epoch, revision, reply) => {
                            let result = if failed {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            } else if arbiter.policy().publishing {
                                if epoch != arbiter.policy().epochs.transport {
                                    Err(WriteStatus::ServiceCommunicationFailure)
                                } else {
                                    arbiter.synchronized(revision).map_err(|_| WriteStatus::ServiceCommunicationFailure)
                                }
                            } else {
                                arbiter
                                    .gateway_resynchronized(epoch, revision)
                                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)
                            };
                            let _ = reply.send(result);
                        }
                        Message::Dispatch(reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            if failed || in_flight.is_some() {
                                let _ = reply.send(Err(WriteStatus::ResourceBusy));
                                continue;
                            }
                            let now = millis(clock);
                            let result = (|| -> Result<Option<DispatchTicket>, String> {
                                let access = |command: &QueuedCommand| {
                                        let binding = configuration.binding(&command.device_id);
                                        Access {
                                            authorized: principals
                                                .get(&command.id)
                                                .is_some_and(|id| authorize(*id)),
                                            device_exists: binding.is_some(),
                                            device_verified: binding.is_some_and(|(_, m)| {
                                                matches!(
                                                    m.verification(),
                                                    homesense_core::Verification::Verified(_)
                                                )
                                            }),
                                            existing_direct_manual: false,
                                        }
                                    };
                                let dispatch = if arbiter.policy().publishing {
                                    if publication.is_none() { return Ok(None); }
                                    homesense_core::arbiter::Dispatch {
                                        command: arbiter.drain_stop(now, access).map_err(|e| e.to_string())?,
                                        ..Default::default()
                                    }
                                } else {
                                    arbiter.dispatch(now, access).map_err(|e| e.to_string())?
                                };
                                if !dispatch.cancelled.is_empty() {
                                    store.cancel_queued(&worker_boot, &dispatch.cancelled, now)
                                        .map_err(|e| e.to_string())?;
                                }
                                for cancelled in &dispatch.cancelled {
                                    principals.remove(&cancelled.command.id);
                                }
                                for (command, _) in &dispatch.rejected {
                                    store
                                        .transition_command(
                                            &worker_boot,
                                            command.id,
                                            Status::Accepted,
                                            Status::Cancelled,
                                            now,
                                        )
                                        .map_err(|e| e.to_string())?;
                                    principals.remove(&command.id);
                                }
                                let Some(command) = dispatch.command else {
                                    return Ok(None);
                                };
                                let (device, module) = configuration
                                    .binding(&command.device_id)
                                    .ok_or("missing binding")?;
                                let frame = hb_transport::PreparedFrame::prepare(
                                    module,
                                    device.channel,
                                    command.command,
                                    options.context,
                                    command.epochs.transport,
                                )
                                .map_err(|e| e.to_string())?;
                                store
                                    .transition_command(
                                        &worker_boot,
                                        command.id,
                                        Status::Accepted,
                                        Status::Writing,
                                        now,
                                    )
                                    .map_err(|e| e.to_string())?;
                                principals.remove(&command.id);
                                in_flight = Some(command.id);
                                write_deadline = Some(command.deadline_ms);
                                Ok(Some(DispatchTicket {
                                    gateway_id: device.gateway.clone(),
                                    command,
                                    frame,
                                }))
                            })();
                            match result {
                                Ok(ticket) => {
                                    if reply.send(Ok(ticket)).is_err() && in_flight.is_some() {
                                        // Delivery raced caller cancellation. Stop; startup recovery
                                        // marks Writing uncertain and never replays the frame.
                                        failed = true;
                                        let _ = arbiter.storage_failed();
                                    }
                                }
                                Err(_) => {
                                    failed = true;
                                    let _ = arbiter.storage_failed();
                                    let _ =
                                        reply.send(Err(WriteStatus::ServiceCommunicationFailure));
                                }
                            }
                        }
                        Message::Finished(id, complete, reply) => {
                            let result = if failed || in_flight != Some(id) {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            } else {
                                let audit = if complete {
                                    store.transition_command(&worker_boot, id, Status::Writing,
                                        Status::Written, millis(clock))
                                } else {
                                    store.fail_transport(&worker_boot, millis(clock))
                                };
                                if audit.is_err()
                                    || arbiter.finish_write(id, complete).is_err()
                                {
                                    failed = true;
                                    let _ = arbiter.storage_failed();
                                    Err(WriteStatus::ServiceCommunicationFailure)
                                } else {
                                    in_flight = None;
                                    write_deadline = None;
                                    if !complete {
                                        principals.clear();
                                        failed = true;
                                        let _ = arbiter.storage_failed();
                                    }
                                    Ok(())
                                }
                            };
                            let _ = reply.send(result);
                        }
                        Message::Generation(reply) => {
                            let result = if failed || !arbiter.policy().gateway_ready || arbiter.policy().publishing {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            } else {
                                Ok(TaskGeneration { boot: worker_boot.clone(), epochs: arbiter.policy().epochs })
                            };
                            generation_updates.send_replace(result.as_ref().ok().cloned());
                            let _ = reply.send(result);
                        }
                        Message::Submit(submission, generation, deadline, reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            let result = (|| {
                                if failed || millis(clock) >= deadline || generation.as_ref().is_some_and(|scope| match scope {
                                    SubmissionScope::Generation(g) => g.boot != worker_boot || g.epochs != arbiter.policy().epochs,
                                    SubmissionScope::Configuration(revision) => *revision != configuration.revision(),
                                }) {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let (submission, study, automation, variables, feedback, audit, scene) = match *submission {
                                    Submission::Automation { batch, variables, manual_fence, feedback, audit, scene } => {
                                        if manual_fence.is_some_and(|fence|batch.intents.iter().any(|i|manual_versions.get(&i.device).is_some_and(|version|*version>=fence))) {
                                            return Err(WriteStatus::InvalidValueInRequest);
                                        }
                                        if !batch.subscriptions.is_empty() || (batch.intents.is_empty() && variables.is_empty() && audit.is_none()) || variables.len()>1024 || variables.iter().any(|v|v.revision!=configuration.revision()) { return Err(WriteStatus::InvalidValueInRequest); }
                                        (Submission::Lights(batch), None, true, variables, feedback, audit, scene)
                                    },
                                    Submission::Study { batch, expected_mode, next_mode } => {
                                        if batch.intents.iter().any(|i| !["control4:210","control4:278","control4:308","control4:336","control4:63:key:1","control4:63:key:3","control4:63:key:5"].contains(&i.device.as_str())) || batch.intents.is_empty() || batch.intents.len()>16 || !batch.subscriptions.is_empty() {return Err(WriteStatus::InvalidValueInRequest);}
                                        (Submission::Lights(batch), Some((expected_mode,next_mode)), true, vec![], None, None, None)
                                    },
                                    other => (other,None,false,vec![],None,None,None),
                                };
                                if audit.as_ref().is_some_and(|a|a.revision!=configuration.revision()){return Err(WriteStatus::InvalidValueInRequest);}
                                // Prepare in-memory deltas without publishing them. Persistent
                                // variables and queued commands must commit first.
                                let mut memory_updates=BTreeMap::new();
                                let mut persistent_updates=vec![];
                                let mut variable_names=std::collections::BTreeSet::new();
                                for update in variables {
                                    if !variable_names.insert(update.name.clone()){return Err(WriteStatus::InvalidValueInRequest);}
                                    if let Some(current)=temporary.get(&update.name) {
                                        if current.version!=update.expected_version {return Err(WriteStatus::InvalidValueInRequest);}
                                        let correct=matches!((&update.value,current.kind), (None,_) | (Some(homesense_core::rule::Value::Bool(_)),homesense_core::rule::ValueType::Bool) | (Some(homesense_core::rule::Value::Integer(_)),homesense_core::rule::ValueType::Integer));
                                        if !correct{return Err(WriteStatus::InvalidValueInRequest);}
                                        let mut next=current.clone();next.value=update.value;next.version=next.version.checked_add(1).ok_or(WriteStatus::InvalidValueInRequest)?;
                                        memory_updates.insert(update.name,next);
                                    } else {persistent_updates.push(update);}
                                }
                                let variables=persistent_updates;
                                let _feedback_guard = feedback.as_ref().map(|lease|lease.lock(
                                    Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128(),
                                    configuration.revision(), arbiter.policy().epochs.transport, millis(clock))
                                    .map_err(|_|WriteStatus::InvalidValueInRequest)).transpose()?;
                                if let Some(lease) = &feedback {
                                    for (device, observed) in &lease.observations {
                                        if !store.feedback_is_settled(&worker_boot,device,*observed).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {
                                            return Err(WriteStatus::ResourceBusy);
                                        }
                                    }
                                }
                                let mut restore_intents = Vec::new();
                                let (batch, compiled) = match submission {
                                    Submission::Study { .. } | Submission::Automation { .. } => unreachable!(),
                                    Submission::Lights(mut batch) => {
                                        if !authorize(batch.controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                        restore_intents = batch.intents.iter().map(|i| (i.device.clone(), i.command)).collect();
                                        let mut expanded = Vec::new();
                                        let mut seen = std::collections::BTreeSet::new();
                                        for intent in batch.intents {
                                            if !seen.insert(intent.device.clone()) || configuration.devices().values().any(|d| d.enabled && d.supply_relay.as_ref() == Some(&intent.device)) {
                                                return Err(WriteStatus::InvalidValueInRequest);
                                            }
                                            let (device, module) = configuration.binding(&intent.device).ok_or(WriteStatus::ResourceDoesNotExist)?;
                                            if let Some(relay) = &device.supply_relay {
                                                // HAP normalization must resolve restoration before admission.
                                                if matches!(intent.command, homesense_core::Command::Switch(true)) {
                                                    return Err(WriteStatus::InvalidValueInRequest);
                                                }
                                                for (output, command) in crate::combined_light::plan(intent.command, None, device.default_on_level).map_err(|_| WriteStatus::InvalidValueInRequest)? {
                                                    expanded.push(homesense_hap::requests::Intent { device: match output {
                                                        crate::combined_light::Output::Relay => relay.clone(),
                                                        crate::combined_light::Output::Dimmer => intent.device.clone(),
                                                    }, command });
                                                }
                                            } else {
                                                expanded.push(homesense_hap::requests::Intent { device: intent.device, command: if automation && matches!(intent.command, homesense_core::Command::PanelLed(_) | homesense_core::Command::PauseRamp) {intent.command} else {crate::light_command::plan(module, intent.command).map_err(|_| WriteStatus::InvalidValueInRequest)?} });
                                            }
                                        }
                                        batch.intents = expanded;
                                        (batch, true)
                                    },
                                    Submission::PanelLed { controller, device, active } => (
                                        Batch { controller, intents: vec![homesense_hap::requests::Intent {
                                            device, command: homesense_core::Command::PanelLed(active)
                                        }], subscriptions: vec![] }, true),
                                    Submission::Motor { controller, device, action } => {
                                        if !authorize(controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                        let command = configuration.motor_command(&device, action)
                                            .map_err(|_| WriteStatus::InvalidValueInRequest)?;
                                        (Batch { controller, intents: vec![homesense_hap::requests::Intent { device, command }], subscriptions: vec![] }, true)
                                    }
                                };
                                if !authorize(batch.controller) {
                                    return Err(WriteStatus::InsufficientPrivileges);
                                }
                                if batch.intents.len() > 128 {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                // Subscription-only requests still require authorization.
                                if batch.intents.is_empty() {
                                    if !variables.is_empty() || audit.is_some() {
                                        match store.accept_queue_batch_with_rule_audit(&worker_boot,&[],&[],millis(clock),None,&variables,audit.as_ref()) {
                                            Ok(()) => {},
                                            Err(homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit) => return Err(WriteStatus::InvalidValueInRequest),
                                            Err(_) => {
                                                failed = true;
                                                let _ = arbiter.storage_failed();
                                                return Err(WriteStatus::ServiceCommunicationFailure);
                                            }
                                        }
                                    }
                                    temporary.extend(memory_updates);
                                    return Ok(Vec::new());
                                }
                                let end = next_id
                                    .checked_add(batch.intents.len() as u64)
                                    .ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let manual_devices: Vec<_> = if automation {vec![]} else {batch.intents.iter().map(|i|i.device.clone()).collect()};
                                let mut commands = Vec::new();
                                for (offset, intent) in batch.intents.into_iter().enumerate() {
                                    let (device, module) = configuration
                                        .binding(&intent.device)
                                        .ok_or(WriteStatus::ResourceDoesNotExist)?;
                                    let command = if compiled { intent.command } else {
                                        crate::light_command::plan(module, intent.command)
                                            .map_err(|_| WriteStatus::InvalidValueInRequest)?
                                    };
                                    hb_protocol::encode_in_context(
                                        module,
                                        device.channel,
                                        command,
                                        options.context,
                                    )
                                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
                                    commands.push((
                                        QueuedCommand {
                                            id: next_id + offset as u64,
                                            device_id: intent.device,
                                            command,
                                            origin: if automation { Origin::Rule } else { Origin::Hap },
                                            epochs: arbiter.policy().epochs,
                                            deadline_ms: deadline,
                                            run_id: automation.then_some(next_id),
                                        },
                                        Access {
                                            authorized: true,
                                            device_exists: true,
                                            device_verified: matches!(
                                                module.verification(),
                                                homesense_core::Verification::Verified(_)
                                            ),
                                            existing_direct_manual: false,
                                        },
                                    ));
                                }
                                let now = millis(clock);
                                let result = arbiter.submit_batch_committed(
                                    commands,
                                    now,
                                    |commands, cancelled| {
                                        store.accept_queue_batch_with_audits(
                                            &worker_boot,
                                            commands,
                                            cancelled,
                                            now,
                                            study,
                                            &variables,
                                            audit.as_ref(),
                                            scene.as_ref(),
                                        )
                                    },
                                );
                                match result {
                                    Ok(cancelled) => {
                                        temporary.extend(memory_updates);
                                        for device in manual_devices { manual_versions.insert(device,next_id); }
                                        for (device, command) in restore_intents {
                                            restore_policy.accepted(device, command, now);
                                        }
                                        for id in next_id..end {
                                            principals.insert(id, batch.controller);
                                        }
                                        for cancellation in cancelled {
                                            principals.remove(&cancellation.command.id);
                                        }
                                        let ids = (next_id..end).collect();
                                        next_id = end;
                                        Ok(ids)
                                    }
                                    Err(BatchCommitError::Admission(ArbiterError::Queue(_))) => {
                                        Err(WriteStatus::ResourceBusy)
                                    }
                                    Err(BatchCommitError::Admission(_)) => {
                                        Err(WriteStatus::ServiceCommunicationFailure)
                                    }
                                    Err(BatchCommitError::Persistence(homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit)) => {
                                        Err(WriteStatus::InvalidValueInRequest)
                                    }
                                    Err(BatchCommitError::Persistence(_)) => {
                                        failed = true;
                                        let _ = arbiter.storage_failed();
                                        Err(WriteStatus::ServiceCommunicationFailure)
                                    }
                                }
                            })();
                            let _ = reply.send(result);
                        }
                    }
                }
                generation_updates.send_replace(None);
                drop(store);
                drop(lease);
                if let Some(reply) = stopped {
                    let _ = reply.send(());
                }
            })
            .map_err(|e| e.to_string())?;
        start.await.map_err(|_| "admission worker exited")??;
        Ok(Self {
            sender,
            boot,
            clock,
            ttl: options.request_ttl,
            stop_requested,
            generations,
        })
    }
    /// Trusted runtime coordinator only: validate then enter the publication barrier.
    /// The caller must drain STOP via the transport owner, commit, rebuild projections
    /// and synchronize the new mapping before reopening control. Not a HAP endpoint.
    pub async fn begin_publication(
        &self,
        document: Vec<u8>,
        expected: u64,
        timeout: Duration,
    ) -> Result<(), WriteStatus> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(30)
            || document.len() > 4 * 1024 * 1024
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let deadline = millis(self.clock)
            .checked_add(timeout.as_millis() as u64)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::BeginPublication(
                document, expected, deadline, reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn commit_publication(&self) -> Result<u64, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::CommitPublication(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Before commit only; never revives cancelled work or old task generations.
    pub async fn abort_publication(&self) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::AbortPublication(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Only the feedback owner supplies snapshots; never call with desired state.
    pub async fn remember_levels(
        &self,
        revision: u64,
        snapshot: homesense_state::Snapshot,
        max_age: u64,
    ) -> Result<RestoreLevels, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Remember(revision, snapshot, max_age, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Called by the sole transport owner. No socket is opened here.
    pub async fn dispatch(&self) -> Result<Option<DispatchTicket>, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Dispatch(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Full local write is Written, never Observed. Any incomplete result is uncertain.
    pub async fn finish_write(&self, id: u64, complete: bool) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Finished(id, complete, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn transport_failed(&self) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::TransportFailed(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) fn stop_now(&self) {
        self.stop_requested.store(true, Ordering::Release);
        // An idle worker may be blocked in recv. Full means queued work already
        // wakes it; disconnected means it has stopped. Neither needs a retry.
        let _ = self.sender.try_send(Message::Wake);
    }
    pub(crate) fn monotonic_ms(&self) -> u64 {
        millis(self.clock)
    }
    pub fn boot_id(&self) -> &str {
        &self.boot
    }
    /// One actor message reads the complete requested variable set without
    /// interleaving configuration publication or another rule update.
    /// Internal audit read remains available after transport failure. It cannot
    /// authorize or replay commands and is scoped to the captured process boot.
    pub(crate) async fn feedback_settled(&self,controller:Uuid,generation:TaskGeneration,lease:crate::feedback::RuleLease)->Result<bool,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::FeedbackSettled(controller,generation,lease,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn scene_history(
        &self, controller: Uuid, generation: TaskGeneration,
        query: homesense_storage::scene_run::Query,
    ) -> Result<Option<homesense_storage::scene_run::Page>, WriteStatus> {
        query.validate().map_err(|_|WriteStatus::InvalidValueInRequest)?;
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::SceneQuery(controller,generation,query,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn scene_update(&self, controller: Uuid, generation: TaskGeneration, update: SceneUpdate) -> Result<u64, WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::SceneUpdate(controller,generation,update,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn record_rule_outcome(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        correlation: u128,
        error: Option<String>,
    ) -> Result<(), WriteStatus> {
        if correlation == 0 || error.as_ref().is_some_and(|e| e.len() > 8192) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::RuleOutcome(
                controller,
                generation,
                correlation,
                error,
                reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn rule_chain(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        correlation: u128,
        after: u64,
        limit: usize,
    ) -> Result<homesense_storage::rule_audit::Page, WriteStatus> {
        if correlation == 0 || after > i64::MAX as u64 || !(1..=64).contains(&limit) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::RuleChain(
                controller,
                generation,
                correlation,
                after,
                limit,
                reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn command_statuses(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        ids: Vec<u64>,
    ) -> Result<Vec<Status>, WriteStatus> {
        if ids.is_empty() || ids.len() > 128 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::CommandStatuses(controller, generation, ids, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn calendar_watermark(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        update: Option<homesense_storage::calendar::Checkpoint>,
    ) -> Result<Option<i64>, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Calendar(controller, generation, update, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn variable_snapshot(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        names: Vec<String>,
    ) -> Result<BTreeMap<String, homesense_storage::variables::Stored>, WriteStatus> {
        if names.len() > 1024 || names.iter().any(|n| n.is_empty() || n.len() > 256) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::VariableSnapshot(
                controller, generation, names, reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn manual_fence(&self) -> Result<u64, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::ManualFence(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)
    }
    pub(crate) async fn submit_scene(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        manual_fence: u64,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scene_with_feedback(generation, batch, manual_fence, None)
            .await
    }
    pub(crate) async fn submit_scene_with_feedback(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        manual_fence: u64,
        feedback: Option<crate::feedback::RuleLease>,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation {
                batch,
                variables: vec![],
                manual_fence: Some(manual_fence),
                feedback: feedback.map(Box::new),
                audit: None,
                scene: None,
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    pub(crate) async fn submit_scene_step(
        &self, generation: TaskGeneration, batch: Batch, manual_fence: u64,
        scene: homesense_storage::scene_run::StepLink,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(Submission::Automation { batch, variables: vec![], manual_fence: Some(manual_fence), feedback: None, audit: None, scene: Some(scene) }, Some(SubmissionScope::Generation(generation))).await
    }
    pub(crate) async fn submit_audited_rule(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        feedback: Option<crate::feedback::RuleLease>,
        audit: homesense_storage::rule_audit::Admission,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation {
                batch,
                variables,
                feedback: feedback.map(Box::new),
                audit: Some(audit),
                scene: None,
                manual_fence: None,
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    pub(crate) async fn submit_feedback_rule(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        feedback: crate::feedback::RuleLease,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation {
                batch,
                variables,
                manual_fence: None,
                feedback: Some(Box::new(feedback)),
                audit: None,
                scene: None,
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    pub async fn submit_rule_update(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation {
                batch,
                variables,
                manual_fence: None,
                feedback: None,
                audit: None,
                scene: None,
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    /// Defining a variable is an explicit configuration operation; existing names
    /// cannot be silently overwritten. A read passes None and leaves storage unchanged.
    pub async fn initialize_variables(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        definitions: BTreeMap<String, homesense_core::rule::Variable>,
    ) -> Result<(), WriteStatus> {
        if definitions.len() > 1024
            || definitions
                .keys()
                .any(|n| n.is_empty() || n.len() > 256 || n.chars().any(char::is_control))
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::InitializeVariables(
                controller,
                generation,
                definitions,
                reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn define_temporary_variable(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        name: String,
        kind: homesense_core::rule::ValueType,
    ) -> Result<(), WriteStatus> {
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Temporary(
                controller, generation, name, kind, reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn logical_variable(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        name: String,
        define: Option<homesense_core::rule::ValueType>,
    ) -> Result<Option<homesense_storage::variables::Stored>, WriteStatus> {
        if name.len() > 256 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Variable(
                controller, generation, name, define, reply,
            ))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn study_mode(&self) -> Result<Option<u8>, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::StudyMode(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn submit_study(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        expected_mode: Option<u8>,
        next_mode: u8,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Study {
                batch,
                expected_mode,
                next_mode,
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    pub async fn cancel_scene_run(
        &self,
        controller: Uuid,
        generation: TaskGeneration,
        run: u64,
    ) -> Result<usize, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::CancelRun(controller, generation, run, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn submit(&self, batch: Batch) -> Result<Vec<u64>, WriteStatus> {
        self.submit_request(Submission::Lights(batch)).await
    }
    /// Trusted authenticated adapter entry. Resolve logical direction from the
    /// persisted per-channel calibration on the admission worker, never from a
    /// client-supplied electrical direction. This does not publish HAP accessories.
    pub async fn submit_motor(
        &self,
        controller: Uuid,
        device: homesense_core::DeviceId,
        action: homesense_core::MotorAction,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_request(Submission::Motor {
            controller,
            device,
            action,
        })
        .await
    }
    /// Manual LED control only; encoding checks the persisted panel profile,
    /// verification and Manual LED mode before any command is accepted.
    pub async fn submit_panel_led(
        &self,
        controller: Uuid,
        device: homesense_core::DeviceId,
        active: bool,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_request(Submission::PanelLed {
            controller,
            device,
            active,
        })
        .await
    }
    pub(crate) fn generation_updates(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<TaskGeneration>> {
        self.generations.clone()
    }
    /// Capture before starting a timer or other delayed producer. This is not
    /// authorization; controller permissions are still checked on submission.
    pub async fn task_generation(&self) -> Result<TaskGeneration, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Generation(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Delayed producers must retain the original generation across awaits.
    pub async fn submit_delayed(
        &self,
        generation: TaskGeneration,
        batch: Batch,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Lights(batch),
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    /// Bind a graph's interpretation to its immutable configuration revision.
    /// Reconnection preserves this binding; publication invalidates the old graph.
    pub async fn submit_for_revision(
        &self,
        revision: u64,
        batch: Batch,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Lights(batch),
            Some(SubmissionScope::Configuration(revision)),
        )
        .await
    }
    async fn submit_request(&self, submission: Submission) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(submission, None).await
    }
    async fn submit_scoped(
        &self,
        submission: Submission,
        generation: Option<SubmissionScope>,
    ) -> Result<Vec<u64>, WriteStatus> {
        let deadline = millis(self.clock)
            .checked_add(
                u64::try_from(self.ttl.as_millis())
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?,
            )
            .ok_or(WriteStatus::ServiceCommunicationFailure)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Submit(Box::new(submission), generation, deadline, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    /// Stop admission and wait until storage and the exclusive lease are released.
    /// Existing nonterminal audit records are handled by the next startup.
    pub async fn shutdown(self) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Shutdown(reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)
    }
    /// Trusted transport supervisor only: all replacement streams must be bounded,
    /// share this fresh epoch, and have completed queries for the active mapping.
    /// Cancelled owners are terminal and cannot use this to reset stop_now.
    pub async fn recover_synchronized(&self, epoch: u64, revision: u64) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Recover(epoch, revision, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn synchronized(&self, epoch: u64, revision: u64) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Ready(epoch, revision, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn stopping_idle_owner_notifies_generation_subscribers() {
        let directory =
            std::env::temp_dir().join(format!("homesense-idle-stop-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .publish_configuration(
                None,
                include_bytes!("../../../config/simulation.json"),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        drop(store);
        let actor = Admission::open(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            |_| true,
        )
        .await
        .unwrap();
        let generation = actor.task_generation().await.unwrap();
        let mut updates = actor.generation_updates();
        actor.stop_now();
        tokio::time::timeout(
            Duration::from_secs(1),
            updates.wait_for(|v| v.as_ref() != Some(&generation)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(actor.task_generation().await.is_err());
        actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
