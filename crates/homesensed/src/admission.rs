//! Dedicated bounded admission owner. No transport is dispatched by this actor.
use hap::server::WriteStatus;
use homesense_core::{
    Command, ExecutionContext, LevelTransition,
    admission::{Access, RuntimePolicy},
    arbiter::{Arbiter, ArbiterError, BatchCommitError},
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_hap::requests::Batch;
use homesense_storage::Store;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
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
enum Message {
    Shutdown(oneshot::Sender<()>),
    Submit(Batch, u64, oneshot::Sender<Result<Vec<u64>, WriteStatus>>),
    Ready(u64, u64, oneshot::Sender<Result<(), WriteStatus>>),
}
#[derive(Clone)]
pub struct Admission {
    sender: mpsc::Sender<Message>,
    boot: String,
    clock: Instant,
    ttl: Duration,
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
        let (sender, mut receiver) = mpsc::channel(options.mailbox_capacity);
        let (started, start) = oneshot::channel();
        let boot = Uuid::new_v4().to_string();
        let worker_boot = boot.clone();
        let clock = Instant::now();
        let authorize = Arc::new(authorize);
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
                    Ok((lease, store, configuration, arbiter))
                };
                let (lease, mut store, configuration, mut arbiter) = match initialize() {
                    Ok(v) => {
                        let _ = started.send(Ok(()));
                        v
                    }
                    Err(e) => {
                        let _ = started.send(Err(e));
                        return;
                    }
                };
                let mut next_id = 1u64;
                let mut failed = false;
                let mut stopped = None;
                while let Some(message) = receiver.blocking_recv() {
                    match message {
                        Message::Shutdown(reply) => {
                            stopped = Some(reply);
                            break;
                        }
                        Message::Ready(epoch, revision, reply) => {
                            let result = if failed {
                                Err(WriteStatus::ServiceCommunicationFailure)
                            } else {
                                arbiter
                                    .gateway_resynchronized(epoch, revision)
                                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)
                            };
                            let _ = reply.send(result);
                        }
                        Message::Submit(batch, deadline, reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            let result = (|| {
                                if failed || millis(clock) >= deadline {
                                    return Err(WriteStatus::ServiceCommunicationFailure);
                                }
                                if !authorize(batch.controller) {
                                    return Err(WriteStatus::InsufficientPrivileges);
                                }
                                if batch.intents.len() > 128 {
                                    return Err(WriteStatus::InvalidValueInRequest);
                                }
                                // Subscription-only requests still require authorization.
                                if batch.intents.is_empty() {
                                    return Ok(Vec::new());
                                }
                                let end = next_id
                                    .checked_add(batch.intents.len() as u64)
                                    .ok_or(WriteStatus::ServiceCommunicationFailure)?;
                                let mut commands = Vec::new();
                                for (offset, intent) in batch.intents.into_iter().enumerate() {
                                    if !matches!(
                                        intent.command,
                                        Command::Switch(_)
                                            | Command::SetLevel {
                                                transition: LevelTransition::Immediate,
                                                ..
                                            }
                                    ) {
                                        return Err(WriteStatus::InvalidValueInRequest);
                                    }
                                    let (device, module) = configuration
                                        .binding(&intent.device)
                                        .ok_or(WriteStatus::ResourceDoesNotExist)?;
                                    hb_protocol::encode_in_context(
                                        module,
                                        device.channel,
                                        intent.command,
                                        options.context,
                                    )
                                    .map_err(|_| WriteStatus::InvalidValueInRequest)?;
                                    commands.push((
                                        QueuedCommand {
                                            id: next_id + offset as u64,
                                            device_id: intent.device,
                                            command: intent.command,
                                            origin: Origin::Hap,
                                            epochs: arbiter.policy().epochs,
                                            deadline_ms: deadline,
                                            run_id: None,
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
                                        store.accept_queue_batch(
                                            &worker_boot,
                                            commands,
                                            cancelled,
                                            now,
                                        )
                                    },
                                );
                                match result {
                                    Ok(_) => {
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
        })
    }
    pub fn boot_id(&self) -> &str {
        &self.boot
    }
    pub async fn submit(&self, batch: Batch) -> Result<Vec<u64>, WriteStatus> {
        let deadline = millis(self.clock)
            .checked_add(
                u64::try_from(self.ttl.as_millis())
                    .map_err(|_| WriteStatus::ServiceCommunicationFailure)?,
            )
            .ok_or(WriteStatus::ServiceCommunicationFailure)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Message::Submit(batch, deadline, reply))
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
