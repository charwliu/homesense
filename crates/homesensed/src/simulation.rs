//! Configured offline control loop. Acceptance is in-memory and must never be
//! used as durable acceptance for a live HAP service.
use hb_protocol::{Decoded, Registry, StreamDecoder, encode_in_context};
use homesense_config::Configuration;
use homesense_core::{
    ExecutionContext,
    admission::{Access, RuntimePolicy},
    arbiter::Arbiter,
    execution::Execution,
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_hap::requests::Batch;
use homesense_simulator::{Faults, Gateway};
use homesense_state::{Snapshot, StateStore};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
struct Bus {
    gateway: Gateway,
    registry: Registry,
    decoder: StreamDecoder,
}
pub struct SimulationRuntime {
    configuration: Configuration,
    arbiter: Arbiter,
    states: StateStore,
    buses: BTreeMap<String, Bus>,
    controllers: BTreeSet<Uuid>,
    next_id: u64,
    now: u64,
    failed: bool,
}
impl SimulationRuntime {
    pub fn new(
        document: &[u8],
        controllers: impl IntoIterator<Item = Uuid>,
        capacity: usize,
    ) -> Result<Self, String> {
        let configuration =
            Configuration::parse(document, ExecutionContext::Simulation).map_err(error)?;
        let epochs = Epochs {
            transport: 1,
            configuration: configuration.revision(),
            control: 1,
        };
        let mut states =
            StateStore::new(1, 1, configuration.devices().len().max(1), 256).map_err(error)?;
        let mut buses = BTreeMap::new();
        for (name, gateway) in configuration.gateways() {
            let modules: Vec<_> = gateway.modules().values().cloned().collect();
            let mut registry = Registry::for_simulation();
            for module in &modules {
                registry.register(module.clone()).map_err(error)?;
            }
            buses.insert(
                name.clone(),
                Bus {
                    gateway: Gateway::new(modules, Faults::default(), 256).map_err(error)?,
                    registry,
                    decoder: StreamDecoder::default(),
                },
            );
        }
        for id in configuration.devices().keys() {
            if let Some((_, module)) = configuration.binding(id) {
                states.register(id.clone(), module.kind()).map_err(error)?;
            }
        }
        let arbiter = Arbiter::new(
            RuntimePolicy {
                epochs,
                publishing: false,
                controller: true,
                gateway_ready: true,
                storage_healthy: true,
                manual_degraded_enabled: false,
                valid_configuration_loaded: true,
            },
            capacity,
            2,
        )
        .map_err(error)?;
        Ok(Self {
            configuration,
            arbiter,
            states,
            buses,
            controllers: controllers.into_iter().collect(),
            next_id: 1,
            now: 0,
            failed: false,
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        self.states.snapshot()
    }
    pub fn events_since(
        &self,
        cursor: homesense_state::Cursor,
    ) -> Result<Vec<homesense_state::Event>, String> {
        self.states.events_since(cursor).map_err(error)
    }
    pub fn queued_len(&self) -> usize {
        self.arbiter.queued_len()
    }
    pub fn submit(&mut self, batch: Batch, now: u64) -> Result<(), String> {
        if self.failed || now < self.now || !self.controllers.contains(&batch.controller) {
            return Err("inactive runtime, backwards clock or unauthorized controller".into());
        }
        if batch.intents.len() > 128 {
            return Err("batch exceeds 128 commands".into());
        }
        let end = self
            .next_id
            .checked_add(batch.intents.len() as u64)
            .ok_or("command IDs exhausted")?;
        let deadline = now.checked_add(1000).ok_or("deadline overflow")?;
        let mut commands = Vec::new();
        for (offset, intent) in batch.intents.into_iter().enumerate() {
            if !matches!(
                intent.command,
                homesense_core::Command::Switch(_)
                    | homesense_core::Command::SetLevel {
                        transition: homesense_core::LevelTransition::Immediate,
                        ..
                    }
            ) {
                return Err(
                    "simulation HAP admission accepts immediate light commands only".into(),
                );
            }
            let (device, module) = self
                .configuration
                .binding(&intent.device)
                .ok_or("unknown or disabled device")?;
            // All capabilities are checked before queue mutation.
            encode_in_context(
                module,
                device.channel,
                intent.command,
                ExecutionContext::Simulation,
            )
            .map_err(error)?;
            commands.push((
                QueuedCommand {
                    id: self.next_id + offset as u64,
                    device_id: intent.device,
                    command: intent.command,
                    origin: Origin::Hap,
                    epochs: self.arbiter.policy().epochs,
                    deadline_ms: deadline,
                    run_id: None,
                },
                Access {
                    authorized: true,
                    device_exists: true,
                    device_verified: false,
                    existing_direct_manual: false,
                },
            ));
        }
        self.arbiter.submit_batch(commands, now).map_err(error)?;
        self.next_id = end;
        self.now = now;
        Ok(())
    }
    /// Drain accepted commands through the real codec and simulated bus. An
    /// unexpected error poisons the runtime, preventing implicit retries.
    pub fn advance(&mut self, now: u64) -> Result<(), String> {
        if self.failed || now < self.now {
            return Err("inactive runtime or backwards clock".into());
        }
        self.failed = true;
        self.now = now;
        loop {
            let dispatch = self
                .arbiter
                .dispatch(now, |_| Access {
                    authorized: true,
                    device_exists: true,
                    device_verified: false,
                    existing_direct_manual: false,
                })
                .map_err(error)?;
            let Some(command) = dispatch.command else {
                break;
            };
            let (device, module) = self
                .configuration
                .binding(&command.device_id)
                .ok_or("missing binding")?;
            let frame = encode_in_context(
                module,
                device.channel,
                command.command,
                ExecutionContext::Simulation,
            )
            .map_err(error)?;
            let bus = self
                .buses
                .get_mut(&device.gateway)
                .ok_or("missing gateway")?;
            self.states
                .set_desired(&command.device_id, command.id, command.command, now)
                .map_err(error)?;
            let mut execution = Execution::new(command.clone(), now).map_err(error)?;
            execution.begin_write(command.epochs, now).map_err(error)?;
            for chunk in frame.bytes().chunks(2) {
                for result in bus.gateway.receive(chunk, now).map_err(error)? {
                    result.map_err(error)?;
                }
            }
            execution.written(now).map_err(error)?;
            self.arbiter.finish_write(command.id, true).map_err(error)?;
            for chunk in bus.gateway.drain_due(now).map_err(error)? {
                for frame in bus.decoder.feed(&chunk) {
                    if let Decoded::Feedback {
                        address,
                        channel,
                        observation,
                    } = bus.registry.decode(frame)
                    {
                        for id in self.configuration.devices().keys() {
                            if let Some((mapped, _)) = self.configuration.binding(id)
                                && mapped.gateway == device.gateway
                                && mapped.address == address
                                && mapped.channel == channel
                            {
                                self.states
                                    .observe(id, observation, command.epochs.transport, now)
                                    .map_err(error)?;
                                if execution
                                    .observe(id, observation, command.epochs.transport, now)
                                    .map_err(error)?
                                {
                                    self.states
                                        .finish_pending(id, command.id, now)
                                        .map_err(error)?;
                                }
                            }
                        }
                    }
                }
            }
        }
        self.failed = false;
        Ok(())
    }
}
