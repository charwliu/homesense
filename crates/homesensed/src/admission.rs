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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;
/// Combine completed fault handling and durable audit. The audit argument must
/// be evaluated even when the arbiter failed; neither success alone is recovery.
fn transport_fault_audit_result(
    fault_state: Result<Vec<homesense_core::queue::Cancellation>, homesense_core::arbiter::ArbiterError>,
    journal_audit: Result<(), homesense_storage::Error>,
) -> Result<(), WriteStatus> {
    fault_state
        .map(|_| ())
        .map_err(|_| WriteStatus::ServiceCommunicationFailure)
        .and(journal_audit.map_err(|_| WriteStatus::ServiceCommunicationFailure))
}

#[derive(Clone, Copy)]
pub struct Options {
    pub context: ExecutionContext,
    pub mailbox_capacity: usize,
    pub command_capacity: usize,
    pub request_ttl: Duration,
    /// Shared physical write gap, 20 ms..=1 s. Applies to queries and controls.
    pub write_interval: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            context: ExecutionContext::Live,
            mailbox_capacity: 32,
            command_capacity: 128,
            request_ttl: Duration::from_secs(2),
            write_interval: Duration::from_millis(20),
        }
    }
}
/// Issued only after durable Writing. The owner must report the write result;
/// dropping a ticket is never permission to retry it.
pub struct DispatchTicket {
    pub gateway_id: String,
    pub command: QueuedCommand,
    pub frame: hb_transport::PreparedFrame,
    pub(crate) panel_authorization: Option<crate::panel_auth::Lease>,
    pub(crate) source_authorization: Option<crate::source_auth::Source>,
    pub(crate) away_authorization: Option<crate::away_auth::Authorization>,
    pub(crate) publication_stop:Option<crate::publication_auth::DrainStop>,
}
/// Captured before a delayed task starts. Private fields prevent callers from
/// relabeling old work as belonging to the current runtime generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskGeneration {
    boot: String,
    pub(crate) epochs: Epochs,
}
/// One owner-thread snapshot; readiness does not prove physical device state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    pub gateways:Vec<crate::gateway_runtime::View>,
    pub boot: String,
    pub epochs: Epochs,
    pub queued: usize,
    pub in_flight: bool,
    pub failed: bool,
    pub stopping: bool,
    pub publishing: bool,
    pub gateway_ready: bool,
    pub controller_enabled: bool,
    pub storage_healthy: bool,
    pub valid_configuration_loaded: bool,
    pub wire_capacity:crate::WireCapacity,
}
impl Diagnostics {
    /// A partial gateway failure keeps the current mapping readable. This does
    /// not authorize writes: their exact gateway scope is checked by Admission.
    pub(crate) fn mapping_readable(&self) -> bool {
        !self.failed && !self.stopping && !self.publishing
            && self.controller_enabled && self.storage_healthy && self.valid_configuration_loaded
            && (self.gateway_ready || !self.gateways.is_empty())
    }
    /// Current admission readiness, not end-to-end physical device reachability.
    pub fn ready(&self) -> bool {
        !self.failed && !self.stopping && !self.publishing && self.gateway_ready
            && self.controller_enabled && self.storage_healthy && self.valid_configuration_loaded
    }
}
enum SubmissionScope {
    Generation(TaskGeneration),
    Configuration(u64),
}
// Only Rejected(ResourceBusy) proves that admission did not accept work.
// Failures while reading receipts after acceptance must remain fatal.
#[derive(Debug)]
pub(crate) enum DeferredFailure {
    Rejected(WriteStatus),
    Uncertain(WriteStatus),
}
impl DeferredFailure {
    fn status(self) -> WriteStatus {
        match self {
            Self::Rejected(status) | Self::Uncertain(status) => status,
        }
    }
}
/// Trusted in-process acceptance payload. It reserves durable pending receipts;
/// the runtime must reserve capacity beforehand and install timelines afterward.
pub struct DeferredRule {
    pub batch: Batch,
    pub variables: Vec<homesense_storage::variables::Update>,
    pub audit: homesense_storage::rule_audit::Admission,
    pub starts: Vec<homesense_storage::scene_run::Start>,
}
/// Ephemeral installation capability; deliberately not Clone or serializable.
pub struct AcceptedScene {
    pub(crate) generation: TaskGeneration,
    pub(crate) controller: Uuid,
    pub(crate) manual_fence: u64,
    pub(crate) start: homesense_storage::scene_run::Start,
    pub(crate) started_ms: u64,
    pub(crate) causal_deadline: Option<u64>,
    pub(crate) source_authorization: Option<crate::source_auth::Source>,
}
pub struct AcceptedDeferred {
    pub command_ids: Vec<u64>,
    pub scenes: Vec<AcceptedScene>,
}
#[derive(Default)]
pub(crate) struct RuleEntry {
    pub deadline:Option<u64>,pub admit_before:Option<u64>,
    pub selection_reads:Option<BTreeMap<String,homesense_storage::scene_observation::Snapshot>>,
    pub condition_reads:Vec<homesense_storage::variables::ReadCheck>,
    pub deactivated_scenes:std::collections::BTreeSet<String>,
}
struct RuleEntryReads {
    deactivated_scenes:std::collections::BTreeSet<String>,
    selection:Option<BTreeMap<String,homesense_storage::scene_observation::Snapshot>>,
    conditions:Vec<homesense_storage::variables::ReadCheck>,
}
enum Submission {
     Native(Box<crate::native_scene::Submission>),
    NativePhysical(Batch),
    Indicator {
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        feedback: Box<crate::feedback::RuleLease>,
        reads: BTreeMap<String, homesense_storage::scene_observation::Snapshot>,
        output_fence:Option<crate::indicator_outputs::Fence>,
    },
    Automation {
        entry_reads:Option<Box<RuleEntryReads>>,
        software_sample: bool,
        command_sources: Option<Vec<String>>,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        manual_fence: Option<u64>,
        feedback: Option<Box<crate::feedback::RuleLease>>,
        audit: Option<homesense_storage::rule_audit::Admission>,
        scene: Option<homesense_storage::scene_run::StepLink>,
        starts: Vec<homesense_storage::scene_run::Start>,
    },
    Study {
        batch: Batch,
        expected_mode: Option<u8>,
        next_mode: u8,
    },
    Lights(Batch),
    PanelCommand {
        controller: Uuid,
        device: homesense_core::DeviceId,
        command: homesense_core::Command,
        lease: crate::panel_auth::Lease,
    },
    PanelLed {
        controller: Uuid,
        device: homesense_core::DeviceId,
        active: bool,
    },
    KeypadPress {
        controller: Uuid,
        device: homesense_core::DeviceId,
        keypad: String,
        key: u8,
    },
    Motor {
        controller: Uuid,
        device: homesense_core::DeviceId,
        action: homesense_core::MotorAction,
    },
}
type RestoreLevels = BTreeMap<homesense_core::DeviceId, homesense_core::Percent>;
/// Projection lifecycle is distinct from an actual actor/storage read failure.
/// Unavailable performs no restore read/write and never grants control.
pub(crate) enum HapProjection {
    Unavailable,
    Ready {restore:RestoreLevels,targets:RestoreLevels,transport_epoch:u64},
}

struct PendingPublication {
    document: Vec<u8>,
    rules: Option<Vec<u8>>,
    cancellations: Vec<homesense_core::queue::Cancellation>,
    stops:BTreeMap<u64,crate::publication_auth::DrainStop>,
}
pub(crate) enum SceneUpdate {
    Expiry { run: String, expected: u64, expiry: homesense_storage::scene_run::Expiry },
    BeginScoped { run: String, definition: Option<String>, targets:Vec<homesense_core::DeviceId> },
    Checkpoint { run: String, expected: u64, status: homesense_storage::scene_run::Status, cancellation: Option<homesense_storage::scene_run::Cancellation> },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmissionFailure {
    ManualSuperseded,
    Status(WriteStatus),
}
impl From<WriteStatus> for SubmissionFailure {
    fn from(status: WriteStatus) -> Self { Self::Status(status) }
}
impl SubmissionFailure {
    fn status(self) -> WriteStatus {
        match self {
            Self::ManualSuperseded => WriteStatus::InvalidValueInRequest,
            Self::Status(status) => status,
        }
    }
}
// Shared persistence semantics for ordinary remember requests and the atomic
// HAP projection capture. The actor supplies one policy/time snapshot.
fn remember_projection(
    store: &mut Store,
    restore_policy: &mut crate::restore_policy::RestorePolicy,
    configuration: &homesense_config::Configuration,
    boot: &str,
    policy: RuntimePolicy,
    now: u64,
    observation: (&homesense_state::Snapshot, u64),
) -> Result<RestoreLevels, WriteStatus> {
    let (snapshot, max_age) = observation;
    if max_age == 0 || snapshot.cursor.boot_id != Uuid::parse_str(boot).unwrap().as_u128() {
        return Err(WriteStatus::InvalidValueInRequest);
    }
    let mut saved = store
        .light_restore_levels(configuration.revision())
        .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
    // Prune cancelled, superseded or old-epoch local targets first.
    restore_policy
        .targets(policy.epochs, now, |id| store.command_status(boot, id))
        .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
    let mut settled_reads = BTreeMap::new();
    for (id, state) in &snapshot.devices {
        let Some((device, module)) = configuration.binding(id) else {
            continue;
        };
        if module.kind() != homesense_core::ModuleKind::Dimmer {
            continue;
        }
        if state.quality != homesense_state::Quality::Confirmed
            || !state
                .observed_at_ms
                .and_then(|at| now.checked_sub(at))
                .is_some_and(|age| age < max_age)
        {
            continue;
        }
        let mut settled = store
            .feedback_is_settled_in_epoch(boot, id, state.observed_at_ms, policy.epochs.transport)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
        if let Some(relay) = &device.supply_relay {
            let Some(supply) = snapshot.devices.get(relay).filter(|s| {
                s.quality == homesense_state::Quality::Confirmed
                    && s.observed_at_ms
                        .and_then(|at| now.checked_sub(at))
                        .is_some_and(|age| age < max_age)
            }) else {
                continue;
            };
            let sample = supply.observed_at_ms;
            settled &= store
                .feedback_is_settled_in_epoch(boot, relay, sample, policy.epochs.transport)
                .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
        }
        settled_reads.insert(id.clone(), settled);
    }
    let levels: Vec<_> = snapshot
        .devices
        .iter()
        .filter_map(|(id, state)| {
            let (device, module) = configuration.binding(id)?;
            if module.kind() != homesense_core::ModuleKind::Dimmer
                || state.quality != homesense_state::Quality::Confirmed
                || !state
                    .observed_at_ms
                    .and_then(|at| now.checked_sub(at))
                    .is_some_and(|age| age < max_age)
            {
                return None;
            }
            let Some(homesense_core::Observation::Level(mut level)) = state.observed else {
                return None;
            };
            if let Some(relay) = &device.supply_relay {
                let supply = snapshot.devices.get(relay)?;
                if supply.quality != homesense_state::Quality::Confirmed
                    || !supply
                        .observed_at_ms
                        .and_then(|at| now.checked_sub(at))
                        .is_some_and(|age| age < max_age)
                {
                    return None;
                }
                match supply.observed {
                    Some(homesense_core::Observation::Switch(false)) => {
                        level = homesense_core::Percent::new(0).unwrap()
                    }
                    Some(homesense_core::Observation::Switch(true)) => {}
                    _ => return None,
                }
            }
            match restore_policy.remember_settled(
                id,
                level,
                now,
                settled_reads.get(id).copied().unwrap_or(false),
            ) {
                true if saved.get(id) != Some(&level) => Some((id.clone(), level)),
                _ => None,
            }
        })
        .collect();
    if !levels.is_empty() {
        store
            .remember_light_levels(configuration.revision(), &levels)
            .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
        saved.extend(levels);
    }
    Ok(saved)
}

enum GatewayFaultTarget {
    Wire(homesense_core::gateway_scope::WireScope),
    Writing(u64),
}
enum Message {
    ActivateGateways(Vec<crate::gateway_runtime::Initial>,oneshot::Sender<Result<(),WriteStatus>>),
    GatewayFault(GatewayFaultTarget,oneshot::Sender<Result<crate::gateway_runtime::FaultReport,WriteStatus>>),
    GatewayAttempt(String,oneshot::Sender<Result<homesense_core::gateway_scope::WireScope,WriteStatus>>),
    GatewayReady(homesense_core::gateway_scope::WireScope,Vec<crate::gateway_runtime::Sample>,oneshot::Sender<Result<bool,WriteStatus>>),
    GatewayAvailability(oneshot::Sender<Result<Vec<crate::gateway_runtime::View>,WriteStatus>>),
    ReceivedProjection(homesense_core::gateway_scope::WireScope,Vec<u64>,Vec<homesense_core::DeviceId>,u64,homesense_state::Snapshot,oneshot::Sender<Result<(),WriteStatus>>),
    AwayOff(u64,TaskGeneration,crate::away_auth::Authorization,u64,bool,oneshot::Sender<Result<Vec<u64>,WriteStatus>>),
    MotorRuleLease(Uuid,TaskGeneration,Vec<homesense_core::rule_motor::Request>,oneshot::Sender<Result<crate::motor_rules::Lease,WriteStatus>>),
    CurtainViews(Uuid,TaskGeneration,Vec<homesense_core::DeviceId>,oneshot::Sender<Result<BTreeMap<homesense_core::DeviceId,homesense_core::curtain_state::Snapshot>,WriteStatus>>),
    CurtainCandidate(Uuid,TaskGeneration,homesense_core::DeviceId,homesense_core::curtain_state::Timing,oneshot::Sender<Result<homesense_core::curtain_state::Snapshot,WriteStatus>>),
    CurtainInvalidate(Uuid,TaskGeneration,homesense_core::DeviceId),
    ModuleQuery(Uuid,Option<crate::source_auth::Source>,TaskGeneration,String,crate::module_queries::Scope,Instant,bool,oneshot::Sender<Result<crate::module_queries::Accepted,WriteStatus>>),
    ModuleQueryRecords(Uuid,TaskGeneration,Vec<u64>,oneshot::Sender<Result<Vec<homesense_storage::module_history::Record>,WriteStatus>>),
    RecentDeviceCommands(Uuid,TaskGeneration,Vec<homesense_core::DeviceId>,usize,oneshot::Sender<Result<Vec<homesense_storage::module_history::Record>,WriteStatus>>),
    BeginSceneObservation(Uuid,TaskGeneration,u64,crate::feedback::SceneObservationStart,oneshot::Sender<Result<homesense_core::scene_observer::Scope,WriteStatus>>),
    ObserveSceneKey(Uuid,TaskGeneration,homesense_core::scene_observer::Scope,crate::feedback::SceneKeyObservation,oneshot::Sender<Result<(),WriteStatus>>),
    SceneSelections(Uuid,TaskGeneration,Vec<String>,oneshot::Sender<Result<BTreeMap<String,homesense_storage::scene_observation::Snapshot>,WriteStatus>>),
    InitializeSceneSelection(Uuid,TaskGeneration,crate::scene_initialization::Prepared,oneshot::Sender<Result<(),WriteStatus>>),
    ReconcileSceneSelection(Uuid,TaskGeneration,crate::scene_reconciliation::Prepared,oneshot::Sender<Result<(),WriteStatus>>),
    NativeStatus(Uuid,TaskGeneration,String,oneshot::Sender<Result<Option<homesense_storage::native_scene::Record>,WriteStatus>>),
    NativeFail(Uuid,TaskGeneration,String,oneshot::Sender<Result<(),WriteStatus>>),
    NativeFinish(Uuid,Option<crate::source_auth::Source>,Box<crate::native_scene::Completion>,oneshot::Sender<Result<(),WriteStatus>>),
    SceneHistory(Uuid, TaskGeneration, String, Option<homesense_storage::scene_history::Cursor>, usize, oneshot::Sender<Result<homesense_storage::scene_history::Page, WriteStatus>>),
    Diagnostics(Uuid, TaskGeneration, oneshot::Sender<Result<Diagnostics, WriteStatus>>),
    DiagnosticGeneration(Uuid, u64, oneshot::Sender<Result<TaskGeneration, WriteStatus>>),
    ConfigurationExport(Uuid, TaskGeneration, u64, oneshot::Sender<Result<Option<homesense_storage::configuration_history::Export>, WriteStatus>>),
    ConfigurationHistory(Uuid, TaskGeneration, Option<u64>, usize, oneshot::Sender<Result<homesense_storage::configuration_history::Page, WriteStatus>>),
    PublicationBundle(Uuid, oneshot::Sender<Result<(homesense_storage::automation::Bundle, ExecutionContext, u64), WriteStatus>>),
    PublicationState(oneshot::Sender<bool>),
    AutomationBundle(Uuid, TaskGeneration, oneshot::Sender<Result<homesense_storage::automation::Bundle, WriteStatus>>),
    IndicatorOutputs(Uuid,TaskGeneration,Vec<homesense_core::DeviceId>,Vec<u64>,oneshot::Sender<Result<crate::indicator_outputs::Capture,WriteStatus>>),
    FeedbackSettled(Uuid, TaskGeneration, crate::feedback::RuleLease, oneshot::Sender<Result<bool, WriteStatus>>),
    RecordedLevels(Uuid, TaskGeneration, oneshot::Sender<Result<RestoreLevels, WriteStatus>>),
    RecentDimmerOff(Uuid,TaskGeneration,u64,oneshot::Sender<Result<std::collections::BTreeSet<homesense_core::DeviceId>,WriteStatus>>),
    ProjectionLevels(u64, u128, oneshot::Sender<Result<RestoreLevels, WriteStatus>>),
    HapProjection(u64, Option<u64>, homesense_state::Snapshot, u64, oneshot::Sender<Result<HapProjection,WriteStatus>>),
    WrittenFeedbackTarget(Uuid, TaskGeneration, u64, oneshot::Sender<Result<Option<homesense_storage::feedback_inference::WrittenTarget>, WriteStatus>>),
    FeedbackInference(Uuid, TaskGeneration, homesense_storage::feedback_inference::Inference, oneshot::Sender<Result<(), WriteStatus>>),

    SceneQuery(Uuid, TaskGeneration, homesense_storage::scene_run::Query, oneshot::Sender<Result<Option<homesense_storage::scene_run::Page>, WriteStatus>>),

    ManualScenes(Uuid, Option<crate::source_auth::Source>, TaskGeneration, Vec<String>, Vec<homesense_storage::variables::Update>, oneshot::Sender<Result<Vec<AcceptedScene>, WriteStatus>>),
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
    ManualSuperseded(homesense_core::DeviceId, u64, oneshot::Sender<bool>),
    MotorReceipts(Uuid,TaskGeneration,homesense_core::DeviceId,homesense_core::MotorAction,Vec<u64>,oneshot::Sender<Result<Vec<homesense_storage::journal::MotorReceipt>,WriteStatus>>),
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
    BeginPublication(Vec<u8>, Option<Vec<u8>>, u64, u64, Option<crate::publication_auth::Source>, oneshot::Sender<Result<(), WriteStatus>>),
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
    Dispatch(Option<std::collections::BTreeSet<String>>,oneshot::Sender<Result<Option<DispatchTicket>, WriteStatus>>),
    Finished(u64, bool, oneshot::Sender<Result<(), WriteStatus>>),
    Shutdown(oneshot::Sender<()>),
    Submit(
        Box<Submission>,
        Option<crate::source_auth::Source>,
        Option<SubmissionScope>,
        u64,
        oneshot::Sender<Result<Vec<u64>, SubmissionFailure>>,
    ),
    Ready(u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
    Recover(u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
}
#[derive(Clone)]
pub struct Admission {
    context:ExecutionContext,
    source_authorization:Option<crate::source_auth::Source>,
    pub(crate) write_interval: Duration,
    pub(crate) write_slot: std::sync::Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>,
    pub(crate) fade_budget: crate::fade_budget::Budget,
    sender: mpsc::SyncSender<Message>,
    boot: String,
    clock: Clock,
    ttl: Duration,
    stop_requested: Arc<AtomicBool>,
    generations: tokio::sync::watch::Receiver<Option<TaskGeneration>>,
    observation_invalidation: Arc<AtomicU64>,
}
#[cfg(not(test))]
type Clock = Instant;
#[cfg(test)]
#[derive(Clone, Copy)]
struct Clock {
    real: Instant,
    manual: Option<&'static AtomicU64>,
}
#[cfg(test)]
impl Clock {
    fn now(manual: Option<&'static AtomicU64>) -> Self {
        Self { real: Instant::now(), manual }
    }
    fn elapsed(self) -> Duration {
        self.manual.map_or_else(|| self.real.elapsed(), |time| Duration::from_millis(time.load(Ordering::Acquire)))
    }
}
fn millis(clock: Clock) -> u64 {
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
        Self::open_inner(path, options, authorize, #[cfg(test)] None).await
    }
    #[cfg(test)]
    pub(crate) async fn open_with_test_time(
        path: PathBuf,
        options: Options,
        authorize: impl Fn(Uuid) -> bool + Send + Sync + 'static,
        time: &'static AtomicU64,
    ) -> Result<Self, String> {
        Self::open_inner(path, options, authorize, Some(time)).await
    }
    async fn open_inner(
        path: PathBuf,
        options: Options,
        authorize: impl Fn(Uuid) -> bool + Send + Sync + 'static,
        #[cfg(test)] test_time: Option<&'static AtomicU64>,
    ) -> Result<Self, String> {
        if !(1..=128).contains(&options.mailbox_capacity)
            || !(1..=4096).contains(&options.command_capacity)
            || options.request_ttl.as_millis() == 0
            || options.request_ttl > Duration::from_secs(30)
            || !(Duration::from_millis(20)..=Duration::from_secs(1)).contains(&options.write_interval)
        {
            return Err("invalid admission bounds".into());
        }
        let (sender, receiver) = mpsc::sync_channel(options.mailbox_capacity);
        let (generation_updates, generations) = tokio::sync::watch::channel(None);
        let (started, start) = oneshot::channel();
        let boot = Uuid::new_v4().to_string();
        let worker_boot = boot.clone();
        let clock = Clock::now(#[cfg(test)] test_time);
        let authorize = Arc::new(authorize);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_requested.clone();
        let observation_invalidation=Arc::new(AtomicU64::new(1));
        let worker_observation_invalidation=observation_invalidation.clone();
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
                let mut source_leases=BTreeMap::<u64,crate::source_auth::Source>::new();
                let mut panel_leases=BTreeMap::<u64,crate::panel_auth::Lease>::new();
                let mut module_off_jobs = BTreeMap::<u64,crate::away_off::Target>::new();
                let mut away_reset_dependencies = BTreeMap::<u64,Vec<u64>>::new();
                let mut module_queries = BTreeMap::<u64,crate::module_queries::Target>::new();
                let mut motor_guard_ids=BTreeMap::<u64,u64>::new();
                let mut motor_guards=BTreeMap::<u64,crate::motor_rules::QueuedLease>::new();
                let mut manual_versions = BTreeMap::new();
                let mut temporary = BTreeMap::<String, homesense_storage::variables::Stored>::new();
                let mut led_outputs=crate::indicator_outputs::Ledger::default();
                let mut in_flight_led=None;
                let mut in_flight: Option<u64> = None;
                let mut write_deadline: Option<u64> = None;
                let mut next_id = 1u64;
                let mut failed = false;
                let mut gateways:Option<crate::gateway_runtime::State>=None;

                // Only a successful communication-fault audit may keep a HAP
                // graph locally unavailable. Every other terminal failure clears
                // this classification; it never opens ordinary admission.
                let mut audited_transport_failure=false;
                let mut scene_observer=crate::scene_observation_owner::Owner::default();
                let mut scene_projections=BTreeMap::<String,crate::scene_reconciliation::Prepared>::new();
                let observer_boot=Uuid::parse_str(&worker_boot).unwrap().as_u128();
                let mut curtains=crate::curtain_owner::Owner::new(&configuration,homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs},millis(clock));
                let mut observation_fence=worker_observation_invalidation.load(Ordering::Acquire);
                let mut stopped = None;
                let mut publication: Option<PendingPublication> = None;
                loop {
                    if gateways.as_ref().is_some_and(|g|g.table.global().configuration!=arbiter.policy().epochs.configuration
                        || g.table.global().transport!=arbiter.policy().epochs.transport) {gateways=None;}
                    if let Some(g)=&mut gateways && g.table.advance_control(arbiter.policy().epochs).is_err(){failed=true;arbiter.storage_failed().ok();}
                    let current = if failed || worker_stop.load(Ordering::Acquire)
                        || !arbiter.policy().gateway_ready || arbiter.policy().publishing {
                        None
                    } else {
                        Some(TaskGeneration { boot: worker_boot.clone(), epochs: arbiter.policy().epochs })
                    };
                    source_leases.retain(|id,_|principals.contains_key(id) || in_flight==Some(*id));
                    panel_leases.retain(|id,_|principals.contains_key(id) || in_flight==Some(*id));
                    module_off_jobs.retain(|id,_| principals.contains_key(id) || in_flight==Some(*id));
                    away_reset_dependencies.retain(|id,_|principals.contains_key(id)||in_flight==Some(*id));
                    module_queries.retain(|id,_| principals.contains_key(id) || in_flight==Some(*id));
                    motor_guard_ids.retain(|id,_|principals.contains_key(id)||in_flight==Some(*id));
                    motor_guards.retain(|group,_|motor_guard_ids.values().any(|g|g==group));
                    let fence=worker_observation_invalidation.load(Ordering::Acquire);
                    if current.is_none() || scene_observer.scope().is_some_and(|s|s.epochs!=arbiter.policy().epochs) || fence!=observation_fence {scene_observer.invalidate();}
                    observation_fence=fence;
                    if scene_observer.refresh_local_versions(&store,millis(clock)).is_err() {
                        failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();scene_observer.invalidate();
                    }
                    curtains.synchronize(&configuration,homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs},millis(clock),current.is_some());
                    if current.is_some() {
                        let now=millis(clock);
                        if curtains.audit(&store,&configuration,&worker_boot,now).is_err(){failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();curtains.invalidate();}
                        let effects=if failed {vec![]} else {curtains.effects(now,&|id|authorize(id))};
                        if crate::curtain_owner::admit_leds(effects,&configuration,&mut arbiter,&mut store,&worker_boot,now,options.request_ttl.as_millis() as u64,&mut next_id,&mut principals,options.context,&|id|authorize(id),&mut source_leases,gateways.as_ref()).is_err() {
                            failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();curtains.invalidate();
                        }
                    }
                    generation_updates.send_if_modified(|previous| {
                        if *previous == current { false } else { *previous = current; true }
                    });
                    let deadline=write_deadline.into_iter().chain(curtains.next_deadline(millis(clock))).min();
                    let pending = match deadline {
                        Some(deadline) => receiver.recv_timeout(Duration::from_millis(
                            deadline.saturating_sub(millis(clock)),
                        )),
                        None => receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                    };
                    let now = millis(clock);
                    if worker_stop.load(Ordering::Acquire) && !failed {
                        failed = true; audited_transport_failure=false;
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
                        failed = true; audited_transport_failure=false;
                    }
                    let message = match pending {
                        Ok(message) => message,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    // Recheck at the receive boundary, not just before waiting:
                    // an idle actor may receive this read after a TTL or revoke.
                    curtains.retire_revoked(&|id|authorize(id));
                    if curtains.audit(&store,&configuration,&worker_boot,now).is_err(){failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();curtains.invalidate();}
                    match message {
                        Message::ActivateGateways(initial,reply)=>{
                            let result=(|| {
                                if failed || worker_stop.load(Ordering::Acquire) || !arbiter.policy().gateway_ready
                                    || arbiter.policy().publishing || in_flight.is_some() || arbiter.queued_len()!=0 || gateways.is_some() {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let state=crate::gateway_runtime::State::activate(&configuration,arbiter.policy().epochs,initial)?;
                                gateways=Some(state);Ok(())
                            })();let _=reply.send(result);
                        }
                        Message::GatewayAvailability(reply)=>{
                            let result=if failed || worker_stop.load(Ordering::Acquire) || arbiter.policy().publishing {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            }else{Ok(gateways.as_ref().map_or_else(Vec::new,|g|g.views()))};
                            let _=reply.send(result);
                        }
                        Message::GatewayAttempt(gateway,reply)=>{
                            let result=if failed || worker_stop.load(Ordering::Acquire) || arbiter.policy().publishing {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            }else{gateways.as_mut().ok_or(WriteStatus::ServiceCommunicationFailure)
                                .and_then(|g|g.table.begin_attempt(&gateway).map_err(|_|WriteStatus::ServiceCommunicationFailure))};
                            let _=reply.send(result);
                        }
                        Message::GatewayReady(wire,samples,reply)=>{
                            let result=if failed || worker_stop.load(Ordering::Acquire) || arbiter.policy().publishing {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            }else{gateways.as_mut().ok_or(WriteStatus::ServiceCommunicationFailure)
                                .and_then(|g| {
                                    if g.table.current_wire(wire.gateway()).ok().as_ref()==Some(&wire)
                                        && g.table.phase(wire.gateway())==Ok(homesense_core::gateway_scope::Phase::Offline) {return Ok(false);}
                                    g.recover_ready(&configuration,&wire,&samples).map(|()|true)
                                })};
                            let _=reply.send(result);
                        }
                        Message::GatewayFault(target,reply)=>{
                            let result=(|| {
                                if failed || worker_stop.load(Ordering::Acquire) {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let wire=match target {
                                    GatewayFaultTarget::Wire(wire)=>wire,
                                    GatewayFaultTarget::Writing(id)=>{
                                        if in_flight!=Some(id) {return Err(WriteStatus::ServiceCommunicationFailure);}
                                        store.command_wire_scope(&worker_boot,id).map_err(|_|WriteStatus::ServiceCommunicationFailure)?
                                            .ok_or(WriteStatus::ServiceCommunicationFailure)?
                                    },
                                };
                                let g=gateways.as_mut().ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let current=g.table.current_wire(wire.gateway()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                if wire.epoch()<=current.epoch() && store.gateway_fault_recorded(&worker_boot,configuration.revision(),&wire).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {
                                    let audit=g.faults.get(wire.gateway()).filter(|(epoch,_)|*epoch==wire.epoch()).map_or_else(Default::default,|(_,audit)|audit.clone());
                                    return Ok(crate::gateway_runtime::FaultReport {audit,retired:true});
                                }
                                g.table.fault(&wire).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                let audit=store.fail_gateway(&worker_boot,arbiter.policy().epochs,&wire,now)
                                    .map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                arbiter.apply_audited_gateway_fault(&audit.cancelled,&audit.uncertain)
                                    .map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                for id in &audit.cancelled {principals.remove(id);}
                                if in_flight.is_some_and(|id|audit.uncertain.contains(&id)) {
                                    in_flight=None;in_flight_led=None;write_deadline=None;
                                }
                                for (id,device) in configuration.devices() {
                                    if device.gateway==wire.gateway() {
                                        curtains.invalidate_device(id);restore_policy.invalidate_device(id);
                                    }
                                }
                                g.faults.insert(wire.gateway().to_owned(),(wire.epoch(),audit.clone()));
                                Ok(crate::gateway_runtime::FaultReport {audit,retired:false})
                            })();
                            if result.is_err() {failed=true;audited_transport_failure=false;worker_stop.store(true,Ordering::Release);arbiter.storage_failed().ok();}
                            let _=reply.send(result);
                        }
                        Message::MotorRuleLease(controller,generation,requests,reply)=>{
                            let result=(|| {
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InsufficientPrivileges)?;
                                let allowed=document.enabled_motor_requests().map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                crate::motor_rules::Lease::capture(controller,homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs},requests,&allowed,&mut curtains,now)
                            })();let _=reply.send(result);
                        }
                        Message::CurtainCandidate(controller,generation,device,timing,reply)=>{
                            let result=(|| {
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                configuration.motor_command(&device,homesense_core::MotorAction::Stop).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                curtains.candidate(device,timing,now).map_err(|_|WriteStatus::InvalidValueInRequest)
                            })();let _=reply.send(result);
                        }
                        Message::CurtainInvalidate(controller,generation,device)=>{
                            if authorize(controller) && generation.boot==worker_boot && generation.epochs==arbiter.policy().epochs {curtains.invalidate_device(&device);}
                        }
                        Message::CurtainViews(controller,generation,ids,reply)=>{
                            let result=(|| {
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let mut out=BTreeMap::new();
                                for id in ids {
                                    configuration.binding(&id).ok_or(WriteStatus::ResourceDoesNotExist)?;
                                    if let Some(view)=curtains.snapshot(&id,now){out.insert(id,view);}
                                }
                                Ok(out)
                            })();let _=reply.send(result);
                        }

                        Message::PublicationBundle(controller,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || !arbiter.policy().publishing || publication.is_some() {return Err(WriteStatus::ServiceCommunicationFailure)}
                                let bundle=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                if bundle.configuration.revision()!=configuration.revision() {return Err(WriteStatus::ServiceCommunicationFailure)}
                                Ok((bundle,options.context,arbiter.policy().epochs.transport))
                            })();
                            let _=reply.send(result);
                        }
                        Message::DiagnosticGeneration(controller,revision,reply) => {
                            let result=if !authorize(controller) {Err(WriteStatus::InsufficientPrivileges)}
                            else if revision!=configuration.revision() || revision!=durable_configuration.revision || publication.is_some() {Err(WriteStatus::ServiceCommunicationFailure)}
                            else {Ok(TaskGeneration{boot:worker_boot.clone(),epochs:arbiter.policy().epochs})};
                            // The durable pending document is cleared after readback of
                            // the committed pointer. policy.publishing intentionally stays
                            // true until physical resynchronization; do not confuse that
                            // control fence with an uncommitted document. This snapshot
                            // never changes admission or its ready-generation subscribers.
                            let _=reply.send(result);
                        }
                        Message::Diagnostics(controller,generation,reply) => {
                            let policy=arbiter.policy();
                            let result=if !authorize(controller) {Err(WriteStatus::InsufficientPrivileges)}
                            else if generation.boot!=worker_boot || generation.epochs.configuration!=policy.epochs.configuration {Err(WriteStatus::ServiceCommunicationFailure)}
                            else {Ok(Diagnostics {gateways:gateways.as_ref().map_or_else(Vec::new,|g|g.views()),boot:worker_boot.clone(),epochs:policy.epochs,queued:arbiter.queued_len(),in_flight:in_flight.is_some(),failed,stopping:worker_stop.load(Ordering::Acquire),publishing:policy.publishing,gateway_ready:policy.gateway_ready && gateways.as_ref().is_none_or(|g|g.table.all_ready()),controller_enabled:policy.controller,storage_healthy:policy.storage_healthy,valid_configuration_loaded:policy.valid_configuration_loaded,wire_capacity:crate::WireCapacity::for_write_interval(options.write_interval)})};
                            let _=reply.send(result);
                        }
                        Message::PublicationState(reply) => { let _=reply.send(!failed && arbiter.policy().publishing && publication.is_some()); },
                        Message::ConfigurationHistory(controller,generation,before,limit,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure)}
                                store.configuration_history(before,limit).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _=reply.send(result);
                        }
                        Message::ConfigurationExport(controller,generation,revision,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure)}
                                store.export_configuration(revision,options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _=reply.send(result);
                        }
                        Message::AutomationBundle(controller,generation,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure)}
                                let bundle=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                if bundle.configuration.revision()!=generation.epochs.configuration {return Err(WriteStatus::ServiceCommunicationFailure)}
                                Ok(bundle)
                            })();
                            let _=reply.send(result);
                        }
                        Message::HapProjection(revision,transport_epoch,snapshot,max_age,reply) => {
                            let result=(|| {
                                if max_age==0 || snapshot.cursor.boot_id!=Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128() {return Err(WriteStatus::InvalidValueInRequest);}
                                if worker_stop.load(Ordering::Acquire) {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let policy=arbiter.policy();
                                // An explicit closed lifecycle gate is not a failed
                                // database operation. No ledger is touched in this arm.
                                if failed && !audited_transport_failure {return Err(WriteStatus::ServiceCommunicationFailure);}
                                if revision!=configuration.revision() || transport_epoch.is_some_and(|epoch|epoch!=policy.epochs.transport) || policy.publishing || !policy.gateway_ready || audited_transport_failure {return Ok(HapProjection::Unavailable);}
                                if !policy.storage_healthy {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let restore=remember_projection(&mut store,&mut restore_policy,&configuration,&worker_boot,policy,now,(&snapshot,max_age))?;
                                let targets=restore_policy.targets(policy.epochs,now,|id|store.command_status(&worker_boot,id)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                Ok(HapProjection::Ready{restore,targets,transport_epoch:policy.epochs.transport})
                            })();
                            let _=reply.send(result);
                        }
                        Message::ProjectionLevels(revision,boot,reply) => {
                            let result=(|| {
                                if revision!=configuration.revision() || boot!=Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128() || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                restore_policy.targets(arbiter.policy().epochs,millis(clock),|id|store.command_status(&worker_boot,id)).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _=reply.send(result);
                        }
                        Message::RecordedLevels(controller,generation,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let ids=configuration.devices().keys().filter(|id|configuration.binding(id).is_some_and(|(_,m)|m.kind()==homesense_core::ModuleKind::Dimmer)).cloned().collect::<Vec<_>>();
                                store.recorded_dimmer_levels(configuration.revision(),&ids).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _=reply.send(result);
                        }
                        Message::RecentDimmerOff(controller,generation,after,reply) => {
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let mut recent=std::collections::BTreeSet::new();
                                for id in configuration.devices().keys().filter(|id|configuration.binding(id).is_some_and(|(_,m)|m.kind()==homesense_core::ModuleKind::Dimmer)) {
                                    if store.dimmer_setting_written_after(configuration.revision(),id,&worker_boot,after).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {recent.insert(id.clone());}
                                }
                                Ok(recent)
                            })();let _=reply.send(result);
                        }
                        Message::IndicatorOutputs(controller,generation,ids,prior,reply)=>{
                            let result=(||{
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                let policy=arbiter.policy();
                                if failed || generation.boot!=worker_boot || generation.epochs!=policy.epochs {return Err(WriteStatus::ResourceBusy);}
                                if ids.is_empty() || ids.len()>128 || prior.len()>128 {return Err(WriteStatus::InvalidValueInRequest);}
                                if ids.iter().any(|id|configuration.binding(id).is_none_or(|(_,m)|m.kind()!=homesense_core::ModuleKind::Keypad || m.led_mode()!=homesense_core::PanelLedMode::Manual)){return Err(WriteStatus::InvalidValueInRequest);}
                                let mut prior_complete=!prior.is_empty();
                                for id in prior {prior_complete &= store.command_status(&worker_boot,id).map_err(|_|WriteStatus::ServiceCommunicationFailure)?==Some(Status::Written);}
                                Ok(crate::indicator_outputs::Capture{fence:led_outputs.capture(&store,&worker_boot,configuration.revision(),&ids,policy.epochs).map_err(|_|WriteStatus::ServiceCommunicationFailure)?,prior_complete})
                            })();let _=reply.send(result);
                        }
                        Message::FeedbackSettled(controller,generation,lease,reply) => {
                            let result = (|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let _guard=lease.lock(Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128(),configuration.revision(),arbiter.policy().epochs.transport,millis(clock)).map_err(|_|WriteStatus::ResourceBusy)?;
                                for (device,observed) in &lease.observations {
                                    // Every combined light read must cover its power supply too.
                                    if configuration.devices().get(device).and_then(|d|d.supply_relay.as_ref()).is_some_and(|relay|!lease.observations.contains_key(relay)) {return Err(WriteStatus::ResourceBusy);}
                                    if !store.feedback_is_settled_in_epoch(&worker_boot,device,*observed,arbiter.policy().epochs.transport).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {return Ok(false);}
                                }
                                Ok(true)
                            })();
                            let _=reply.send(result);
                        }
                        Message::WrittenFeedbackTarget(controller,generation,id,reply) => {
                            let result=if !authorize(controller) {Err(WriteStatus::InsufficientPrivileges)}
                            else if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {Err(WriteStatus::ServiceCommunicationFailure)}
                            else {store.written_feedback_target(&worker_boot,id).map_err(|_|WriteStatus::ServiceCommunicationFailure)};
                            let _=reply.send(result);
                        }
                        Message::FeedbackInference(controller,generation,inference,reply) => {
                            let result = if !authorize(controller) { Err(WriteStatus::InsufficientPrivileges) }
                            else if failed || generation.boot != worker_boot
                                || generation.epochs != arbiter.policy().epochs
                                || inference.epochs != generation.epochs {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            } else if millis(clock).checked_sub(inference.observed_ms)
                                .is_none_or(|age| age >= inference.window_ms) {
                                Err(WriteStatus::InvalidValueInRequest)
                            } else {
                                store.record_feedback_inference(&worker_boot,&inference).map_err(|error| match error {
                                    homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                                    _ => { failed=true; audited_transport_failure=false; let _=arbiter.storage_failed(); WriteStatus::ServiceCommunicationFailure }
                                })
                            };
                            let _=reply.send(result);
                        }
                        Message::SceneHistory(controller,generation,definition,before,limit,reply) => {
                            let result=if !authorize(controller) {Err(WriteStatus::InsufficientPrivileges)}
                            else if generation.boot!=worker_boot || generation.epochs.configuration!=configuration.revision() {Err(WriteStatus::ServiceCommunicationFailure)}
                            else {store.scene_history(configuration.revision(),&definition,before.as_ref(),limit).map_err(|e|match e {homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,_=>WriteStatus::ServiceCommunicationFailure})};
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
                        Message::ManualScenes(controller,request_source,generation,definitions,updates,reply) => {
                            if reply.is_closed() { continue; }
                            let result = (|| {
                                if request_source.as_ref().is_some_and(|source|!source.valid_for(controller,configuration.revision())) {return Err(WriteStatus::InsufficientPrivileges);}
                                if !authorize(controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                let policy = arbiter.policy();
                                if failed || worker_stop.load(Ordering::Acquire) || policy.publishing
                                    || !policy.gateway_ready || !policy.controller || !policy.storage_healthy
                                    || !policy.valid_configuration_loaded || generation.boot != worker_boot
                                    || generation.epochs != policy.epochs {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                if definitions.is_empty() || definitions.len()>64
                                    || definitions.iter().collect::<std::collections::BTreeSet<_>>().len()!=definitions.len() {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                let bundle=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?
                                    .ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let document=bundle.automation.ok_or(WriteStatus::InvalidValueInRequest)?;
                                for id in &definitions {
                                    let scene=document.timed_scenes.get(id).ok_or(WriteStatus::InvalidValueInRequest)?;
                                    if !scene.enabled || scene.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {
                                        return Err(WriteStatus::InvalidValueInRequest);
                                    }
                                }
                                let starts=definitions.into_iter().map(|definition|homesense_storage::scene_run::ManualStart {
                                    run:Uuid::new_v4().to_string(), definition,
                                }).collect::<Vec<_>>();
                                let now=millis(clock);
                                let gateway_scopes=gateways.as_ref().map(|g|g.scene_scopes(&configuration,&document,&starts.iter().map(|start|(start.run.clone(),start.definition.clone())).collect::<Vec<_>>(),&[])).transpose()?;
                                crate::source_auth::commit(request_source.as_ref(),|| {
                                    match &gateway_scopes {Some(scopes)=>store.begin_manual_scene_batch_scoped(&worker_boot,generation.epochs.configuration,&starts,now,&updates,scopes),None=>store.begin_manual_scene_batch_with_variables(&worker_boot,generation.epochs.configuration,&starts,now,&updates)}
                                }).ok_or(WriteStatus::InsufficientPrivileges)?
                                    .map_err(|error|match error {
                                        homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                        _=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                                    })?;
                                Ok(starts.into_iter().map(|start|AcceptedScene {
                                    generation:generation.clone(),controller,manual_fence:next_id,started_ms:now,causal_deadline:None,source_authorization:request_source.clone(),
                                    start:homesense_storage::scene_run::Start {action_index:0,source:"manual".into(),run:start.run,
                                        revision:generation.epochs.configuration,definition:start.definition},
                                }).collect())
                            })();
                            if reply.send(result).is_err() { failed=true; audited_transport_failure=false;let _=arbiter.storage_failed(); }
                        }
                        Message::SceneUpdate(controller,generation,update,reply) => {
                            let result = if !authorize(controller) { Err(WriteStatus::InsufficientPrivileges) }
                            else if generation.boot != worker_boot || (matches!(&update,SceneUpdate::BeginScoped{..}) && (failed || generation.epochs != arbiter.policy().epochs)) { Err(WriteStatus::ServiceCommunicationFailure) }
                            else {
                                let stored = match update {
                                    SceneUpdate::BeginScoped{run,definition,targets} => {
                                        if let Some(g)=&gateways {
                                            match g.device_requirements(&configuration,targets) {
                                                Ok(wires)=>store.begin_scene_run_scoped(&worker_boot,&run,generation.epochs.configuration,definition.as_deref(),millis(clock),&homesense_storage::gateway_audit::OperationScope {
                                                    key:homesense_storage::gateway_audit::OperationKey {kind:homesense_storage::gateway_audit::OperationKind::Scene,id:run.clone()},global:generation.epochs,wires,
                                                }).map(|()|0),
                                                Err(status)=>{let _=reply.send(Err(status));continue;},
                                            }
                                        } else {store.begin_scene_run(&worker_boot,&run,generation.epochs.configuration,definition.as_deref(),millis(clock)).map(|()|0)}
                                    },
                                    SceneUpdate::Expiry{run,expected,expiry} => store.checkpoint_scene_run_with_expiry(&worker_boot,&run,expected,homesense_storage::scene_run::Status::Running,millis(clock),&expiry),
                                    SceneUpdate::Checkpoint{run,expected,status,cancellation} => store.checkpoint_scene_run_with_cancellation(&worker_boot,&run,expected,status,millis(clock),cancellation.as_ref()),
                                };
                                stored.map_err(|error| match error {
                                    homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit => WriteStatus::InvalidValueInRequest,
                                    _ => { failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure }
                                })
                            };
                            let _=reply.send(result);
                        }
                        Message::ManualFence(reply) => { let _ = reply.send(next_id); }
                        Message::ManualSuperseded(device, fence, reply) => { let _ = reply.send(manual_versions.get(&device).is_some_and(|version| *version >= fence)); }
                        Message::RuleOutcome(controller,generation,correlation,error,reply)=>{
                            let result=if !authorize(controller){Err(WriteStatus::InsufficientPrivileges)}
                            // Terminal audit remains writable after transport closure; it admits no work.
                            else if generation.boot!=worker_boot || generation.epochs.configuration!=configuration.revision(){Err(WriteStatus::ServiceCommunicationFailure)}
                            else{store.finish_rule_chain(&worker_boot,configuration.revision(),correlation,millis(clock),error.as_deref()).map_err(|error|match error{
                                homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                _=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                            })};
                            let _=reply.send(result);
                        }
                        Message::RuleChain(controller,generation,correlation,after,limit,reply)=>{
                            let result=if !authorize(controller){Err(WriteStatus::InsufficientPrivileges)}
                            else if generation.boot!=worker_boot{Err(WriteStatus::ServiceCommunicationFailure)}
                            else{store.rule_chain(&worker_boot,correlation,after,limit).map_err(|_|WriteStatus::ServiceCommunicationFailure)};
                            let _=reply.send(result);
                        }
                        Message::MotorReceipts(controller,generation,device,action,ids,reply)=>{
                            let result=(|| {
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure)}
                                let command=configuration.motor_command(&device,action).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                ids.into_iter().map(|id|store.motor_receipt(&worker_boot,id,&device,command,generation.epochs).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ResourceDoesNotExist)).collect()
                            })();let _=reply.send(result);
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
                                _ => { failed = true; audited_transport_failure=false; WriteStatus::ServiceCommunicationFailure }
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
                                    _=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
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
                        Message::BeginSceneObservation(controller,generation,fence,proof,reply)=>{
                            if reply.is_closed() {continue;}
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure)}
                                let _lease=proof.lease().lock(observer_boot,configuration.revision(),arbiter.policy().epochs.transport,now).map_err(|_|WriteStatus::ResourceBusy)?;
                                let bundle=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ResourceDoesNotExist)?;
                                let document=bundle.automation.ok_or(WriteStatus::ResourceDoesNotExist)?;
                                if worker_observation_invalidation.load(Ordering::Acquire)!=fence {return Err(WriteStatus::ResourceBusy)}
                                let scope=scene_observer.begin(&store,&document,proof.snapshot(),observer_boot,arbiter.policy().epochs,now).map_err(|error|match error {
                                    homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                    _=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                                })?;
                                if worker_observation_invalidation.load(Ordering::Acquire)!=fence {scene_observer.invalidate();return Err(WriteStatus::ResourceBusy)}
                                observation_fence=fence;Ok(scope)
                            })();let _=reply.send(result);
                        }
                        Message::ObserveSceneKey(controller,generation,scope,proof,reply)=>{
                            if reply.is_closed() {continue;}
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs || !arbiter.policy().gateway_ready || arbiter.policy().publishing {return Err(WriteStatus::ServiceCommunicationFailure)}
                                if worker_observation_invalidation.load(Ordering::Acquire)!=observation_fence {scene_observer.invalidate();return Err(WriteStatus::InvalidValueInRequest)}
                                let _lease=proof.lease().lock(observer_boot,configuration.revision(),arbiter.policy().epochs.transport,now).map_err(|_|WriteStatus::ResourceBusy)?;
                                scene_observer.observe(&mut store,scope,proof.event(),now).map_err(|error|match error {
                                    homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,
                                    _=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure}
                                })
                            })();let _=reply.send(result);
                        }
                        Message::InitializeSceneSelection(controller,generation,prepared,reply)=>{
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if !prepared.owned_by(controller,&generation) {return Err(WriteStatus::InvalidValueInRequest);}
                                let policy=arbiter.policy();
                                if generation.boot!=worker_boot || generation.epochs!=policy.epochs {return Err(WriteStatus::InvalidValueInRequest);}
                                if failed || worker_stop.load(Ordering::Acquire) || policy.publishing || !policy.gateway_ready || !policy.controller || !policy.storage_healthy || !policy.valid_configuration_loaded || in_flight.is_some() || arbiter.queued_len()!=0 {return Err(WriteStatus::ResourceBusy);}
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|bundle|bundle.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                prepared.commit(&mut store,&configuration,&document,&worker_boot,policy.epochs,millis(clock))
                            })();
                            if result==Err(WriteStatus::ServiceCommunicationFailure) {failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();}
                            let _=reply.send(result);
                        }
                        Message::AwayOff(revision,generation,lease,deadline,submit,reply)=>{
                            if reply.is_closed(){continue;}
                            let result=(||{
                                let controller=lease.controller();
                                if !authorize(controller)||!lease.valid(){return Err(WriteStatus::InsufficientPrivileges);}
                                let policy=arbiter.policy();
                                let now=millis(clock);
                                if failed || worker_stop.load(Ordering::Acquire) || generation.boot!=worker_boot || generation.epochs!=policy.epochs || revision!=configuration.revision() || policy.publishing || !policy.gateway_ready || !policy.controller || !policy.storage_healthy || !policy.valid_configuration_loaded || now>=deadline {return Err(WriteStatus::ResourceBusy);}
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                let key_binding=lease.key_binding(&configuration).map_err(|_|WriteStatus::InsufficientPrivileges)?;
                                let indicator=lease.indicator_lease(&configuration).map_err(|_|WriteStatus::InsufficientPrivileges)?;
                                let targets=crate::away_off::plan(&configuration,&document,lease.clone(),options.context).map_err(|_|WriteStatus::InsufficientPrivileges)?;
                                let count=targets.len()+usize::from(indicator.is_some());
                                if count>128 {return Err(WriteStatus::ResourceBusy);}
                                if !submit{
                                    if let Some(g)=&gateways {
                                        let mut required=targets.iter().map(|target|target.plan.modules()[target.index].gateway_id().to_owned()).collect::<Vec<_>>();
                                        if let Some(key)=&key_binding {required.push(configuration.binding(&key.endpoint).ok_or(WriteStatus::InvalidValueInRequest)?.0.gateway.clone());}
                                        g.table.capture(required).map_err(|_|WriteStatus::ResourceBusy)?;
                                    }
                                    return Ok(vec![]);
                                }
                                let supersede=matches!(document.away_off.as_ref().map(|p|p.active_execution_policy),Some(homesense_config::rule_document::AwayExecutionPolicy::CancelAffectedLighting));
                                if !supersede && (in_flight.is_some() || store.has_unsettled_control_commands(&worker_boot,revision).map_err(|_|WriteStatus::ServiceCommunicationFailure)? || store.has_active_scene_runs(&worker_boot,revision).map_err(|_|WriteStatus::ServiceCommunicationFailure)?) {return Err(WriteStatus::ResourceBusy);}
                                let affected=targets.iter().flat_map(|t|t.plan.modules()[t.index].devices().iter().cloned()).collect::<std::collections::BTreeSet<_>>();
                                let cost=(count as u64).checked_mul(options.write_interval.as_millis() as u64).ok_or(WriteStatus::InvalidValueInRequest)?;
                                if now.checked_add(cost).is_none_or(|end|end>=deadline){return Err(WriteStatus::ResourceBusy);}
                                let end=next_id.checked_add(count as u64).ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let modules_end=next_id+targets.len() as u64;
                                let module_commands=targets.iter().enumerate().map(|(offset,_)|{
                                    let id=next_id+offset as u64;
                                    (QueuedCommand{id,device_id:crate::away_off::receipt_device(id),command:homesense_core::Command::ModuleAllOff,origin:targets[0].lease.origin(),epochs:policy.epochs,deadline_ms:deadline,run_id:None},Access{authorized:true,device_exists:true,device_verified:true,existing_direct_manual:false})
                                }).collect::<Vec<_>>();
                                if module_commands.iter().any(|(c,_)|configuration.devices().contains_key(&c.device_id)){return Err(WriteStatus::InvalidValueInRequest);}
                                let reset=indicator.as_ref().map(|(endpoint,_)|(QueuedCommand{id:modules_end,device_id:endpoint.clone(),command:homesense_core::Command::PanelLed(false),origin:lease.origin(),epochs:policy.epochs,deadline_ms:deadline,run_id:None},Access{authorized:true,device_exists:true,device_verified:true,existing_direct_manual:false}));
                                let mut commands=module_commands.clone();commands.extend(reset.clone());
                                let effects=targets.iter().enumerate().map(|(n,t)|t.effects(next_id+n as u64)).collect::<Vec<_>>();
                                for target in &targets {target.frame(&configuration,options.context,policy.epochs.transport).map_err(|_|WriteStatus::InvalidValueInRequest)?;}
                                let gateway_scopes=if let Some(g)=&gateways {
                                    let routes=effects.iter().map(|effect|(effect.command,effect.gateway_id.clone())).collect::<BTreeMap<_,_>>();
                                    let mut requirements=g.table.capture(routes.values().cloned()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.wires().to_vec();
                                    if let Some(key)=&key_binding {requirements.extend(g.device_requirements(&configuration,vec![key.endpoint.clone()])?);}
                                    Some(g.scope(&configuration,&commands.iter().map(|(c,_)|c.clone()).collect::<Vec<_>>(),&routes,&requirements,vec![])? )
                                }else{None};
                                let persist=|commands:&[QueuedCommand],cancelled:&[homesense_core::queue::Cancellation]| {
                                    match &gateway_scopes {Some(scopes)=>store.accept_queue_batch_scoped(&worker_boot,commands,cancelled,now,None,&[],None,None,&[],&[],None,&effects,scopes),None=>store.accept_module_off_commands(&worker_boot,commands,cancelled,now,&effects)}
                                };
                                let accepted=lease.with_valid(||if supersede {arbiter.submit_module_off_with_reset_committed(module_commands,reset,&affected,now,persist)} else {arbiter.submit_batch_committed(commands,now,persist)}).ok_or(WriteStatus::InsufficientPrivileges)?;
                                match accepted {
                                    Ok(cancelled)=>{for c in cancelled {principals.remove(&c.command.id);}},
                                    Err(BatchCommitError::Admission(_))=>return Err(WriteStatus::ResourceBusy),
                                    Err(BatchCommitError::Persistence(_))=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();return Err(WriteStatus::ServiceCommunicationFailure);}
                                }
                                let ids=(next_id..end).collect::<Vec<_>>();
                                if let Some((_,panel_lease))=indicator {
                                    panel_leases.insert(modules_end,panel_lease);principals.insert(modules_end,controller);
                                    away_reset_dependencies.insert(modules_end,(next_id..modules_end).collect());
                                }
                                for (id,target) in (next_id..modules_end).zip(targets) {
                                    for device in target.plan.modules()[target.index].devices() {
                                        manual_versions.insert(device.clone(),end);
                                        restore_policy.invalidate_device(device);
                                    }
                                    principals.insert(id,controller);module_off_jobs.insert(id,target);
                                }
                                next_id=end;Ok(ids)
                            })();let _=reply.send(result);
                        }
                        Message::ModuleQuery(controller,request_source,generation,module_id,scope,deadline,submit,reply)=>{
                            let result=(||{
                                if reply.is_closed() || Instant::now()>=deadline {return Err(WriteStatus::ResourceBusy);}
                                if request_source.as_ref().is_some_and(|source|!source.valid_for(controller,configuration.revision())) {return Err(WriteStatus::InsufficientPrivileges);}
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                let policy=arbiter.policy();
                                if failed || worker_stop.load(Ordering::Acquire) || generation.boot!=worker_boot || generation.epochs!=policy.epochs || policy.publishing || !policy.gateway_ready || !policy.controller || !policy.storage_healthy || !policy.valid_configuration_loaded {return Err(WriteStatus::ResourceBusy);}
                                let targets=crate::module_queries::plan(&configuration,options.context,&module_id,&scope).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                let now=millis(clock);
                                let end=next_id.checked_add(targets.len() as u64).ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let deadline_ms=now.checked_add(u64::try_from(deadline.saturating_duration_since(Instant::now()).as_millis()).map_err(|_|WriteStatus::InvalidValueInRequest)?).ok_or(WriteStatus::InvalidValueInRequest)?;
                                let commands=targets.iter().enumerate().map(|(offset,_)|{
                                    let id=next_id+offset as u64;
                                    (QueuedCommand{id,device_id:crate::module_queries::receipt_device(id),command:homesense_core::Command::Query,origin:Origin::Background,epochs:policy.epochs,deadline_ms,run_id:None},Access{authorized:true,device_exists:true,device_verified:true,existing_direct_manual:false})
                                }).collect::<Vec<_>>();
                                if commands.iter().any(|(c,_)|configuration.devices().contains_key(&c.device_id)) {return Err(WriteStatus::InvalidValueInRequest);}
                                let mut accepted=vec![];
                                if submit {
                                    let gateway_scopes=if let Some(g)=&gateways {
                                        let routes=targets.iter().enumerate().map(|(n,t)|configuration.module(&t.module_id).map(|(gateway,_)|(next_id+n as u64,gateway.gateway.to_owned())).ok_or(WriteStatus::InvalidValueInRequest)).collect::<Result<BTreeMap<_,_>,_>>()?;
                                        Some(g.scope(&configuration,&commands.iter().map(|(c,_)|c.clone()).collect::<Vec<_>>(),&routes,&[],vec![])? )
                                    }else{None};
                                    match crate::source_auth::commit(request_source.as_ref(),||arbiter.submit_batch_committed(commands,now,|commands,cancelled| {
                                        match &gateway_scopes {Some(scopes)=>store.accept_queue_batch_scoped(&worker_boot,commands,cancelled,now,None,&[],None,None,&[],&[],None,&[],scopes),None=>store.accept_queue_batch(&worker_boot,commands,cancelled,now)}
                                    })).ok_or(WriteStatus::InsufficientPrivileges)? {
                                        Ok(cancelled)=>{for c in cancelled {principals.remove(&c.command.id);}},
                                        Err(BatchCommitError::Admission(_))=>return Err(WriteStatus::ResourceBusy),
                                        Err(BatchCommitError::Persistence(_))=>{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();return Err(WriteStatus::ServiceCommunicationFailure);}
                                    }
                                    for (id,target) in (next_id..end).zip(targets) {principals.insert(id,controller);if let Some(source)=&request_source {source_leases.insert(id,source.clone());}module_queries.insert(id,target);accepted.push(id.to_string());}
                                    next_id=end;
                                }
                                Ok(crate::module_queries::Accepted{boot:worker_boot.clone(),revision:configuration.revision().to_string(),transport_epoch:policy.epochs.transport.to_string(),commands:accepted})
                            })();let _=reply.send(result);
                        }
                        Message::ModuleQueryRecords(controller,generation,ids,reply)=>{
                            let result=if !authorize(controller){Err(WriteStatus::InsufficientPrivileges)}
                                else if generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {Err(WriteStatus::InvalidValueInRequest)}
                                else {store.module_query_records(&worker_boot,configuration.revision(),&ids).map_err(|_|WriteStatus::ServiceCommunicationFailure)};
                            let _=reply.send(result);
                        }
                        Message::RecentDeviceCommands(controller,generation,ids,limit,reply)=>{
                            let result=(||{
                                if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs{return Err(WriteStatus::ServiceCommunicationFailure);}
                                store.recent_device_commands(&worker_boot,configuration.revision(),&ids,limit).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();let _=reply.send(result);
                        }
                        Message::NativeStatus(controller,generation,key,reply)=>{
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if generation.boot!=worker_boot {return Err(WriteStatus::InvalidValueInRequest);}
                                store.expire_native_scenes(&worker_boot,arbiter.policy().epochs,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                let mut row=store.native_scene_request(&controller.to_string(),&key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                if let Some(current)=row.as_ref().filter(|r|r.status==homesense_storage::native_scene::Status::Pending) {
                                    let version=store.logical_variable(current.request.epochs.configuration,&current.request.variable).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                    let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                    let group=document.scene_switch_groups.iter().find(|g|g.id==current.request.group).ok_or(WriteStatus::InvalidValueInRequest)?;
                                    let targets=group.absolute_light_targets(&document.timed_scenes).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                    let physical=targets.values().flat_map(|targets|targets.keys()).flat_map(|id|std::iter::once(id).chain(configuration.binding(id).and_then(|(d,_)|d.supply_relay.as_ref())));
                                    if version.is_none_or(|v|v.version!=current.request.variable_version)
                                        || (current.fence>0 && physical.into_iter().any(|id|manual_versions.get(id).is_some_and(|v|*v>=current.fence))) {
                                        if let Some(first)=current.initial_commands.first() {
                                            arbiter.cancel_run_committed((1u64<<62)+first,|items|store.cancel_queued(&worker_boot,items,millis(clock))).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                        }
                                        store.fail_native_scene(&controller.to_string(),&key,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                        row=store.native_scene_request(&controller.to_string(),&key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                    }
                                }
                                Ok(row)
                            })();let _=reply.send(result);
                        }
                        Message::NativeFail(controller,generation,key,reply)=>{
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                if generation.boot!=worker_boot {return Err(WriteStatus::InvalidValueInRequest);}
                                if let Some(row)=store.native_scene_request(&controller.to_string(),&key).map_err(|_|WriteStatus::ServiceCommunicationFailure)? && let Some(first)=row.initial_commands.first() {
                                    arbiter.cancel_run_committed((1u64<<62)+first,|items|store.cancel_queued(&worker_boot,items,millis(clock))).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                }
                                store.fail_native_scene(&controller.to_string(),&key,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)
                            })();let _=reply.send(result);
                        }
                        Message::NativeFinish(controller,request_source,completion,reply)=>{
                            let result=(||{
                                if request_source.as_ref().is_some_and(|source|!source.valid_for(controller,configuration.revision())) {return Err(WriteStatus::InsufficientPrivileges);}
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                let policy=arbiter.policy();let now=millis(clock);
                                if failed || worker_stop.load(Ordering::Acquire) || !policy.gateway_ready || policy.publishing || !policy.controller || !policy.storage_healthy || completion.generation.boot!=worker_boot || completion.generation.epochs!=policy.epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                let row=store.native_scene_request(&controller.to_string(),&completion.request.key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::InvalidValueInRequest)?;
                                if row.request!=completion.request || row.status!=homesense_storage::native_scene::Status::Pending || row.phase!=completion.phase || now>=row.request.deadline_ms || completion.physical.iter().any(|id|manual_versions.get(id).is_some_and(|v|*v>=row.fence)) || completion.levels.iter().any(|(id,command,_)|!row.initial_commands.contains(command) || configuration.binding(id).is_none_or(|(d,m)|d.supply_relay.is_some() || m.kind()!=homesense_core::ModuleKind::Dimmer)) {return Err(WriteStatus::InvalidValueInRequest);}
                                if store.native_written_query_devices(&row.request,&row.commands).map_err(|_|WriteStatus::ServiceCommunicationFailure)?!=Some(completion.physical.clone()) {return Err(WriteStatus::ResourceBusy);}
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                let (selected,_)=completion.projection.validate(&store,&configuration,&document,&worker_boot,policy.epochs,now)?;
                                crate::source_auth::commit(request_source.as_ref(),||store.finish_native_scene(&row.request,row.phase,selected,&completion.levels,now)).ok_or(WriteStatus::InsufficientPrivileges)?.map_err(|e|match e {homesense_storage::Error::Conflict|homesense_storage::Error::InvalidAudit=>WriteStatus::InvalidValueInRequest,_=>WriteStatus::ServiceCommunicationFailure})?;
                                scene_projections.insert(completion.projection.variable().to_owned(),completion.projection);
                                Ok(())
                            })();if result==Err(WriteStatus::ServiceCommunicationFailure){failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();}let _=reply.send(result);
                        }
                        Message::ReconcileSceneSelection(controller,generation,prepared,reply)=>{
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                let policy=arbiter.policy();
                                if !prepared.owned_by(controller,&generation) || generation.boot!=worker_boot || generation.epochs!=policy.epochs {return Err(WriteStatus::InvalidValueInRequest);}
                                if failed || worker_stop.load(Ordering::Acquire) || policy.publishing || !policy.gateway_ready || !policy.controller || !policy.storage_healthy || !policy.valid_configuration_loaded {return Err(WriteStatus::ResourceBusy);}
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                prepared.validate(&store,&configuration,&document,&worker_boot,policy.epochs,millis(clock))?;
                                scene_projections.retain(|name,_|document.scene_switch_groups.iter().any(|g|g.variable==*name && g.reconciliation.as_ref().is_some_and(|p|p.enabled)));
                                scene_projections.insert(prepared.variable().to_owned(),prepared);
                                Ok(())
                            })();
                            if result==Err(WriteStatus::ServiceCommunicationFailure) {failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();}
                            let _=reply.send(result);
                        }
                        Message::SceneSelections(controller,generation,names,reply)=>{
                            let result=(||{
                                if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges)}
                                if failed || generation.boot!=worker_boot || generation.epochs!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure)}
                                if worker_observation_invalidation.load(Ordering::Acquire)!=observation_fence {scene_observer.invalidate();}
                                store.expire_native_scenes(&worker_boot,arbiter.policy().epochs,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation);
                                let policy=arbiter.policy();
                                let ready=!failed && !worker_stop.load(Ordering::Acquire) && !policy.publishing && policy.gateway_ready && policy.controller && policy.storage_healthy && policy.valid_configuration_loaded;
                                let mut values=BTreeMap::new();
                                for name in names {
                                    let mut value=store.scene_selection(configuration.revision(),&name,scene_observer.scope()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ResourceDoesNotExist)?;
                                    if let Some(document)=&document && let Some(group)=document.scene_switch_groups.iter().find(|g|g.variable==name) {
                                        crate::scene_reconciliation::project(&mut value,scene_projections.get(&name),group,&store,&configuration,document,&worker_boot,policy.epochs,millis(clock),ready)?;
                                    }
                                    if values.insert(name,value).is_some() {return Err(WriteStatus::InvalidValueInRequest)}
                                }
                                Ok(values)
                            })();let _=reply.send(result);
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
                        Message::BeginPublication(document, rules, expected, deadline, request_source, reply) => {
                            if reply.is_closed() { continue; }
                            let result = (|| {
                                if request_source.as_ref().is_some_and(|source|!authorize(source.controller()) || !source.matches(&document,rules.as_deref(),expected)) {return Err(WriteStatus::InsufficientPrivileges);}
                                if failed || arbiter.policy().publishing || expected != configuration.revision() {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let candidate = homesense_config::Configuration::parse(&document, options.context)
                                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
                                if candidate.revision() <= expected { return Err(WriteStatus::InvalidValueInRequest); }
                                if store.active().map_err(|_| WriteStatus::ServiceCommunicationFailure)?.as_ref() != Some(&durable_configuration) {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                if let Some(rules) = &rules {
                                    let automation=crate::configuration_publish::validate_automation(&candidate,rules,options.context).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                    for (name, variable) in &automation.variables {
                                        if variable.lifetime == homesense_core::rule::Lifetime::Persistent {
                                            let old=store.logical_variable(configuration.revision(),name).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                            automation.published_variable_value(Some(configuration.revision()),name,old.as_ref().map(|v|(v.kind,v.value.as_ref()))).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                        }
                                    }
                                } else if store.automation_payload(configuration.revision()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.is_some() {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                let enter=|| {
                                let cancelled = arbiter.begin_publication(millis(clock), deadline)
                                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?;
                                if store.cancel_queued(&worker_boot, &cancelled, millis(clock)).is_err() {
                                    failed = true; audited_transport_failure=false;
                                    let _ = arbiter.storage_failed();
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                for c in &cancelled { principals.remove(&c.command.id); }
                                let stops=arbiter.publication_stops().map(|command|crate::publication_auth::DrainStop::capture(command.clone(),arbiter.policy().epochs.control).map(|lease|(command.id,lease)).ok_or(WriteStatus::ServiceCommunicationFailure)).collect::<Result<BTreeMap<_,_>,_>>()?;
                                publication = Some(PendingPublication { document, rules, cancellations: cancelled, stops });
                                Ok(())
                                };
                                // Linearization point: source mutation before this
                                // guard rejects entry; after it, finish STOP drain
                                // and atomic commit without a second auth abort.
                                match &request_source {Some(source)=>source.with_valid(enter).ok_or(WriteStatus::InsufficientPrivileges)?,None=>enter()}
                            })();
                            let _ = reply.send(result);
                        }
                        Message::AbortPublication(reply) => {
                            let result = (|| {
                                let cancelled = arbiter.abort_before_commit().map_err(|_| WriteStatus::ResourceBusy)?;
                                publication = None;
                                if store.cancel_queued(&worker_boot, &cancelled, millis(clock)).is_err() {
                                    failed = true; audited_transport_failure=false;
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
                                let attempted = if let Some(rules)=&pending.rules {
                                    store.publish_automation(Some(configuration.revision()), &pending.document, rules, options.context, &audit).map(|b|b.configuration)
                                } else {
                                    store.publish_configuration(Some(configuration.revision()), &pending.document, options.context, &audit)
                                };
                                // Read back durable truth even on transaction failure. Never
                                // infer rollback, restore a queue, or retry a physical action.
                                let current = store.active();
                                let committed = current.as_ref().ok().and_then(|v| v.as_ref()).filter(|v| v.payload == pending.document && store.automation_payload(v.revision).ok().is_some_and(|rules|rules==pending.rules));
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
                                failed = true; audited_transport_failure=false;
                                let _ = arbiter.storage_failed();
                                Err(WriteStatus::ServiceCommunicationFailure)
                            })();
                            let _ = reply.send(result);
                        }

                        Message::ReceivedProjection(wire,written,received,received_ms,snapshot,reply)=>{
                            let result=(||{
                                let now=millis(clock);
                                let policy=arbiter.policy();
                                if failed || worker_stop.load(Ordering::Acquire) || !policy.storage_healthy {return Err(WriteStatus::ServiceCommunicationFailure);}
                                // A publication handoff consumes old bounded readers but cannot
                                // use their projection to reopen its global admission gate.
                                if policy.publishing || !policy.controller || !policy.valid_configuration_loaded || !policy.gateway_ready {return Ok(());}
                                let Some(g)=&gateways else{return Ok(());};
                                if !g.table.ready(&wire){return Ok(());}
                                if written.len()>256 || received.len()>256 || snapshot.cursor.boot_id!=observer_boot || received_ms>now {return Err(WriteStatus::InvalidValueInRequest);}
                                restore_policy.targets(policy.epochs,now,|id|store.command_status(&worker_boot,id)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                let written=written.into_iter().collect::<std::collections::BTreeSet<_>>();
                                for id in received {
                                    let Some((device,module))=configuration.binding(&id) else{return Err(WriteStatus::InvalidValueInRequest);};
                                    if device.gateway!=wire.gateway() || module.kind()!=homesense_core::ModuleKind::Dimmer {continue;}
                                    let Some((target,commands,scope))=restore_policy.owner(&id) else {continue;};
                                    if scope!=policy.epochs || commands.is_empty() || commands.iter().any(|id|!written.contains(id)){continue;}
                                    let Some(state)=snapshot.devices.get(&id).filter(|state|state.quality==homesense_state::Quality::Confirmed && state.observed_at_ms==Some(received_ms) && now.checked_sub(received_ms).is_some_and(|age|age<5000)) else{continue;};
                                    if state.observed!=Some(homesense_core::Observation::Level(target)){continue;}
                                    let mut completed=true;
                                    for command in &commands {
                                        completed&=matches!(store.command_status(&worker_boot,*command).map_err(|_|WriteStatus::ServiceCommunicationFailure)?,Some(homesense_storage::journal::Status::Written|homesense_storage::journal::Status::Observed))
                                            && store.command_wire_scope(&worker_boot,*command).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_ref()==Some(&wire);
                                    }
                                    if !completed{continue;}
                                    if let Some(relay)=&device.supply_relay {
                                        let Some(supply)=snapshot.devices.get(relay).filter(|state|state.quality==homesense_state::Quality::Confirmed && state.observed==Some(homesense_core::Observation::Switch(true)) && state.observed_at_ms.and_then(|at|now.checked_sub(at)).is_some_and(|age|age<5000)) else{continue;};
                                        if !store.feedback_is_settled_in_wire_scope(&worker_boot,relay,supply.observed_at_ms,policy.epochs,&wire).map_err(|_|WriteStatus::ServiceCommunicationFailure)?{continue;}
                                    }
                                    restore_policy.reached_after_written(&id,target,&commands,scope,now);
                                }
                                remember_projection(&mut store,&mut restore_policy,&configuration,&worker_boot,policy,now,(&snapshot,5000)).map(|_|())
                            })();
                            if result==Err(WriteStatus::ServiceCommunicationFailure){failed=true;audited_transport_failure=false;let _=arbiter.storage_failed();}
                            let _=reply.send(result);
                        }
                        Message::Remember(revision, snapshot, max_age, reply) => {
                            let result = (|| {
                                if revision != configuration.revision() || arbiter.policy().publishing {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                remember_projection(&mut store,&mut restore_policy,&configuration,&worker_boot,arbiter.policy(),now,(&snapshot,max_age))
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
                            let fault_state=arbiter.storage_failed();
                            principals.clear();
                            // Execute the journal audit even if in-memory fault handling failed.
                            // Only both completed parts authorize a recoverable transport handoff.
                            let result = transport_fault_audit_result(
                                fault_state,
                                store.fail_transport(&worker_boot, millis(clock)),
                            );
                            audited_transport_failure = result.is_ok();
                            let _ = reply.send(result);
                        }
                        Message::Shutdown(reply) => {
                            stopped = Some(reply);
                            break;
                        }
                        Message::Recover(epoch, revision, reply) => {
                            if reply.is_closed() { continue; }
                            // A failed recovery read/audit is not a successfully
                            // audited transport outage, even after an earlier one.
                            audited_transport_failure=false;
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
                                if arbiter.policy().publishing {
                                    arbiter.publication_recovered(epoch, revision).map_err(|_| failure)?;
                                } else {
                                    arbiter.transport_reconnected(epoch).map_err(|_| failure)?;
                                    arbiter.gateway_resynchronized(epoch, revision).map_err(|_| failure)?;
                                    arbiter.storage_recovered(revision).map_err(|_| failure)?;
                                }
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
                        Message::Dispatch(available,reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            if failed || in_flight.is_some() {
                                let _ = reply.send(Err(WriteStatus::ResourceBusy));
                                continue;
                            }
                            let now = millis(clock);
                            let result = (|| -> Result<Option<DispatchTicket>, String> {
                                let reset_ready=away_reset_dependencies.iter().map(|(reset,ids)| {
                                    let mut ready=true;for id in ids {ready &= matches!(store.command_status(&worker_boot,*id).map_err(|e|e.to_string())?,Some(Status::Written|Status::Observed));}
                                    Ok((*reset,ready))
                                }).collect::<Result<BTreeMap<_,_>,String>>()?;
                                let allowed=if motor_guards.is_empty(){vec![]}else{store.active_bundle(options.context).map_err(|e|e.to_string())?.and_then(|b|b.automation).ok_or("motor guard document missing")?.enabled_motor_requests().map_err(|e|e.to_string())?};
                                let scope=homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs};
                                let valid:BTreeMap<_,_>=motor_guards.iter_mut().map(|(group,guard)|{
                                    let authorized=authorize(guard.controller());
                                    (*group,guard.check(scope,&allowed,&mut curtains,now,authorized))
                                }).collect();
                                let draining=arbiter.policy().publishing && publication.is_some();
                                let access = |command: &QueuedCommand| {
                                        // Accepted publication owns only the immutable
                                        // reserved STOPs. Other queued work retains its
                                        // live source check and cannot use this path.
                                        let handoff_stop=draining && publication.as_ref().and_then(|p|p.stops.get(&command.id)).is_some_and(|lease|lease.matches_reserved(command));
                                        let binding = configuration.binding(&command.device_id);
                                        Access {
                                            authorized: handoff_stop || (principals
                                                .get(&command.id)
                                                .is_some_and(|id| authorize(*id)) && source_leases.get(&command.id).is_none_or(|lease|lease.valid()) && panel_leases.get(&command.id).is_none_or(|lease|lease.valid()) && module_off_jobs.get(&command.id).is_none_or(|job|job.lease.valid()) && motor_guard_ids.get(&command.id).is_none_or(|group|valid.get(group)==Some(&true)) && curtains.permits_queued_hint(command,now)),
                                            device_exists: binding.is_some() || module_off_jobs.contains_key(&command.id) || module_queries.get(&command.id).is_some_and(|q|q.frame(&configuration,options.context,command.epochs.transport).is_ok()),
                                            device_verified: module_off_jobs.contains_key(&command.id) || module_queries.contains_key(&command.id) || binding.is_some_and(|(_, m)| {
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
                                    arbiter.dispatch_eligible(now,access,|command| {
                                        let gateway=if let Some(target)=module_off_jobs.get(&command.id) {
                                            Some(target.plan.modules()[target.index].gateway_id().to_owned())
                                        }else if let Some(target)=module_queries.get(&command.id) {
                                            configuration.module(&target.module_id).map(|(g,_)|g.gateway.clone())
                                        }else{configuration.binding(&command.device_id).map(|(d,_)|d.gateway.clone())};
                                        reset_ready.get(&command.id).is_none_or(|ready|*ready) && gateway.is_none_or(|gateway|available.as_ref().is_none_or(|ids|ids.contains(&gateway)))
                                    }).map_err(|e| e.to_string())?
                                };
                                if !dispatch.cancelled.is_empty() {
                                    store.cancel_queued(&worker_boot, &dispatch.cancelled, now)
                                        .map_err(|e| e.to_string())?;
                                }
                                for cancelled in &dispatch.cancelled {
                                    curtains.cancelled(cancelled.command.id,cancelled.reason);
                                    principals.remove(&cancelled.command.id);
                                }
                                for (command, _) in &dispatch.rejected {
                                    curtains.cancelled(command.id,homesense_core::queue::CancelReason::RunCancelled);
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
                                // Projection LEDs are disposable, unlike actuator/STOP
                                // operations. Do not acquire a Writing obligation at the
                                // expiry edge: the bounded worker and completion audit
                                // need one existing wire-gap quantum. This cancels only
                                // still-Accepted work, never extends or replays its lease.
                                let current=millis(clock);
                                if matches!(command.command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff)
                                    && current.saturating_add(options.write_interval.as_millis() as u64)>=command.deadline_ms {
                                    store.transition_command(&worker_boot,command.id,Status::Accepted,Status::Cancelled,current).map_err(|e|e.to_string())?;
                                    arbiter.cancel_selected_before_write(command.id).map_err(|e|e.to_string())?;
                                    principals.remove(&command.id);
                                    return Ok(None);
                                }
                                let physical_wire=if let Some(g)=&gateways {
                                    let wire=store.command_wire_scope(&worker_boot,command.id).map_err(|e|e.to_string())?.ok_or("missing mandatory command wire")?;
                                    if !g.table.ready(&wire) {return Err("command wire is offline or changed".into());}
                                    Some(wire)
                                }else{None};
                                let frame_epoch=physical_wire.as_ref().map_or(command.epochs.transport,|wire|wire.epoch());
                                let (gateway_id,frame) = if let Some(target)=module_off_jobs.get(&command.id) {
                                    if command.command!=homesense_core::Command::ModuleAllOff {return Err("module control ticket mismatch".into());}
                                    target.frame(&configuration,options.context,frame_epoch)?
                                } else if let Some(target)=module_queries.get(&command.id) {
                                    if command.command!=homesense_core::Command::Query {return Err("read ticket command mismatch".into());}
                                    target.frame(&configuration,options.context,frame_epoch)?
                                } else {
                                    let (device,module)=configuration.binding(&command.device_id).ok_or("missing binding")?;
                                    (device.gateway.clone(),hb_transport::PreparedFrame::prepare(module,device.channel,command.command,options.context,frame_epoch).map_err(|e|e.to_string())?)
                                };
                                let publication_stop=if draining {
                                    let lease=publication.as_mut().and_then(|p|p.stops.remove(&command.id)).ok_or("missing reserved STOP authority")?;
                                    if !lease.matches_write(&command){return Err("reserved STOP write binding changed".into());}
                                    Some(lease)
                                }else{None};
                                let source_authorization=if publication_stop.is_some(){None}else{source_leases.get(&command.id).cloned()};
                                if source_authorization.as_ref().is_some_and(|source|!source.valid_for(principals[&command.id],configuration.revision())) {
                                    store.transition_command(&worker_boot,command.id,Status::Accepted,Status::Cancelled,now).map_err(|e|e.to_string())?;
                                    arbiter.cancel_selected_before_write(command.id).map_err(|e|e.to_string())?;
                                    principals.remove(&command.id);return Ok(None);
                                }
                                let panel_authorization=panel_leases.get(&command.id).cloned();
                                if panel_authorization.as_ref().is_some_and(|lease|!lease.valid_for(&configuration,principals[&command.id],&command.device_id,command.command)) {
                                    store.transition_command(&worker_boot,command.id,Status::Accepted,Status::Cancelled,now).map_err(|e|e.to_string())?;
                                    arbiter.cancel_selected_before_write(command.id).map_err(|e|e.to_string())?;
                                    principals.remove(&command.id);return Ok(None);
                                }
                                if physical_wire.as_ref().is_some_and(|wire|wire.gateway()!=gateway_id) {return Err("persisted wire route changed".into());}
                                let audit_started=millis(clock);
                                let reservation=store.reserve_command_write(&worker_boot,command.id,now,physical_wire.as_ref()).map_err(|e|e.to_string())?;
                                let Some(reservation)=reservation else {
                                    arbiter.cancel_selected_before_write(command.id).map_err(|e|e.to_string())?;
                                    principals.remove(&command.id);
                                    return Ok(None);
                                };
                                let audited=millis(clock);
                                if matches!(command.command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff) && audited>=command.deadline_ms {
                                    // This reservation has never been released or handed
                                    // to a transmitter: zero possible wire bytes, even if
                                    // the synchronous Writing commit outlasted its lease.
                                    reservation.cancel_expired_led_before_delivery(audited).map_err(|e|e.to_string())?;
                                    arbiter.cancel_selected_before_write(command.id).map_err(|e|e.to_string())?;
                                    principals.remove(&command.id);
                                    log::warn!("dispatch_expired_before_delivery command={} audit_started_ms={} audit_completed_ms={} deadline_ms={} bytes_possible=0",command.id,audit_started,audited,command.deadline_ms);
                                    return Ok(None);
                                }
                                reservation.release();
                                principals.remove(&command.id);
                                in_flight_led=crate::indicator_outputs::Ledger::color(command.command).map(|color|(command.id,command.device_id.clone(),color,command.epochs));
                                in_flight = Some(command.id);
                                write_deadline = Some(command.deadline_ms);
                                let away_authorization=module_off_jobs.get(&command.id).map(|target|target.lease.clone());
                                Ok(Some(DispatchTicket {
                                    gateway_id,
                                    command,
                                    frame,
                                    panel_authorization,
                                    source_authorization,
                                    away_authorization,
                                    publication_stop,
                                }))
                            })();
                            match result {
                                Ok(ticket) => {
                                    if reply.send(Ok(ticket)).is_err() && in_flight.is_some() {
                                        // Delivery raced caller cancellation. Stop; startup recovery
                                        // marks Writing uncertain and never replays the frame.
                                        failed = true; audited_transport_failure=false;
                                        let _ = arbiter.storage_failed();
                                    }
                                }
                                Err(_) => {
                                    failed = true; audited_transport_failure=false;
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
                                let written_ms=millis(clock);
                                let audit = if complete {
                                    store.transition_command(&worker_boot, id, Status::Writing,
                                        Status::Written, written_ms)
                                } else {
                                    store.fail_transport(&worker_boot, millis(clock))
                                };
                                if audit.is_err()
                                    || arbiter.finish_write(id, complete).is_err()
                                {
                                    failed = true; audited_transport_failure=false;
                                    let _ = arbiter.storage_failed();
                                    Err(WriteStatus::ServiceCommunicationFailure)
                                } else {
                                    if complete {
                                        // Validate before applying our own write so a different
                                        // subject changing the owner while in-flight is not absorbed.
                                        if let Some(group)=motor_guard_ids.get(&id).copied()
                                            && let Some(guard)=motor_guards.get_mut(&group) {
                                            let allowed=store.active_bundle(options.context).ok().flatten().and_then(|b|b.automation).and_then(|d|d.enabled_motor_requests().ok()).unwrap_or_default();
                                            guard.check(homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs},&allowed,&mut curtains,written_ms,authorize(guard.controller()));
                                        }
                                        curtains.written(id,written_ms);
                                        if let Some((command,device,color,epochs))=in_flight_led.take()
                                            && (command!=id || led_outputs.written(device,command,color,epochs).is_err()) {
                                            failed=true;audited_transport_failure=false;let _=arbiter.storage_failed();
                                        }
                                        if let Some(group)=motor_guard_ids.remove(&id)
                                            && let Some(guard)=motor_guards.get_mut(&group) {guard.own_written(&mut curtains,written_ms);}
                                    } else {in_flight_led=None;curtains.invalidate();}
                                    in_flight = None;
                                    write_deadline = None;
                                    if !complete {
                                        principals.clear();
                                        failed = true;
                                        audited_transport_failure=arbiter.storage_failed().is_ok();
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
                        Message::Submit(submission, request_source, generation, deadline, reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            let mut manual_superseded = false;
                            let native_mode=matches!(&*submission,Submission::Native(_));
                            let mut native_attempt:Option<(homesense_storage::native_scene::Request,Option<u64>)>=None;
                            let mut native_accepted:Option<(homesense_storage::native_scene::Request,u64,u64,bool)>=None;
                            let mut native_run=None;let mut native_deadline=None;
                            let result = (|| {
                                if failed || millis(clock) >= deadline || generation.as_ref().is_some_and(|scope| match scope {
                                    SubmissionScope::Generation(g) => g.boot != worker_boot || g.epochs != arbiter.policy().epochs,
                                    SubmissionScope::Configuration(revision) => *revision != configuration.revision(),
                                }) {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                let mut selection_checks = vec![];
                                let mut command_sources = None;
                                let software_sample = matches!(&*submission, Submission::Automation { software_sample: true, .. });
                                if request_source.as_ref().is_some_and(|source|!source.valid()) {return Err(WriteStatus::InsufficientPrivileges);}
                                let (submission, study, automation, variables, feedback, audit, scene, starts) = match *submission {
                                    Submission::Native(prepared)=>{
                                        let policy=arbiter.policy();
                                        let request=prepared.request().clone();
                                        if next_id>=(1u64<<62) {return Err(WriteStatus::ResourceBusy);}
                                        native_deadline=Some(request.deadline_ms);
                                        if !authorize(Uuid::parse_str(&request.controller).map_err(|_|WriteStatus::InvalidValueInRequest)?) {return Err(WriteStatus::InsufficientPrivileges);}
                                        if prepared.generation()!=&(TaskGeneration{boot:worker_boot.clone(),epochs:policy.epochs}) || !policy.gateway_ready || policy.publishing || !policy.controller || !policy.storage_healthy || !policy.valid_configuration_loaded {return Err(WriteStatus::ResourceBusy);}
                                        store.expire_native_scenes(&worker_boot,policy.epochs,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                        let old=store.native_scene_request(&request.controller,&request.key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                        native_run=Some((1u64<<62)+old.as_ref().and_then(|r|r.initial_commands.first().copied()).unwrap_or(next_id));
                                        let before=old.map(|r|r.phase);
                                        native_attempt=Some((request.clone(),before));
                                        if let Some(g)=&gateways && before.is_some() {
                                            let key=homesense_storage::gateway_audit::OperationKey {kind:homesense_storage::gateway_audit::OperationKind::Native,id:native_run.ok_or(WriteStatus::InvalidValueInRequest)?.to_string()};
                                            let (original,status)=store.gateway_operation_scope(&worker_boot,&key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                            if status!="active" || original.global!=policy.epochs || original.wires.iter().any(|wire|!g.table.ready(wire)) {return Err(WriteStatus::ResourceBusy);}
                                        }
                                        let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                        let group=document.scene_switch_groups.iter().find(|g|g.id==request.group).ok_or(WriteStatus::InvalidValueInRequest)?;
                                        let mut current=store.scene_selection(configuration.revision(),&request.variable,scene_observer.scope()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::InvalidValueInRequest)?;
                                        crate::scene_reconciliation::project(&mut current,scene_projections.get(&request.variable),group,&store,&configuration,&document,&worker_boot,policy.epochs,millis(clock),true)?;
                                        let permit=crate::source_auth::commit(request_source.as_ref(),||prepared.admit(&mut store,&configuration,&document,&worker_boot,policy.epochs,millis(clock),current.stored.value.is_none(),&manual_versions)).ok_or(WriteStatus::InsufficientPrivileges)??;
                                        selection_checks.push(homesense_storage::variables::ReadCheck{revision:configuration.revision(),name:request.variable.clone(),expected_version:request.variable_version});
                                        native_accepted=Some((permit.request,permit.phase,permit.fence,permit.batch.intents.iter().any(|i|i.command!=homesense_core::Command::Query)));
                                        (Submission::NativePhysical(permit.batch),None,false,vec![],Some(Box::new(permit.lease)),None,None,vec![])
                                    },
                                    Submission::Automation { batch, variables, manual_fence, feedback, audit, scene, starts, command_sources: sources, entry_reads, .. } => {
                                        homesense_storage::rule_audit::validate_sources(sources.as_deref(), batch.intents.len()).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                        if sources.is_some() && audit.is_none() {return Err(WriteStatus::InvalidValueInRequest);}
                                        command_sources = sources;
                                        if let Some(reads)=entry_reads {
                                            if !authorize(batch.controller){return Err(WriteStatus::InsufficientPrivileges);}
                                            if !reads.deactivated_scenes.is_empty() {
                                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                if reads.deactivated_scenes.len()>64 || reads.deactivated_scenes.iter().any(|id|!document.scene_deactivations.get(id).is_some_and(|l|l.enabled)) {return Err(WriteStatus::InvalidValueInRequest);}
                                                if store.has_unsettled_control_commands(&worker_boot,configuration.revision()).map_err(|_|WriteStatus::ServiceCommunicationFailure)? || store.has_active_scene_runs(&worker_boot,configuration.revision()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?
                                                    || starts.iter().any(|s|reads.deactivated_scenes.contains(&s.definition)) {return Err(WriteStatus::ResourceBusy);}
                                            }
                                            if reads.conditions.len()>1024 || reads.selection.as_ref().is_some_and(|r|r.len()>64){return Err(WriteStatus::InvalidValueInRequest);}
                                            let mut versions=BTreeMap::new();
                                            for read in reads.conditions {
                                                if read.revision!=configuration.revision(){return Err(WriteStatus::InvalidValueInRequest);}
                                                if let Some(current)=temporary.get(&read.name) {
                                                    if current.version!=read.expected_version{return Err(WriteStatus::InvalidValueInRequest);}
                                                } else if versions.insert(read.name,read.expected_version).is_some(){return Err(WriteStatus::InvalidValueInRequest);}
                                            }
                                            if let Some(expected_reads)=reads.selection {
                                                if worker_observation_invalidation.load(Ordering::Acquire)!=observation_fence {scene_observer.invalidate();}
                                                store.expire_native_scenes(&worker_boot,arbiter.policy().epochs,millis(clock)).map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                let policy=arbiter.policy();
                                                let ready=!failed&&!worker_stop.load(Ordering::Acquire)&&!policy.publishing&&policy.gateway_ready&&policy.controller&&policy.storage_healthy&&policy.valid_configuration_loaded;
                                                if !ready{return Err(WriteStatus::ServiceCommunicationFailure);}
                                                for (name,expected) in expected_reads {
                                                    let group=document.scene_switch_groups.iter().find(|g|g.variable==name).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                    let mut current=store.scene_selection(configuration.revision(),&name,scene_observer.scope()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::InvalidValueInRequest)?;
                                                    crate::scene_reconciliation::project(&mut current,scene_projections.get(&name),group,&store,&configuration,&document,&worker_boot,policy.epochs,millis(clock),ready)?;
                                                    if current.stored!=expected.stored || current.evidence!=expected.evidence || current.evidence_current!=expected.evidence_current || current.initialization!=expected.initialization || current.physical!=expected.physical{return Err(WriteStatus::InvalidValueInRequest);}
                                                    if versions.insert(name,expected.stored.version).is_some_and(|old|old!=expected.stored.version){return Err(WriteStatus::InvalidValueInRequest);}
                                                }
                                            }
                                            selection_checks.extend(versions.into_iter().map(|(name,expected_version)|homesense_storage::variables::ReadCheck{revision:configuration.revision(),name,expected_version}));
                                        }
                                        if software_sample && batch.intents.iter().any(|intent| !matches!(intent.command, homesense_core::Command::SetLevel { transition: homesense_core::LevelTransition::Immediate, .. })) {
                                            return Err(WriteStatus::InvalidValueInRequest);
                                        }
                                        if manual_fence.is_some_and(|fence|batch.intents.iter().any(|i|manual_versions.get(&i.device).is_some_and(|version|*version>=fence))) {
                                            manual_superseded = true;
                                            return Err(WriteStatus::InvalidValueInRequest);
                                        }
                                        if !batch.subscriptions.is_empty() || (batch.intents.is_empty() && variables.is_empty() && audit.is_none()) || variables.len()>1024 || variables.iter().any(|v|v.revision!=configuration.revision()) { return Err(WriteStatus::InvalidValueInRequest); }
                                        (Submission::Lights(batch), None, true, variables, feedback, audit, scene, starts)
                                    },
                                    Submission::Indicator { batch, variables, feedback, reads,output_fence } => {
                                        if !authorize(batch.controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                        if !batch.subscriptions.is_empty() || batch.intents.is_empty() || feedback.observations.is_empty() || reads.len()>64 || variables.iter().any(|u| !reads.contains_key(&u.name) || u.value!=Some(homesense_core::rule::Value::Integer(0))) {return Err(WriteStatus::InvalidValueInRequest);}
                                        let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                        for intent in &batch.intents {
                                            let homesense_core::Command::PanelLed(on)=intent.command else {return Err(WriteStatus::InvalidValueInRequest);};
                                            let link=document.indicator_links.iter().find(|l|l.enabled && l.leds.contains(&intent.device)).ok_or(WriteStatus::InvalidValueInRequest)?;
                                            if on {
                                                if !variables.is_empty() {return Err(WriteStatus::InvalidValueInRequest);}
                                                let scene=link.scene.as_ref().ok_or(WriteStatus::InvalidValueInRequest)?;
                                                let group=document.scene_switch_groups.iter().find(|g|g.reconciliation.as_ref().is_some_and(|p|p.enabled) && g.members.iter().any(|m|&m.scene==scene)).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                let member=group.members.iter().find(|m|&m.scene==scene).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                let source=reads.get(&group.variable).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                if source.stored.value!=Some(homesense_core::rule::Value::Integer(member.value)) || !source.physical.as_ref().is_some_and(|p|!p.pending_relative && p.observed_ms.is_some()) {return Err(WriteStatus::InvalidValueInRequest);}
                                            }
                                        }
                                        if let Some(expected)=output_fence {
                                            let ids=batch.intents.iter().map(|i|i.device.clone()).collect::<Vec<_>>();
                                            if expected.len()!=ids.len() || ids.iter().any(|id|!expected.contains_key(id)) || led_outputs.capture(&store,&worker_boot,configuration.revision(),&ids,arbiter.policy().epochs).map_err(|_|WriteStatus::ServiceCommunicationFailure)?!=expected {return Err(WriteStatus::ResourceBusy);}
                                        }
                                        if worker_observation_invalidation.load(Ordering::Acquire)!=observation_fence {scene_observer.invalidate();}
                                        for (name, expected) in &reads {
                                            if !document.scene_switch_groups.iter().any(|g|g.variable==*name && g.members.iter().any(|m|document.indicator_links.iter().any(|l|l.enabled && l.scene.as_ref()==Some(&m.scene)))) {return Err(WriteStatus::InvalidValueInRequest);}
                                            let mut current=store.scene_selection(configuration.revision(),name,scene_observer.scope()).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::InvalidValueInRequest)?;
                                            let group=document.scene_switch_groups.iter().find(|g|g.variable==*name).ok_or(WriteStatus::InvalidValueInRequest)?;
                                            let policy=arbiter.policy();
                                            crate::scene_reconciliation::project(&mut current,scene_projections.get(name),group,&store,&configuration,&document,&worker_boot,policy.epochs,millis(clock),policy.gateway_ready && !policy.publishing)?;
                                            if current.stored!=expected.stored || current.evidence!=expected.evidence || current.evidence_current!=expected.evidence_current || current.initialization!=expected.initialization || current.physical!=expected.physical {return Err(WriteStatus::InvalidValueInRequest);}
                                            if let Some(source)=&expected.physical && let Some(observed)=source.observed_ms {
                                                if feedback.indicator_after.is_none_or(|after|after<observed) {return Err(WriteStatus::InvalidValueInRequest);}
                                                for device in &feedback.recorded_off_reads {
                                                    if !store.dimmer_setting_written_after(configuration.revision(),device,&worker_boot,observed).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {return Err(WriteStatus::InvalidValueInRequest);}
                                                }
                                            }
                                            if let Some(source)=&expected.initialization && source.value>0 && source.boot_id==Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128() {
                                                if feedback.indicator_after.is_none_or(|after|after<source.initialized_ms) {return Err(WriteStatus::InvalidValueInRequest);}
                                                for device in &feedback.recorded_off_reads {
                                                    if !store.dimmer_setting_written_after(configuration.revision(),device,&worker_boot,source.initialized_ms).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {return Err(WriteStatus::InvalidValueInRequest);}
                                                }
                                            }
                                            if expected.evidence_current && let Some(e)=&expected.evidence && feedback.indicator_after.is_none_or(|after|after<e.observed_ms) {return Err(WriteStatus::InvalidValueInRequest);}
                                            if expected.evidence_current && let Some(e)=&expected.evidence {
                                                for device in &feedback.recorded_off_reads {
                                                    if !store.dimmer_setting_written_after(configuration.revision(),device,&worker_boot,e.observed_ms).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {return Err(WriteStatus::InvalidValueInRequest);}
                                                }
                                            }
                                            selection_checks.push(homesense_storage::variables::ReadCheck {revision:configuration.revision(),name:name.clone(),expected_version:expected.stored.version});
                                        }
                                        (Submission::Lights(batch),None,true,variables,Some(feedback),None,None,vec![])
                                    },
                                    Submission::Study { batch, expected_mode, next_mode } => {
                                        if batch.intents.iter().any(|i| !["control4:210","control4:278","control4:308","control4:336","control4:63:key:1","control4:63:key:3","control4:63:key:5"].contains(&i.device.as_str())) || batch.intents.is_empty() || batch.intents.len()>16 || !batch.subscriptions.is_empty() {return Err(WriteStatus::InvalidValueInRequest);}
                                        (Submission::Lights(batch), Some((expected_mode,next_mode)), true, vec![], None, None, None, vec![])
                                    },
                                    other => (other,None,false,vec![],None,None,None,vec![]),
                                };
                                // Only trusted rule adapters attach this persistent audit.
                                // External HAP/HTTP light batches never select ordered mode.
                                let ordered_rule = audit.is_some();
                                if audit.as_ref().is_some_and(|a|a.revision!=configuration.revision()){return Err(WriteStatus::InvalidValueInRequest);}
                                if !starts.is_empty() {
                                    if starts.len()>64 || audit.is_none() || scene.is_some() {return Err(WriteStatus::InvalidValueInRequest);}
                                    let bundle=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                    let document=bundle.automation.ok_or(WriteStatus::InvalidValueInRequest)?;
                                    for start in &starts {
                                        if start.revision!=configuration.revision() || !document.timed_scenes.get(&start.definition).is_some_and(|s|s.enabled && s.review_evidence.as_ref().is_some_and(|v|!v.trim().is_empty())) {return Err(WriteStatus::InvalidValueInRequest);}
                                    }
                                }
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
                                if let Some(motor)=feedback.as_ref().and_then(|lease|lease.motor.as_ref()) {
                                    let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InsufficientPrivileges)?;
                                    let allowed=document.enabled_motor_requests().map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                    // Same owner thread, before any queue or variable mutation.
                                    let controller=match &submission {Submission::Lights(batch)|Submission::NativePhysical(batch)=>batch.controller,_=>return Err(WriteStatus::InvalidValueInRequest)};
                                    if !authorize(controller){return Err(WriteStatus::InsufficientPrivileges);}
                                    motor.validate(controller,homesense_core::curtain_state::Scope{boot:observer_boot,epochs:arbiter.policy().epochs},&allowed,&mut curtains,millis(clock))?;
                                }
                                let _feedback_guard = feedback.as_ref().map(|lease|lease.lock(
                                    Uuid::parse_str(&worker_boot).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.as_u128(),
                                    configuration.revision(), arbiter.policy().epochs.transport, millis(clock))
                                    .map_err(|_|WriteStatus::InvalidValueInRequest)).transpose()?;
                                if let Some(lease) = &feedback {
                                    for (device, observed) in &lease.observations {
                                        if !store.feedback_is_settled_in_epoch(&worker_boot,device,*observed,arbiter.policy().epochs.transport).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {
                                            return Err(WriteStatus::ResourceBusy);
                                        }
                                    }
                                }
                                let motor_queue_lease=feedback.as_ref().and_then(|lease|lease.motor.clone());
                                let mut restore_intents = Vec::new();
                                let mut key_dependencies = Vec::new();
                                let mut inference_source = None;
                                let panel_authorization=match &submission {Submission::PanelCommand{lease,..}=>Some(lease.clone()),_=>None};
                                let (batch, compiled) = match submission {
                                    Submission::Study { .. } | Submission::Automation { .. } | Submission::Indicator { .. } | Submission::Native(_) => unreachable!(),
                                    Submission::NativePhysical(batch)=>{restore_intents=batch.intents.iter().filter(|i|!matches!(i.command,homesense_core::Command::Query|homesense_core::Command::Toggle)).map(|i|(i.device.clone(),i.command)).collect();(batch,true)},
                                    Submission::Lights(mut batch) => {
                                        if !authorize(batch.controller) { return Err(WriteStatus::InsufficientPrivileges); }
                                        restore_intents = batch.intents.iter().map(|i| (i.device.clone(), i.command)).collect();
                                        let mut expanded = Vec::new();
                                        let mut expanded_sources = command_sources.as_ref().map(|_| Vec::new());
                                        let mut seen = std::collections::BTreeSet::new();
                                        for (index, intent) in batch.intents.into_iter().enumerate() {
                                            if (!ordered_rule && !seen.insert(intent.device.clone())) || configuration.devices().values().any(|d| d.enabled && d.supply_relay.as_ref() == Some(&intent.device)) {
                                                return Err(WriteStatus::InvalidValueInRequest);
                                            }
                                            let (device, module) = configuration.binding(&intent.device).ok_or(WriteStatus::ResourceDoesNotExist)?;
                                            if let Some(relay) = &device.supply_relay {
                                                // HAP normalization must resolve restoration before admission.
                                                if matches!(intent.command, homesense_core::Command::Switch(true)) {
                                                    return Err(WriteStatus::InvalidValueInRequest);
                                                }
                                                for (output, mut command) in crate::combined_light::plan(intent.command, None, device.default_on_level).map_err(|_| WriteStatus::InvalidValueInRequest)? {
                                                    if software_sample && let homesense_core::Command::SetLevel { level, .. } = command {
                                                        command = homesense_core::Command::SetLevel { level, transition: homesense_core::LevelTransition::Immediate };
                                                    }
                                                    if let Some(labels) = &mut expanded_sources {labels.push(command_sources.as_ref().expect("validated labels")[index].clone());}
                                                    expanded.push(homesense_hap::requests::Intent { device: match output {
                                                        crate::combined_light::Output::Relay => relay.clone(),
                                                        crate::combined_light::Output::Dimmer => intent.device.clone(),
                                                    }, command });
                                                }
                                            } else {
                                                if let Some(labels) = &mut expanded_sources {labels.push(command_sources.as_ref().expect("validated labels")[index].clone());}
                                                let command=if automation && let homesense_core::Command::Motor(motion)=intent.command {
                                                    // Configuration lowered logical direction; independently
                                                    // require current calibrated motor scope at admission.
                                                    configuration.motor_command(&intent.device,homesense_core::MotorAction::Stop).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                                    homesense_core::Command::Motor(motion)
                                                } else if software_sample || automation && matches!(intent.command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PauseRamp){intent.command}
                                                else if automation {crate::light_command::plan_automation(module,intent.command).map_err(|_|WriteStatus::InvalidValueInRequest)?}
                                                else {crate::light_command::plan(module,intent.command).map_err(|_|WriteStatus::InvalidValueInRequest)?};
                                                expanded.push(homesense_hap::requests::Intent{device:intent.device,command});
                                            }
                                        }
                                        command_sources = expanded_sources;
                                        batch.intents = expanded;
                                        (batch, true)
                                    },
                                    Submission::PanelCommand {controller,device,command,lease} => {
                                        if !lease.valid_for(&configuration,controller,&device,command) {return Err(WriteStatus::InsufficientPrivileges);}
                                        (Batch {controller,intents:vec![homesense_hap::requests::Intent{device,command}],subscriptions:vec![]},true)
                                    },
                                    Submission::PanelLed { controller, device, active } => (
                                        Batch { controller, intents: vec![homesense_hap::requests::Intent {
                                            device, command: homesense_core::Command::PanelLed(active)
                                        }], subscriptions: vec![] }, true),
                                    Submission::KeypadPress { controller, device, keypad, key } => {
                                        if !authorize(controller) {return Err(WriteStatus::InsufficientPrivileges);}
                                        let source=request_source.as_ref().filter(|source|source.valid_for(controller,configuration.revision())).ok_or(WriteStatus::InsufficientPrivileges)?;
                                        let original=crate::automation_control::Action::DeviceCommand{controller,revision:configuration.revision(),device:device.clone(),action:crate::management_devices::DeviceAction::KeypadPress{keypad:keypad.clone(),key}};
                                        if !source.matches(&original) {return Err(WriteStatus::InsufficientPrivileges);}
                                        let binding=crate::keypad_motor::resolve(&configuration,&device,&keypad,key)?;
                                        if !source.permits_device(device.as_str()) || !source.permits_device(binding.endpoint.as_str()) {
                                            return Err(WriteStatus::InsufficientPrivileges);
                                        }
                                        if arbiter.has_pending_motor(&device) {return Err(WriteStatus::ResourceBusy);}
                                        curtains.retire_revoked(&|id|authorize(id));
                                        let movement=curtains.snapshot(&device,millis(clock)).and_then(|s|s.inferred_movement.map(|f|f.value));
                                        let action=crate::keypad_motor::decide(binding.action,movement);
                                        if action==homesense_core::MotorAction::Stop {inference_source=curtains.inference_source(&device);}
                                        let command=configuration.motor_command(&device,action).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                        key_dependencies.push(binding.endpoint);
                                        (Batch {controller,intents:vec![homesense_hap::requests::Intent{device,command}],subscriptions:vec![]},true)
                                    },
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
                                if request_source.as_ref().is_some_and(|source|!source.valid_for(batch.controller,configuration.revision())) {return Err(WriteStatus::InsufficientPrivileges);}
                                if batch.intents.len() > 128 {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                // Subscription-only requests still require authorization.
                                if batch.intents.is_empty() {
                                    let commit=||->Result<(),WriteStatus> {
                                    if !variables.is_empty() || audit.is_some() {
                                        let accepted=if let Some(g)=&gateways {
                                            let mut reads=feedback.as_ref().map_or_else(Vec::new,|lease|lease.observations.keys().cloned().collect());
                                            if let Some(motor)=feedback.as_ref().and_then(|lease|lease.motor.as_ref()) {reads.extend(motor.devices());}
                                            let requirements=g.device_requirements(&configuration,reads)?;
                                            let declared=if starts.is_empty(){vec![]}else{
                                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                g.scene_scopes(&configuration,&document,&starts.iter().map(|start|(start.run.clone(),start.definition.clone())).collect::<Vec<_>>(),&requirements)?
                                            };
                                            let scopes=g.scope(&configuration,&[],&BTreeMap::new(),&requirements,declared)?;
                                            store.accept_queue_batch_scoped(&worker_boot,&[],&[],millis(clock),None,&variables,audit.as_ref(),None,&starts,&selection_checks,command_sources.as_deref(),&[],&scopes)
                                        }else{store.accept_queue_batch_with_read_checks_and_sources(&worker_boot,&[],&[],millis(clock),None,&variables,audit.as_ref(),None,&starts,&selection_checks,command_sources.as_deref())};
                                        match accepted {
                                            Ok(()) => {},
                                            Err(homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit) => return Err(WriteStatus::InvalidValueInRequest),
                                            Err(_) => {
                                                failed = true; audited_transport_failure=false;
                                                let _ = arbiter.storage_failed();
                                                return Err(WriteStatus::ServiceCommunicationFailure);
                                            }
                                        }
                                    }
                                    temporary.extend(memory_updates);
                                    Ok(())
                                    };
                                    crate::source_auth::commit(request_source.as_ref(),commit).ok_or(WriteStatus::InsufficientPrivileges)??;
                                    return Ok(Vec::new());
                                }
                                let end = next_id
                                    .checked_add(batch.intents.len() as u64)
                                    .ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let manual_devices: Vec<_> = if automation {vec![]} else {batch.intents.iter().filter(|i|!matches!(i.command,homesense_core::Command::PanelParameter(_)) && (!native_mode || i.command!=homesense_core::Command::Query)).map(|i|i.device.clone()).collect()};
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
                                            deadline_ms: deadline.min(native_deadline.unwrap_or(deadline)),
                                            run_id: native_run.or_else(||automation.then_some(next_id)),
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
                                if (!starts.is_empty() && !module_off_jobs.is_empty()) || commands.iter().any(|(command,_)| !command.is_stop() && module_off_jobs.values().any(|target|target.plan.modules()[target.index].devices().contains(&command.device_id))) {
                                    return Err(WriteStatus::ResourceBusy);
                                }
                                let curtain_commands:Vec<_>=commands.iter().filter(|(c,_)|matches!(c.command,homesense_core::Command::Motor(_))).map(|(c,_)|c.clone()).collect();
                                let restore_owned: BTreeMap<_,Vec<u64>> = restore_intents.iter().map(|(id,_)| {
                                    let supply=configuration.devices().get(id).and_then(|d|d.supply_relay.as_ref());
                                    (id.clone(),commands.iter().filter(|(c,_)|&c.device_id==id || Some(&c.device_id)==supply).map(|(c,_)|c.id).collect())
                                }).collect();
                                let now = millis(clock);
                                let gateway_scopes=if let Some(g)=&gateways {
                                    let mut reads=feedback.as_ref().map_or_else(Vec::new,|lease|lease.observations.keys().cloned().collect());
                                    if let Some(motor)=feedback.as_ref().and_then(|lease|lease.motor.as_ref()) {reads.extend(motor.devices());}
                                    reads.extend(commands.iter().map(|(command,_)|command.device_id.clone()));
                                    reads.extend(key_dependencies);
                                    let requirements=g.device_requirements(&configuration,reads)?;
                                    let mut requirements=requirements;
                                    let mut declared=if starts.is_empty(){vec![]}else{
                                        let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|b|b.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                        g.scene_scopes(&configuration,&document,&starts.iter().map(|start|(start.run.clone(),start.definition.clone())).collect::<Vec<_>>(),&requirements)?
                                    };
                                    if let Some((request,_,_,_))=&native_accepted {
                                        let key=homesense_storage::gateway_audit::OperationKey {kind:homesense_storage::gateway_audit::OperationKind::Native,id:native_run.ok_or(WriteStatus::InvalidValueInRequest)?.to_string()};
                                        let operation=match store.gateway_operation_scope(&worker_boot,&key).map_err(|_|WriteStatus::ServiceCommunicationFailure)? {
                                            Some((original,status))=>{
                                                if status!="active" || original.global!=arbiter.policy().epochs || original.wires.iter().any(|wire|!g.table.ready(wire)) {return Err(WriteStatus::ResourceBusy);}
                                                original
                                            },
                                            None=>{
                                                let document=store.active_bundle(options.context).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.and_then(|bundle|bundle.automation).ok_or(WriteStatus::InvalidValueInRequest)?;
                                                let reads=crate::gateway_runtime::scene_devices(&document,&request.scene).map_err(|_|WriteStatus::InvalidValueInRequest)?;
                                                homesense_storage::gateway_audit::OperationScope {key,global:arbiter.policy().epochs,wires:g.device_requirements(&configuration,reads)?}
                                            },
                                        };
                                        requirements.extend(operation.wires.clone());declared.push(operation);
                                    }
                                    // A single evaluated action may combine immediate outputs and
                                    // multiple future timelines. Every child retains that complete
                                    // atomic decision scope, not only its own next output bus.
                                    if !declared.is_empty() {
                                        let mut union=requirements.iter().map(|wire|(wire.gateway().to_owned(),wire.clone())).collect::<BTreeMap<_,_>>();
                                        for operation in &declared {for wire in &operation.wires {union.insert(wire.gateway().to_owned(),wire.clone());}}
                                        requirements=union.into_values().collect();
                                        for operation in &mut declared {operation.wires=requirements.clone();}
                                    }
                                    let scene_key=scene.as_ref().map(|scene|homesense_storage::gateway_audit::OperationKey {kind:homesense_storage::gateway_audit::OperationKind::Scene,id:scene.run.clone()});
                                    if let Some(key)=&scene_key {
                                        let (original,status)=store.gateway_operation_scope(&worker_boot,key).map_err(|_|WriteStatus::ServiceCommunicationFailure)?.ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                        let stops_only=commands.iter().all(|(command,_)|command.is_stop());
                                        if !stops_only {
                                            if status!="active" || original.global!=arbiter.policy().epochs {return Err(WriteStatus::ServiceCommunicationFailure);}
                                            requirements.extend(original.wires);
                                        }else{
                                            for (command,_) in &commands {
                                                let physical=g.wire_for(&configuration,&command.device_id)?;
                                                if !original.wires.contains(&physical) {return Err(WriteStatus::ServiceCommunicationFailure);}
                                            }
                                        }
                                    }
                                    let mut scopes=g.scope(&configuration,&commands.iter().map(|(command,_)|command.clone()).collect::<Vec<_>>(),&BTreeMap::new(),&requirements,declared)?;
                                    for command in &mut scopes.commands {
                                        command.operations.extend(scopes.operations.iter().map(|operation|operation.key.clone()));
                                        if let Some(key)=&scene_key {command.operations.push(key.clone());}
                                    }
                                    Some(scopes)
                                }else{None};
                                let persist = |commands: &[QueuedCommand], cancelled: &[homesense_core::queue::Cancellation]| {
                                    if let Some(scopes)=&gateway_scopes {
                                        return store.accept_queue_batch_scoped(&worker_boot,commands,cancelled,now,study,&variables,audit.as_ref(),scene.as_ref(),&starts,&selection_checks,command_sources.as_deref(),&[],scopes);
                                    }
                                    store.accept_queue_batch_with_read_checks_and_sources(
                                        &worker_boot,
                                        commands,
                                        cancelled,
                                        now,
                                        study,
                                        &variables,
                                        audit.as_ref(),
                                        scene.as_ref(),
                                        &starts,
                                        &selection_checks,
                                        command_sources.as_deref(),
                                    )
                                };
                                if request_source.as_ref().is_some_and(|source|!source.valid()) {return Err(WriteStatus::InsufficientPrivileges);}
                                if panel_authorization.as_ref().is_some_and(|lease|!lease.valid()) {return Err(WriteStatus::InsufficientPrivileges);}
                                let commit=|| if ordered_rule {
                                    arbiter.submit_ordered_rule_batch_committed(commands, now, persist)
                                } else {
                                    arbiter.submit_batch_committed(commands, now, persist)
                                };
                                // Serialize a source generation/session change with
                                // queue and variable acceptance, not just physical I/O.
                                let result=crate::source_auth::commit_with_dependency(request_source.as_ref(),inference_source.as_ref(),commit).ok_or(WriteStatus::InsufficientPrivileges)?;
                                match result {
                                    Ok(cancelled) => {
                                        temporary.extend(memory_updates);
                                        for device in manual_devices { manual_versions.insert(device,next_id); }
                                        for (device, command) in restore_intents {
                                            restore_policy.accepted(device.clone(), command, now);
                                            if matches!(command,homesense_core::Command::SetLevel {..}|homesense_core::Command::Switch(false)) {
                                                restore_policy.bind(&device,restore_owned.get(&device).cloned().unwrap_or_default(),arbiter.policy().epochs);
                                            }
                                        }
                                        for id in next_id..end {
                                            principals.insert(id, batch.controller);
                                            if let Some(lease)=&panel_authorization {panel_leases.insert(id,lease.clone());}
                                            if let Some(source)=&request_source {source_leases.insert(id,source.clone());}
                                        }
                                        for cancellation in cancelled {
                                            curtains.cancelled(cancellation.command.id,cancellation.reason);
                                            principals.remove(&cancellation.command.id);
                                        }
                                        for command in &curtain_commands {curtains.accepted_with_source(&configuration,command,batch.controller,now,request_source.clone());}
                                        if next_id<end && let Some(lease)=motor_queue_lease {
                                            motor_guards.insert(next_id,crate::motor_rules::QueuedLease::admitted(lease,&mut curtains,now));
                                            for id in next_id..end {motor_guard_ids.insert(id,next_id);}
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
                                        failed = true; audited_transport_failure=false;
                                        let _ = arbiter.storage_failed();
                                        Err(WriteStatus::ServiceCommunicationFailure)
                                    }
                                }
                            })();
                            let result=result.and_then(|ids:Vec<u64>|{
                                if let Some((request,phase,fence,physical))=&native_accepted {
                                    let fence=if *physical {ids.last().and_then(|id|id.checked_add(1)).unwrap_or(*fence)} else {*fence};
                                    store.native_phase_accepted(request,*phase,&ids,fence,millis(clock)).map_err(|_|{failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();WriteStatus::ServiceCommunicationFailure})?;
                                }Ok(ids)
                            });
                            if result.is_err() && let Some((request,before))=&native_attempt
                                && let Ok(Some(row))=store.native_scene_request(&request.controller,&request.key)
                                && before.is_none_or(|phase|row.phase>phase)
                                && store.fail_native_scene(&request.controller,&request.key,millis(clock)).is_err() {
                                failed=true; audited_transport_failure=false;let _=arbiter.storage_failed();
                            }
                            let result = result.map_err(|status| if manual_superseded {
                                SubmissionFailure::ManualSuperseded
                            } else { SubmissionFailure::Status(status) });
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
            context:options.context,
            fade_budget: crate::fade_budget::Budget::for_write_interval(options.write_interval),
            write_slot: Default::default(),
            write_interval: options.write_interval,
            sender,
            boot,
            clock,
            ttl: options.request_ttl,
            stop_requested,
            generations,
            observation_invalidation,
            source_authorization:None,
        })
    }
    /// Trusted runtime coordinator only: validate then enter the publication barrier.
    /// The caller must drain STOP via the transport owner, commit, rebuild projections
    /// and synchronize the new mapping before reopening control. Not a HAP endpoint.
    pub(crate) async fn publication_bundle(&self, controller:Uuid)->Result<(homesense_storage::automation::Bundle,ExecutionContext,u64),WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::PublicationBundle(controller,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn publication_in_progress(&self)->Result<bool,WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::PublicationState(reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)
    }
    pub async fn begin_publication(
        &self,
        document: Vec<u8>,
        expected: u64,
        timeout: Duration,
    ) -> Result<(), WriteStatus> {
        self.begin_publication_inner(document,None,expected,timeout,None).await
    }
    /// Trusted coordinator: validates both documents before invalidating old work.
    pub async fn begin_automation_publication(&self, document:Vec<u8>, rules:Vec<u8>, expected:u64, timeout:Duration)->Result<(),WriteStatus> {
        self.begin_publication_inner(document,Some(rules),expected,timeout,None).await
    }
    pub(crate) async fn begin_authorized_automation_publication(&self,document:Vec<u8>,rules:Vec<u8>,expected:u64,timeout:Duration,source:Option<crate::publication_auth::Source>)->Result<(),WriteStatus> {
        self.begin_publication_inner(document,Some(rules),expected,timeout,source).await
    }
    async fn begin_publication_inner(&self, document:Vec<u8>, rules:Option<Vec<u8>>, expected:u64, timeout:Duration, source:Option<crate::publication_auth::Source>)->Result<(),WriteStatus> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(30)
            || document.len() > 4 * 1024 * 1024
            || rules.as_ref().is_some_and(|bytes|bytes.len()>4*1024*1024)
        {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let deadline = millis(self.clock)
            .checked_add(timeout.as_millis() as u64)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::BeginPublication(
                document, rules, expected, deadline, source, reply,
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
    pub(crate) async fn dispatch_available(&self,available:std::collections::BTreeSet<String>)->Result<Option<DispatchTicket>,WriteStatus> {
        let(reply,result)=oneshot::channel();self.sender.try_send(Message::Dispatch(Some(available),reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        result.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub async fn dispatch(&self) -> Result<Option<DispatchTicket>, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Dispatch(None,reply))
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
    pub(crate) fn context(&self)->ExecutionContext {self.context}
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
    pub(crate) async fn indicator_outputs(&self,controller:Uuid,generation:TaskGeneration,ids:Vec<homesense_core::DeviceId>,prior:Vec<u64>)->Result<crate::indicator_outputs::Capture,WriteStatus>{
        if !self.source_valid(){return Err(WriteStatus::InsufficientPrivileges);}
        let(reply,receive)=oneshot::channel();self.sender.try_send(Message::IndicatorOutputs(controller,generation,ids,prior,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn feedback_settled(&self,controller:Uuid,generation:TaskGeneration,lease:crate::feedback::RuleLease)->Result<bool,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::FeedbackSettled(controller,generation,lease,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn recent_dimmer_off(&self,controller:Uuid,generation:TaskGeneration,after:u64)->Result<std::collections::BTreeSet<homesense_core::DeviceId>,WriteStatus> {
        let (reply,receive)=oneshot::channel();self.sender.try_send(Message::RecentDimmerOff(controller,generation,after,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn recorded_dimmer_levels(&self,controller:Uuid,generation:TaskGeneration)->Result<RestoreLevels,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::RecordedLevels(controller,generation,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    // Same trusted snapshot publisher boundary as remember_levels; no external
    // request or device action uses this route to bypass controller checks.
    /// Trusted HAP publisher: remember settled observations and capture active
    /// targets in one owner turn. Publication cannot interleave the two reads.
    pub(crate) async fn activate_gateways(&self,initial:Vec<crate::gateway_runtime::Initial>)->Result<(),WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::ActivateGateways(initial,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn received_projection(&self,wire:homesense_core::gateway_scope::WireScope,written:Vec<u64>,received:Vec<homesense_core::DeviceId>,at:u64,snapshot:homesense_state::Snapshot)->Result<(),WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::ReceivedProjection(wire,written,received,at,snapshot,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn gateway_availability(&self)->Result<Vec<crate::gateway_runtime::View>,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayAvailability(tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn gateway_attempt(&self,gateway:String)->Result<homesense_core::gateway_scope::WireScope,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayAttempt(gateway,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn gateway_ready(&self,wire:homesense_core::gateway_scope::WireScope,samples:Vec<crate::gateway_runtime::Sample>)->Result<bool,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayReady(wire,samples,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) fn gateway_fault_blocking(&self,wire:homesense_core::gateway_scope::WireScope)->Result<crate::gateway_runtime::FaultReport,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayFault(GatewayFaultTarget::Wire(wire),tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.blocking_recv().map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn gateway_write_fault(&self,id:u64)->Result<homesense_storage::gateway_audit::FaultAudit,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayFault(GatewayFaultTarget::Writing(id),tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?.map(|report|report.audit)
    }
    pub(crate) async fn gateway_fault(&self,wire:homesense_core::gateway_scope::WireScope)->Result<homesense_storage::gateway_audit::FaultAudit,WriteStatus> {
        let(tx,rx)=oneshot::channel();self.sender.try_send(Message::GatewayFault(GatewayFaultTarget::Wire(wire),tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?.map(|report|report.audit)
    }
    pub(crate) async fn hap_projection(&self,revision:u64,transport_epoch:Option<u64>,snapshot:homesense_state::Snapshot,max_age:u64)->Result<HapProjection,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::HapProjection(revision,transport_epoch,snapshot,max_age,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn projection_levels(&self,revision:u64,boot:u128)->Result<RestoreLevels,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ProjectionLevels(revision,boot,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn scene_history_page(&self,controller:Uuid,generation:TaskGeneration,definition:String,before:Option<homesense_storage::scene_history::Cursor>,limit:usize)->Result<homesense_storage::scene_history::Page,WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::SceneHistory(controller,generation,definition,before,limit,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn diagnostics(&self,controller:Uuid,generation:TaskGeneration)->Result<Diagnostics,WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::Diagnostics(controller,generation,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn configuration_history(&self,controller:Uuid,generation:TaskGeneration,before:Option<u64>,limit:usize)->Result<homesense_storage::configuration_history::Page,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ConfigurationHistory(controller,generation,before,limit,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn configuration_export(&self,controller:Uuid,generation:TaskGeneration,revision:u64)->Result<Option<homesense_storage::configuration_history::Export>,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ConfigurationExport(controller,generation,revision,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn automation_bundle(&self, controller: Uuid, generation: TaskGeneration) -> Result<homesense_storage::automation::Bundle, WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::AutomationBundle(controller,generation,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
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
    pub(crate) async fn written_feedback_target(&self, controller:Uuid, generation:TaskGeneration, id:u64) -> Result<Option<homesense_storage::feedback_inference::WrittenTarget>,WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::WrittenFeedbackTarget(controller,generation,id,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    /// Claim a candidate observation before dispatching any derived rule.
    /// This is an inferred link only; the actor rechecks authority, generation,
    /// clock freshness and the durable local write. It admits no commands.
    pub async fn record_feedback_inference(
        &self, controller: Uuid, generation: TaskGeneration,
        inference: homesense_storage::feedback_inference::Inference,
    ) -> Result<(), WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender.try_send(Message::FeedbackInference(controller,generation,inference,reply))
            .map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
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
    /// Read the single owner projection. Callers must retain their original
    /// device-scoped grants; inference is never a substitute for authorization.
    pub async fn curtain_views(&self,controller:Uuid,generation:TaskGeneration,ids:Vec<homesense_core::DeviceId>)->Result<BTreeMap<homesense_core::DeviceId,crate::curtain_owner::View>,WriteStatus> {
        Ok(self.curtain_snapshots(controller,generation,ids).await?.into_iter().map(|(id,s)|(id,s.into())).collect())
    }
    pub(crate) async fn motor_rule_lease(&self,controller:Uuid,generation:TaskGeneration,requests:Vec<homesense_core::rule_motor::Request>)->Result<crate::motor_rules::Lease,WriteStatus> {
        let (tx,rx)=oneshot::channel();self.sender.try_send(Message::MotorRuleLease(controller,generation,requests,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn curtain_snapshots(&self,controller:Uuid,generation:TaskGeneration,ids:Vec<homesense_core::DeviceId>)->Result<BTreeMap<homesense_core::DeviceId,homesense_core::curtain_state::Snapshot>,WriteStatus> {
        if ids.len()>256{return Err(WriteStatus::InvalidValueInRequest);}
        let (tx,rx)=oneshot::channel();self.sender.try_send(Message::CurtainViews(controller,generation,ids,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn curtain_candidate(&self,controller:Uuid,generation:TaskGeneration,device:homesense_core::DeviceId,timing:homesense_core::curtain_state::Timing)->Result<homesense_core::curtain_state::Snapshot,WriteStatus> {
        let (tx,rx)=oneshot::channel();self.sender.try_send(Message::CurtainCandidate(controller,generation,device,timing,tx)).map_err(|_|WriteStatus::ResourceBusy)?;
        rx.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) fn invalidate_curtain(&self,controller:Uuid,generation:TaskGeneration,device:homesense_core::DeviceId) {
        let _=self.sender.try_send(Message::CurtainInvalidate(controller,generation,device));
    }
    pub async fn motor_receipts(&self,controller:Uuid,generation:TaskGeneration,device:homesense_core::DeviceId,action:homesense_core::MotorAction,ids:Vec<u64>)->Result<Vec<homesense_storage::journal::MotorReceipt>,WriteStatus> {
        if ids.is_empty() || ids.len()>128 {return Err(WriteStatus::InvalidValueInRequest)}
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::MotorReceipts(controller,generation,device,action,ids,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
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
    /// Trusted stream owner invalidation works even while the mailbox is full.
    pub(crate) fn invalidate_scene_observation_now(&self)->Result<(),String> {
        self.observation_invalidation.try_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map(|_|()).map_err(|_|"scene observation fence exhausted".into())
    }
    pub(crate) async fn begin_scene_observation(&self,controller:Uuid,generation:TaskGeneration,proof:crate::feedback::SceneObservationStart)->Result<homesense_core::scene_observer::Scope,WriteStatus> {
        let (reply,receive)=oneshot::channel();self.sender.try_send(Message::BeginSceneObservation(controller,generation,self.observation_invalidation.load(Ordering::Acquire),proof,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn observe_scene_key(&self,controller:Uuid,generation:TaskGeneration,scope:homesense_core::scene_observer::Scope,proof:crate::feedback::SceneKeyObservation)->Result<(),WriteStatus> {
        let (reply,receive)=oneshot::channel();self.sender.try_send(Message::ObserveSceneKey(controller,generation,scope,proof,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    /// Selection consumers may use current inferred values with their source.
    /// Ordinary rule snapshots deliberately continue to mask them as unknown.
    pub async fn scene_selection_snapshot(&self,controller:Uuid,generation:TaskGeneration,names:Vec<String>)->Result<BTreeMap<String,homesense_storage::scene_observation::Snapshot>,WriteStatus> {
        if names.len()>64 || names.iter().any(|n|n.is_empty() || n.len()>256) {return Err(WriteStatus::InvalidValueInRequest)}
        let (reply,receive)=oneshot::channel();self.sender.try_send(Message::SceneSelections(controller,generation,names,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    #[cfg(test)]
    pub(crate) async fn module_query(&self,controller:Uuid,generation:TaskGeneration,module_id:String,scope:crate::module_queries::Scope,submit:bool)->Result<crate::module_queries::Accepted,WriteStatus>{
        self.module_query_until(controller,generation,module_id,scope,submit,Instant::now()+self.ttl).await
    }
    pub(crate) async fn module_query_until(&self,controller:Uuid,generation:TaskGeneration,module_id:String,scope:crate::module_queries::Scope,submit:bool,deadline:Instant)->Result<crate::module_queries::Accepted,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ModuleQuery(controller,self.source_authorization.clone(),generation,module_id,scope,deadline.min(Instant::now()+self.ttl),submit,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn module_query_records(&self,controller:Uuid,generation:TaskGeneration,commands:Vec<u64>)->Result<Vec<homesense_storage::module_history::Record>,WriteStatus>{
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ModuleQueryRecords(controller,generation,commands,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn recent_device_commands(&self,controller:Uuid,generation:TaskGeneration,ids:Vec<homesense_core::DeviceId>,limit:usize)->Result<Vec<homesense_storage::module_history::Record>,WriteStatus>{
        if ids.len()>128 || !(1..=64).contains(&limit){return Err(WriteStatus::InvalidValueInRequest);}
        let (reply,receive)=oneshot::channel();self.sender.try_send(Message::RecentDeviceCommands(controller,generation,ids,limit,reply)).map_err(|_|WriteStatus::ResourceBusy)?;receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn native_scene_status(&self,controller:Uuid,generation:TaskGeneration,key:String)->Result<Option<homesense_storage::native_scene::Record>,WriteStatus>{let (reply,receive)=oneshot::channel();self.sender.try_send(Message::NativeStatus(controller,generation,key,reply)).map_err(|_|WriteStatus::ResourceBusy)?;receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?}
    pub(crate) async fn fail_native_scene(&self,controller:Uuid,generation:TaskGeneration,key:String)->Result<(),WriteStatus>{let (reply,receive)=oneshot::channel();self.sender.try_send(Message::NativeFail(controller,generation,key,reply)).map_err(|_|WriteStatus::ResourceBusy)?;receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?}
    pub(crate) async fn finish_native_scene(&self,controller:Uuid,completion:crate::native_scene::Completion)->Result<(),WriteStatus>{let (reply,receive)=oneshot::channel();self.sender.try_send(Message::NativeFinish(controller,self.source_authorization.clone(),Box::new(completion),reply)).map_err(|_|WriteStatus::ResourceBusy)?;receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?}
    pub(crate) async fn submit_native_scene(&self,generation:TaskGeneration,submission:crate::native_scene::Submission)->Result<Vec<u64>,WriteStatus>{self.submit_scoped_before(Submission::Native(Box::new(submission)),Some(SubmissionScope::Generation(generation)),None).await}
    pub(crate) async fn reconcile_scene_selection(&self,controller:Uuid,generation:TaskGeneration,prepared:crate::scene_reconciliation::Prepared)->Result<(),WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ReconcileSceneSelection(controller,generation,prepared,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn initialize_scene_selection(&self,controller:Uuid,generation:TaskGeneration,prepared:crate::scene_initialization::Prepared)->Result<(),WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::InitializeSceneSelection(controller,generation,prepared,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) async fn scene_selection_values(&self,controller:Uuid,generation:TaskGeneration,names:Vec<String>)->Result<BTreeMap<String,homesense_storage::variables::Stored>,WriteStatus> {
        Ok(self.scene_selection_snapshot(controller,generation,names).await?.into_iter().map(|(name,snapshot)|(name,snapshot.stored)).collect())
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
    pub(crate) async fn manual_superseded(&self, device: homesense_core::DeviceId, fence: u64) -> Result<bool, WriteStatus> {
        let (reply, receive) = oneshot::channel();
        self.sender.try_send(Message::ManualSuperseded(device, fence, reply)).map_err(|_| WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_| WriteStatus::ServiceCommunicationFailure)
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
        self.submit_scene_with_feedback_before(generation,batch,manual_fence,feedback,None).await
    }
    pub(crate) async fn submit_scene_with_feedback_before(
        &self, generation: TaskGeneration, batch: Batch, manual_fence: u64,
        feedback: Option<crate::feedback::RuleLease>, deadline: Option<u64>,
    ) -> Result<Vec<u64>,WriteStatus> {
        self.submit_scene_sample(generation,batch,manual_fence,feedback,deadline,None,false).await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn submit_scene_sample(
        &self, generation: TaskGeneration, batch: Batch, manual_fence: u64,
        feedback: Option<crate::feedback::RuleLease>, deadline: Option<u64>,
        scene: Option<homesense_storage::scene_run::StepLink>,
        software_sample: bool,
    ) -> Result<Vec<u64>,WriteStatus> {
        self.submit_scoped_before(
            Submission::Automation { entry_reads:None, software_sample, command_sources: None,
                batch,
                variables: vec![],
                manual_fence: Some(manual_fence),
                feedback: feedback.map(Box::new),
                audit: None,
                scene,
                starts: vec![],
            },
            Some(SubmissionScope::Generation(generation)),
            deadline,
        )
        .await
    }
    pub(crate) async fn submit_scene_step(
        &self, generation: TaskGeneration, batch: Batch, manual_fence: u64,
        scene: homesense_storage::scene_run::StepLink,
        deadline: Option<u64>,
    ) -> Result<Vec<u64>, SubmissionFailure> {
        self.submit_scoped_detailed(Submission::Automation { entry_reads:None, software_sample: false, command_sources: None, batch, variables: vec![], manual_fence: Some(manual_fence), feedback: None, audit: None, scene: Some(scene), starts: vec![] }, Some(SubmissionScope::Generation(generation)),deadline).await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn submit_audited_rule(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
        feedback: Option<crate::feedback::RuleLease>,
        audit: homesense_storage::rule_audit::Admission,
        command_sources: Vec<String>,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation { entry_reads:None, software_sample: false, command_sources: Some(command_sources),
                batch,
                variables,
                feedback: feedback.map(Box::new),
                audit: Some(audit),
                scene: None,
                starts: vec![],
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
            Submission::Automation { entry_reads:None, software_sample: false, command_sources: None,
                batch,
                variables,
                manual_fence: None,
                feedback: Some(Box::new(feedback)),
                audit: None,
                scene: None,
                starts: vec![],
            },
            Some(SubmissionScope::Generation(generation)),
        )
        .await
    }
    pub(crate) async fn submit_indicator_rule(
        &self, generation: TaskGeneration, batch: Batch,
        variables: Vec<homesense_storage::variables::Update>, feedback: crate::feedback::RuleLease,
        reads: BTreeMap<String, homesense_storage::scene_observation::Snapshot>,
        output_fence:Option<crate::indicator_outputs::Fence>,
    ) -> Result<Vec<u64>,WriteStatus> {
        self.submit_scoped(Submission::Indicator {batch,variables,feedback:Box::new(feedback),reads,output_fence},Some(SubmissionScope::Generation(generation))).await
    }
    /// Accept named manual scenes together; reserve scheduler slots first.
    /// Dropping an in-flight request closes admission because acceptance may
    /// already be durable. Returned capabilities must be installed, not replayed.
    pub async fn accept_manual_scenes(&self, controller:Uuid, generation:TaskGeneration, definitions:Vec<String>)
        -> Result<Vec<AcceptedScene>,WriteStatus> {
        self.accept_manual_scenes_with_variables(controller,generation,definitions,vec![]).await
    }
    pub(crate) async fn accept_manual_scenes_with_variables(&self,controller:Uuid,generation:TaskGeneration,definitions:Vec<String>,updates:Vec<homesense_storage::variables::Update>)->Result<Vec<AcceptedScene>,WriteStatus> {
        struct Guard(Admission,bool);
        impl Drop for Guard { fn drop(&mut self) { if !self.1 { self.0.stop_now(); } } }
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::ManualScenes(controller,self.source_authorization.clone(),generation,definitions,updates,reply))
            .map_err(|_|WriteStatus::ResourceBusy)?;
        let mut guard=Guard(self.clone(),false);
        let result=receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
        guard.1=true;
        result
    }
    /// Atomically accept variable changes, immediate output and delayed receipts.
    /// No delayed output is scheduled by this call. It is not a network endpoint.
    pub async fn submit_deferred_rule(&self,generation:TaskGeneration,rule:DeferredRule)->Result<AcceptedDeferred,WriteStatus> {
        self.submit_deferred_rule_with_feedback(generation,rule,None).await
    }
    /// A planner's exact observed read set stays locked through durable commit.
    pub async fn submit_deferred_rule_with_feedback(&self,generation:TaskGeneration,rule:DeferredRule,feedback:Option<crate::feedback::RuleLease>)->Result<AcceptedDeferred,WriteStatus> {
        self.submit_deferred_rule_before(generation,rule,feedback,None).await
    }
    pub(crate) async fn submit_deferred_rule_before(&self,generation:TaskGeneration,rule:DeferredRule,feedback:Option<crate::feedback::RuleLease>,deadline:Option<u64>)->Result<AcceptedDeferred,WriteStatus> {
        self.submit_deferred_rule_classified(generation,rule,feedback,deadline).await.map_err(DeferredFailure::status)
    }
    pub(crate) async fn submit_deferred_rule_classified(&self,generation:TaskGeneration,rule:DeferredRule,feedback:Option<crate::feedback::RuleLease>,deadline:Option<u64>)->Result<AcceptedDeferred,DeferredFailure> {
        self.submit_deferred_rule_classified_with_sources(generation,rule,feedback,deadline,None).await
    }
    pub(crate) async fn submit_deferred_rule_classified_with_sources(&self,generation:TaskGeneration,rule:DeferredRule,feedback:Option<crate::feedback::RuleLease>,deadline:Option<u64>,command_sources:Option<Vec<String>>)->Result<AcceptedDeferred,DeferredFailure> {
        self.submit_deferred_rule_for_entry(generation,rule,feedback,command_sources,RuleEntry{deadline,..Default::default()}).await
    }
    /// Admission must finish before the request expires; accepted delayed work
    /// keeps its original causal deadline, not the HTTP request's remaining TTL.
    pub(crate) async fn submit_deferred_rule_for_entry(&self,generation:TaskGeneration,rule:DeferredRule,feedback:Option<crate::feedback::RuleLease>,command_sources:Option<Vec<String>>,entry:RuleEntry)->Result<AcceptedDeferred,DeferredFailure> {
        let RuleEntry{deadline,admit_before,selection_reads,condition_reads,deactivated_scenes}=entry;
        let entry_reads=if selection_reads.as_ref().is_some_and(|r|!r.is_empty()) || !condition_reads.is_empty() || !deactivated_scenes.is_empty(){Some(Box::new(RuleEntryReads{selection:selection_reads,conditions:condition_reads,deactivated_scenes}))}else{None};
        let acceptance_limit=match (deadline,admit_before){(Some(a),Some(b))=>Some(a.min(b)),(a,None)=>a,(None,b)=>b};
        let manual_fence=self.manual_fence().await.map_err(DeferredFailure::Rejected)?;
        let controller=rule.batch.controller;
        let starts=rule.starts.clone();
        let command_ids=self.submit_scoped_before(Submission::Automation{entry_reads,software_sample:false,command_sources,batch:rule.batch,variables:rule.variables,audit:Some(rule.audit),starts:rule.starts,feedback:feedback.map(Box::new),manual_fence:None,scene:None},Some(SubmissionScope::Generation(generation.clone())),acceptance_limit).await.map_err(DeferredFailure::Rejected)?;
        let mut scenes=Vec::with_capacity(starts.len());
        for start in starts {
            let page=self.scene_history(controller,generation.clone(),homesense_storage::scene_run::Query::Run{boot:self.boot_id().into(),run:start.run.clone(),after:0,limit:1}).await.map_err(DeferredFailure::Uncertain)?.ok_or(DeferredFailure::Uncertain(WriteStatus::ServiceCommunicationFailure))?;
            if page.record.version!=0 || page.record.status!=homesense_storage::scene_run::Status::Pending || !page.commands.is_empty() {return Err(DeferredFailure::Uncertain(WriteStatus::ServiceCommunicationFailure));}
            scenes.push(AcceptedScene{generation:generation.clone(),controller,manual_fence,start,started_ms:page.record.started_ms,causal_deadline:deadline,source_authorization:self.source_authorization.clone()});
        }
        Ok(AcceptedDeferred{command_ids,scenes})
    }
    pub async fn submit_rule_update(
        &self,
        generation: TaskGeneration,
        batch: Batch,
        variables: Vec<homesense_storage::variables::Update>,
    ) -> Result<Vec<u64>, WriteStatus> {
        self.submit_scoped(
            Submission::Automation { entry_reads:None, software_sample: false, command_sources: None,
                batch,
                variables,
                manual_fence: None,
                feedback: None,
                audit: None,
                scene: None,
                starts: vec![],
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
    /// Motor intent from an immutable published graph. Resolve calibration and
    /// check the graph revision on the same owner turn as durable acceptance.
    pub async fn submit_motor_for_revision(
        &self,
        revision:u64,
        controller:Uuid,
        device:homesense_core::DeviceId,
        action:homesense_core::MotorAction,
    )->Result<Vec<u64>,WriteStatus> {
        self.submit_scoped(Submission::Motor{controller,device,action},Some(SubmissionScope::Configuration(revision))).await
    }
    /// Native card entry only. Mapping, source scope and current Written owner
    /// trajectory are resolved on one actor turn with durable queue acceptance.
    pub(crate) async fn submit_keypad_press_for_revision(&self,revision:u64,controller:Uuid,
        device:homesense_core::DeviceId,keypad:String,key:u8,deadline:u64)->Result<Vec<u64>,WriteStatus> {
        self.submit_scoped_before(Submission::KeypadPress{controller,device,keypad,key},Some(SubmissionScope::Configuration(revision)),Some(deadline)).await
    }
    /// Typed native panel entry; never make light submissions accept parameters.
    pub(crate) async fn submit_panel_command_for_revision(&self,revision:u64,controller:Uuid,
        device:homesense_core::DeviceId,command:homesense_core::Command,lease:crate::panel_auth::Lease)->Result<Vec<u64>,WriteStatus> {
        if !matches!(command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff|homesense_core::Command::PanelParameter(_)) {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        self.submit_scoped(Submission::PanelCommand{controller,device,command,lease},Some(SubmissionScope::Configuration(revision))).await
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
    pub async fn submit_away_off(&self,revision:u64,lease:impl Into<crate::away_auth::Authorization>,submit:bool)->Result<Vec<u64>,WriteStatus> {
        let lease=lease.into();
        let generation=self.task_generation().await?;
        let deadline=self.monotonic_ms().checked_add(self.ttl.as_millis() as u64).ok_or(WriteStatus::InvalidValueInRequest)?;
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::AwayOff(revision,generation,lease,deadline,submit,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
    pub(crate) fn with_source(&self,source:Option<crate::source_auth::Source>)->Self {let mut scoped=self.clone();scoped.source_authorization=source;scoped}
    pub(crate) fn source(&self)->Option<crate::source_auth::Source> {self.source_authorization.clone()}
    pub(crate) fn source_valid(&self)->bool {self.source_authorization.as_ref().is_none_or(|source|source.valid())}
    /// Coordinator-only snapshot for diagnostics after durable commit, including
    /// unavailable transport. This does not reopen control or update subscribers.
    pub(crate) async fn diagnostic_generation(&self,controller:Uuid,revision:u64)->Result<TaskGeneration,WriteStatus> {
        let (reply,receive)=oneshot::channel();
        self.sender.try_send(Message::DiagnosticGeneration(controller,revision,reply)).map_err(|_|WriteStatus::ResourceBusy)?;
        receive.await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?
    }
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
        self.submit_scoped_before(submission,generation,None).await
    }
    async fn submit_scoped_before(&self,submission:Submission,generation:Option<SubmissionScope>,limit:Option<u64>)->Result<Vec<u64>,WriteStatus> {
        self.submit_scoped_detailed(submission,generation,limit).await.map_err(SubmissionFailure::status)
    }
    async fn submit_scoped_detailed(&self,submission:Submission,generation:Option<SubmissionScope>,limit:Option<u64>)->Result<Vec<u64>,SubmissionFailure> {
        let deadline = millis(self.clock)
            .checked_add(
                u64::try_from(self.ttl.as_millis())
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?,
            )
            .ok_or(WriteStatus::ServiceCommunicationFailure)?;
        let deadline=limit.map_or(deadline,|limit|limit.min(deadline));
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Submit(Box::new(submission), self.source_authorization.clone(), generation, deadline, reply))
            .map_err(|_| WriteStatus::ResourceBusy)?;
        receive
            .await
            .map_err(|_| SubmissionFailure::Status(WriteStatus::ServiceCommunicationFailure))?
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
    async fn scene_fence_reports_only_actual_manual_rejection() {
        let directory=std::env::temp_dir().join(format!("homesense-fence-{}",Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path=directory.join("runtime.sqlite");
        Store::open(&path).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
        let actor=Admission::open(path,Options{context:ExecutionContext::Simulation,..Options::default()}, |_|true).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        let generation=actor.task_generation().await.unwrap();
        let fence=actor.manual_fence().await.unwrap();
        let controller=Uuid::new_v4();
        let batch=||Batch{controller,intents:vec![homesense_hap::requests::Intent{
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel {level:homesense_core::Percent::new(70).unwrap(),transition:homesense_core::LevelTransition::Immediate},
        }],subscriptions:vec![]};
        actor.submit(batch()).await.unwrap();
        let link=homesense_storage::scene_run::StepLink{sample:None,run:"not-created".into(),index:0,source:"synthetic".into()};
        // Actor rejects the old fence before touching the scene record or queue.
        assert_eq!(actor.submit_scene_step(generation.clone(),batch(),fence,link.clone(),None).await.unwrap_err(),SubmissionFailure::ManualSuperseded);
        // Expiration is distinct, even if a manual override also exists.
        assert_eq!(actor.submit_scene_step(generation,batch(),fence,link,Some(0)).await.unwrap_err(),SubmissionFailure::Status(WriteStatus::ServiceCommunicationFailure));
        let ticket=actor.dispatch().await.unwrap().unwrap();
        actor.finish_write(ticket.command.id,true).await.unwrap();
        assert!(actor.dispatch().await.unwrap().is_none());
        actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn diagnostics_remain_readable_after_failure_without_reopening_admission() {
        let directory=std::env::temp_dir().join(format!("homesense-diagnostics-{}",Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path=directory.join("runtime.sqlite");
        Store::open(&path).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();
        let actor=Admission::open(path,Options{context:ExecutionContext::Simulation,..Options::default()},move |id|id==controller).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        let generation=actor.task_generation().await.unwrap();
        let initial=actor.diagnostics(controller,generation.clone()).await.unwrap();
        assert_eq!(initial.wire_capacity,crate::WireCapacity{physical_frames_per_second:50,ordinary_reserved_frames_per_second:30,fade_frames_per_second:20});
        assert!(initial.gateway_ready && initial.storage_healthy && !initial.failed);
        assert!(initial.ready());
        for flag in 0..7 {
            let mut state=initial.clone();
            match flag {0=>state.failed=true,1=>state.stopping=true,2=>state.publishing=true,3=>state.gateway_ready=false,4=>state.controller_enabled=false,5=>state.storage_healthy=false,_=>state.valid_configuration_loaded=false}
            assert!(!state.ready(),"readiness must reject flag {flag}");
        }

        assert_eq!(initial.queued,0);assert!(!initial.in_flight);
        assert!(actor.diagnostics(Uuid::new_v4(),generation.clone()).await.is_err());
        let mut old=generation.clone();old.epochs.configuration+=1;
        assert!(actor.diagnostics(controller,old).await.is_err());
        let mut foreign=generation.clone();foreign.boot=Uuid::new_v4().to_string();
        assert!(actor.diagnostics(controller,foreign).await.is_err());
        actor.submit(Batch{controller,intents:vec![homesense_hap::requests::Intent{
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel{level:homesense_core::Percent::new(30).unwrap(),transition:homesense_core::LevelTransition::Immediate},
        }],subscriptions:vec![]}).await.unwrap();
        let queued=actor.diagnostics(controller,generation.clone()).await.unwrap();assert_eq!(queued.queued,1);assert!(!queued.in_flight);
        let _ticket=actor.dispatch().await.unwrap().unwrap();
        let writing=actor.diagnostics(controller,generation.clone()).await.unwrap();assert_eq!(writing.queued,0);assert!(writing.in_flight);
        actor.transport_failed().await.unwrap();
        let failed=actor.diagnostics(controller,generation).await.unwrap();
        assert!(failed.failed);assert!(!failed.storage_healthy);assert!(!failed.ready());
        assert!(actor.task_generation().await.is_err());
        assert_eq!(failed.queued,0);assert!(!failed.in_flight);
        actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn committed_offline_mapping_has_read_only_generation_without_control_readiness() {
        let directory=std::env::temp_dir().join(format!("homesense-diagnostic-mapping-{}",Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path=directory.join("runtime.sqlite");
        Store::open(&path).unwrap().publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();
        let actor=Admission::open(path,Options{context:ExecutionContext::Simulation,..Options::default()},move |id|id==controller).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        let original=actor.task_generation().await.unwrap();
        let mut configuration:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        configuration["revision"]=2.into();
        actor.begin_publication(serde_json::to_vec(&configuration).unwrap(),1,Duration::from_secs(2)).await.unwrap();
        assert!(actor.diagnostic_generation(controller,1).await.is_err());
        assert!(actor.diagnostic_generation(controller,2).await.is_err());
        assert_eq!(actor.commit_publication().await.unwrap(),2);
        assert!(actor.task_generation().await.is_err());
        assert!(actor.generation_updates().borrow().is_none());
        assert!(actor.diagnostic_generation(Uuid::new_v4(),2).await.is_err());
        assert!(actor.diagnostic_generation(controller,1).await.is_err());
        assert!(actor.diagnostics(controller,original).await.is_err());
        let generation=actor.diagnostic_generation(controller,2).await.unwrap();
        let state=actor.diagnostics(controller,generation.clone()).await.unwrap();
        assert_eq!(state.epochs.configuration,2);
        assert!(state.publishing,"physical synchronization still owns the publication control fence");
        assert!(!state.gateway_ready && !state.ready());
        let (control,_receiver)=crate::automation_control::channel(controller,generation.clone());
        let unavailable=control.unavailable(actor.clone(),generation.clone());
        assert!(matches!(unavailable.request(crate::automation_control::Action::Diagnostics{controller,revision:2}).await.unwrap(),crate::automation_control::Reply::Diagnostics(s) if !s.gateway_ready && !s.ready()));
        assert!(unavailable.request(crate::automation_control::Action::Start("synthetic".into())).await.is_err());
        let batch=Batch{controller,intents:vec![homesense_hap::requests::Intent{
            device:homesense_core::DeviceId::new("simulation:light").unwrap(),
            command:homesense_core::Command::SetLevel{level:homesense_core::Percent::new(30).unwrap(),transition:homesense_core::LevelTransition::Immediate},
        }],subscriptions:vec![]};
        assert!(actor.submit_delayed(generation,batch).await.is_err());
        assert!(actor.task_generation().await.is_err());
        assert!(actor.generation_updates().borrow().is_none());
        actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
    }

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

#[cfg(test)]
#[path = "transport_fault_reply_tests.rs"]
mod transport_fault_reply_tests;

#[cfg(test)]
mod partial_gateway_readiness_tests {
    #[test]
    fn partial_readability_never_reopens_global_failure_or_aggregate_readiness() {
        let base=super::Diagnostics {
            gateways:vec![crate::gateway_runtime::View {gateway:"synthetic-a".into(),connection_epoch:"7".into(),state:"offline"}],
            boot:"synthetic".into(),epochs:homesense_core::queue::Epochs {configuration:1,transport:1,control:1},
            queued:0,in_flight:false,failed:false,stopping:false,publishing:false,gateway_ready:false,
            controller_enabled:true,storage_healthy:true,valid_configuration_loaded:true,
            wire_capacity:crate::WireCapacity::for_write_interval(std::time::Duration::from_millis(20)),
        };
        assert!(base.mapping_readable());assert!(!base.ready());
        let mut legacy=base.clone();legacy.gateways.clear();assert!(!legacy.mapping_readable());
        for flag in 0..6 {
            let mut state=base.clone();
            match flag {0=>state.failed=true,1=>state.stopping=true,2=>state.publishing=true,3=>state.controller_enabled=false,4=>state.storage_healthy=false,_=>state.valid_configuration_loaded=false}
            assert!(!state.mapping_readable(),"global flag {flag} must stay fatal");assert!(!state.ready());
        }
    }
}

#[cfg(test)]
mod received_target_causality_tests {
    use super::*;
    use homesense_core::{DeviceId,Observation,Percent,Command,LevelTransition,gateway_scope::{Port,ReadRequirement,WireScope}};
    use homesense_hap::requests::{Batch,Intent};
    async fn fixture(supply:bool)->(Admission,homesense_config::Configuration,crate::feedback::Feedback,Uuid,PathBuf) {
        let path=std::env::temp_dir().join(format!("received-target-cause-{}.sqlite",Uuid::new_v4()));
        let mut value:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        value["gateways"][0]["id"]=serde_json::json!("a");
        let mut second=value["gateways"][0].clone();second["id"]=serde_json::json!("b");second["endpoint"]=serde_json::json!("127.0.0.1:50008");value["gateways"].as_array_mut().unwrap().push(second);
        value["devices"][0]["id"]=serde_json::json!("a-light");value["devices"][0]["gateway"]=serde_json::json!("a");
        let mut second=value["devices"][0].clone();second["id"]=serde_json::json!("b-light");second["gateway"]=serde_json::json!("b");value["devices"].as_array_mut().unwrap().push(second);
        if supply {
            value["gateways"][1]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            value["devices"][1]["supply_relay"]=serde_json::json!("b-relay");
            value["devices"][1]["default_on_level"]=serde_json::json!(100);
            value["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"b-relay","name":"Synthetic supply","gateway":"b","address":"03","channel":1,"enabled":true}));
        }
        let bytes=serde_json::to_vec(&value).unwrap();let config=homesense_config::Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
        Store::open(&path).unwrap().publish_configuration(None,&bytes,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let time=Box::leak(Box::new(AtomicU64::new(10)));
        let actor=Admission::open_with_test_time(path.clone(),Options {context:ExecutionContext::Simulation,..Default::default()},move |id|id==controller,time).await.unwrap();actor.synchronized(1,1).await.unwrap();
        let initial=config.gateways().iter().map(|(gateway,object)|crate::gateway_runtime::Initial {wire:WireScope::from_persisted(gateway.clone(),1).unwrap(),samples:object.modules().values().flat_map(|module|(1..=module.channels()).map(move|channel|crate::gateway_runtime::Sample {requirement:ReadRequirement {port:Port {address:module.address().get(),channel},before_count:0,sent_at_ms:0,deadline_ms:100},count:1,at:0})).collect()}).collect();actor.activate_gateways(initial).await.unwrap();
        let mut feedback=crate::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),1).unwrap();feedback.enable_local_wires([("a".into(),1),("b".into(),1)].into()).unwrap();
        (actor,config,feedback,controller,path)
    }
    fn sample(feedback:&mut crate::feedback::Feedback,address:u8,value:u8,at:u64) {
        feedback.apply(&crate::transport_owner::GatewayRead {gateway_id:"b".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![hb_protocol::RawFrame {address,command:0xd1,value}]}}).unwrap();
    }
    async fn close(actor:Admission,path:PathBuf) {
        actor.shutdown().await.unwrap();for suffix in ["","-wal","-shm"]{let _=std::fs::remove_file(format!("{}{suffix}",path.display()));}let _=std::fs::remove_file(path.with_extension("runtime.lock"));
    }
    #[tokio::test]
    async fn same_millisecond_match_requires_written_before_receive_and_preserves_later_external_value() {
        let(actor,config,mut feedback,controller,path)=fixture(false).await;let id=DeviceId::new("b-light").unwrap();let wire=WireScope::from_persisted("b".into(),1).unwrap();
        actor.submit(Batch {controller,intents:vec![Intent {device:id.clone(),command:Command::SetLevel {level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();let ticket=actor.dispatch().await.unwrap().unwrap();
        sample(&mut feedback,0x15,30,10);
        actor.received_projection(wire.clone(),vec![ticket.command.id],vec![id.clone()],10,feedback.snapshot()).await.unwrap();
        assert_eq!(actor.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap()[&id].get(),30,"a matching sample cannot settle Writing even with a claimed witness");
        actor.finish_write(ticket.command.id,true).await.unwrap();
        actor.received_projection(wire.clone(),vec![],vec![id.clone()],10,feedback.snapshot()).await.unwrap();
        assert!(actor.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap().contains_key(&id),"a receive begun before Writing completion keeps the original timestamp fence");
        actor.received_projection(wire,vec![ticket.command.id],vec![id.clone()],10,feedback.snapshot()).await.unwrap();
        assert!(actor.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap().is_empty());
        sample(&mut feedback,0x15,45,10);let targets=actor.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap();
        assert_eq!(crate::light_state::project_pending(&config,&feedback.snapshot(),&targets).unwrap().devices[&id].observed,Some(Observation::Level(Percent::new(45).unwrap())));
        close(actor,path).await;
    }
    #[tokio::test]
    async fn post_write_dimmer_match_does_not_bypass_combined_supply_settlement() {
        let(actor,_,mut feedback,controller,path)=fixture(true).await;let id=DeviceId::new("b-light").unwrap();let wire=WireScope::from_persisted("b".into(),1).unwrap();sample(&mut feedback,0x03,0xf1,9);
        actor.submit(Batch {controller,intents:vec![Intent {device:id.clone(),command:Command::SetLevel {level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();
        let mut written=vec![];while let Some(ticket)=actor.dispatch().await.unwrap(){actor.finish_write(ticket.command.id,true).await.unwrap();written.push(ticket.command.id);}
        sample(&mut feedback,0x15,30,10);actor.received_projection(wire,written,vec![id.clone()],10,feedback.snapshot()).await.unwrap();
        assert!(actor.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap().contains_key(&id),"old supply feedback cannot settle even a causally new dimmer match");
        close(actor,path).await;
    }
}
