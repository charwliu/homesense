//! Transport/feedback composition. A cancelled operation invalidates this owner.
use crate::{
    feedback::Feedback,
    transport_owner::{GatewayRead, TransportOwner, WriteOutcome},
};
use homesense_state::{Cursor, Event, Quality, Snapshot};
use std::io::{Read, Write};

/// Explicit recovery contracts. Exempt devices remain unobserved; this does not
/// infer motor position, direction, completion or original-controller state.
#[derive(Clone, Copy, Debug)]
pub enum Synchronization {
    CompleteFeedback,
    OutputsAndKeypadEvents,
    OutputsAndCalibratedMotors,
}
impl Synchronization {
    fn exempt(self, kind: homesense_core::ModuleKind) -> bool {
        match self {
            Self::CompleteFeedback => false,
            Self::OutputsAndKeypadEvents => kind == homesense_core::ModuleKind::Keypad,
            Self::OutputsAndCalibratedMotors => matches!(
                kind,
                homesense_core::ModuleKind::Keypad | homesense_core::ModuleKind::Motor
            ),
        }
    }
}

pub struct ObservedTransport<S> {
    transport: TransportOwner<S>,
    gateways: Vec<String>,
    feedback: Feedback,
    unavailable: bool,
    projection_failed: bool,
    pending_recovery: Option<u64>,
    output_refresh: Option<OutputRefresh>,
}
struct OutputRefresh {
    configuration: homesense_config::Configuration,
    targets: Vec<crate::physical_queries::Target>,
    controller: uuid::Uuid,
    generation: crate::admission::TaskGeneration,
    context: homesense_core::ExecutionContext,
    timeout_ms: u64,
    max_age_ms: u64,
    interval_ms: u64,
    next: usize,
    due: u64,
}
struct Operation<'a> {
    feedback: &'a mut Feedback,
    unavailable: &'a mut bool,
    projection_failed: &'a mut bool,
    complete: bool,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if !self.complete {
            *self.unavailable = true;
            if self.feedback.invalidate().is_err() {
                *self.projection_failed = true;
            }
        }
    }
}
impl<S: Read + Write + Send + 'static> ObservedTransport<S> {
    pub fn new(transport: TransportOwner<S>, feedback: Feedback) -> Self {
        let gateways = transport.gateway_ids();
        Self {
            transport,
            gateways,
            feedback,
            unavailable: false,
            projection_failed: false,
            pending_recovery: None,
            output_refresh: None,
        }
    }
    /// Opt-in periodic queries for output channels only. Event-only panels and
    /// motors retain their actual unknown/event state; no position is synthesized.
    pub(crate) fn configure_output_refresh(
        &mut self,
        configuration: &homesense_config::Configuration,
        controller: uuid::Uuid,
        generation: crate::admission::TaskGeneration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if configuration.revision() != self.feedback.configuration_revision()
            || generation.epochs.configuration != configuration.revision()
            || generation.epochs.transport != self.feedback.transport_epoch()
            || self.pending_recovery.is_some()
            || self.unavailable
            || timeout_ms == 0
            || timeout_ms > 5000
            || max_age_ms == 0
        {
            return Err("invalid output refresh generation or bounds".into());
        }
        self.feedback
            .module_truth()
            .project_logical(configuration)?;
        let targets = crate::physical_queries::plan(configuration, context)?;
        let interval_ms = crate::physical_queries::interval(
            &targets,
            self.transport.write_interval_ms(),
            max_age_ms,
        )?;
        self.output_refresh = Some(OutputRefresh {
            configuration: configuration.clone(),
            targets,
            controller,
            generation,
            context,
            timeout_ms,
            max_age_ms,
            interval_ms,
            next: 0,
            due: self.transport.now_ms().saturating_add(interval_ms),
        });
        Ok(())
    }
    /// One query per due cycle, interleaved with controls. Queries share the existing
    /// wire gap and require a fresh matching response. Failed refreshes hand off
    /// to the transport supervisor rather than extending observation timestamps.
    pub(crate) async fn refresh_outputs_if_due(&mut self) -> Result<(), (String, String)> {
        let Some(mut plan) = self.output_refresh.take() else {
            return Ok(());
        };
        if self.transport.now_ms() < plan.due {
            self.output_refresh = Some(plan);
            return Ok(());
        }
        let available = self
            .transport
            .idle_for_refresh(plan.controller, plan.generation.clone())
            .await;
        if !available {
            self.output_refresh = Some(plan);
            return Ok(());
        }
        let target = plan.targets[plan.next].clone();
        let gateway = target.gateway.clone();
        let result = self
            .query_physical(
                &plan.configuration,
                &target,
                plan.context,
                plan.timeout_ms,
                plan.max_age_ms,
                true,
            )
            .await;
        plan.next = (plan.next + 1) % plan.targets.len();
        // Anchor cadence to the scheduled start, not response completion. Skip
        // overdue slots rather than burst catch-up; wire spacing remains enforced.
        let now = self.transport.now_ms();
        plan.due = plan.due.saturating_add(plan.interval_ms);
        if plan.due <= now {
            plan.due = now;
        }
        self.output_refresh = Some(plan);
        result.map_err(|error| (gateway, error))
    }
    /// Initial live startup after the connector has faulted admission. No state
    /// is considered ready until query_recovery completes on these streams.
    pub(crate) fn new_recovering(
        transport: TransportOwner<S>,
        feedback: Feedback,
        epoch: u64,
    ) -> Result<Self, String> {
        if epoch < 2
            || feedback.transport_epoch() != epoch
            || !transport.matches_generation(&transport.gateway_ids(), epoch)
        {
            return Err("initial recovery generation mismatch".into());
        }
        let mut owner = Self::new(transport, feedback);
        owner.pending_recovery = Some(epoch);
        Ok(owner)
    }
    pub(crate) fn feedback(&self) -> &Feedback {
        &self.feedback
    }
    pub(crate) fn belongs_to(&self, admission: &crate::admission::Admission) -> bool {
        self.transport.belongs_to(admission)
    }
    pub(crate) fn write_interval_ms(&self) -> u64 {
        self.transport.write_interval_ms()
    }
    pub(crate) fn gateway_ids(&self) -> Vec<String> {
        self.gateways.clone()
    }
    /// Replace failed streams with a complete, fresh generation. This never
    /// synchronizes Admission or replays queued writes; the supervisor must
    /// validate/query the new connections before authorizing control.
    pub fn recover(&mut self, transport: TransportOwner<S>, epoch: u64) -> Result<(), String> {
        if !self.unavailable || self.projection_failed {
            return Err("recovery requires an unavailable, intact feedback projection".into());
        }
        if !self.transport.same_runtime(&transport)
            || !transport.matches_generation(&self.gateways, epoch)
        {
            return Err("recovery requires the same gateways at one fresh generation".into());
        }
        self.feedback.resume(epoch, transport.now_ms())?;
        self.transport = transport;
        self.unavailable = false;
        self.pending_recovery = Some(epoch);
        Ok(())
    }
    /// Reopen control only after every configured observation has been refreshed
    /// on the recovered generation. Snapshot data retained across EOF is stale
    /// and cannot satisfy this gate. Event-only or unknown devices require a
    /// separate synchronization policy and cannot silently pass this one.
    pub async fn synchronize_recovery(
        &mut self,
        revision: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        self.synchronize_mapping(revision, max_age_ms, Synchronization::CompleteFeedback)
            .await
    }
    async fn synchronize_mapping(
        &mut self,
        revision: u64,
        max_age_ms: u64,
        policy: Synchronization,
    ) -> Result<(), String> {
        if revision != self.feedback.configuration_revision() {
            return Err("recovery revision does not match feedback".into());
        }
        let epoch = self
            .pending_recovery
            .ok_or("no transport recovery pending")?;
        if self.unavailable || self.projection_failed || max_age_ms == 0 {
            return Err("recovery feedback unavailable".into());
        }
        let now = self.transport.now_ms();
        self.expire(now, max_age_ms)?;
        let truth = self.feedback.module_truth();
        let modules = truth.module_objects();
        let readable = modules
            .iter()
            .filter(|object| {
                matches!(
                    object.spec.kind(),
                    homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer
                )
            })
            .collect::<Vec<_>>();
        if readable.is_empty()
            || readable.iter().any(|object| {
                (1..=object.spec.channels()).any(|channel| {
                    truth
                        .located_state(&object.gateway, object.spec.address().get(), channel)
                        .is_none_or(|state| {
                            state.quality != Quality::Confirmed
                                || state.observed.is_none()
                                || state
                                    .observed_at_ms
                                    .and_then(|at| now.checked_sub(at))
                                    .is_none_or(|age| age >= max_age_ms)
                        })
                })
            })
        {
            return Err(
                "fresh feedback required for every registered output module channel".into(),
            );
        }
        if self
            .snapshot()
            .devices
            .values()
            .filter(|state| !policy.exempt(state.kind))
            .any(|state| {
                state.quality != Quality::Confirmed
                    || state.observed.is_none()
                    || state
                        .observed_at_ms
                        .and_then(|at| now.checked_sub(at))
                        .is_none_or(|age| age >= max_age_ms)
            })
        {
            return Err("fresh logical feedback or explicit event/motor exemption required".into());
        }
        self.transport.authorize_recovery(epoch, revision).await?;
        self.pending_recovery = None;
        Ok(())
    }
    /// Validate the complete plan before querying; exempt devices retain unknown
    /// observations and at least one queryable output is required.
    async fn query_mapping(
        &mut self,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
        selected: Option<&homesense_core::DeviceId>,
        policy: Synchronization,
    ) -> Result<(), String> {
        if config.revision() != self.feedback.configuration_revision() {
            return Err("query revision does not match feedback".into());
        }
        if self.unavailable || timeout_ms == 0 || timeout_ms > 5000 || max_age_ms == 0 {
            return Err("invalid recovery query state or bounds".into());
        }
        self.feedback.module_truth().project_logical(config)?;
        if !self.transport.matches_generation(
            &config.gateways().keys().cloned().collect::<Vec<_>>(),
            self.feedback.transport_epoch(),
        ) {
            return Err("physical query transport generation mismatch".into());
        }
        // Keep original explicit event/motor exemption and calibration gates.
        for id in config.devices().keys() {
            if let Some((device, module)) = config.binding(id)
                && !matches!(
                    module.kind(),
                    homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer
                )
            {
                if !policy.exempt(module.kind()) {
                    return Err("binding has no queryable output feedback".into());
                }
                module
                    .validate_context(context)
                    .map_err(|e| e.to_string())?;
                if module.kind() == homesense_core::ModuleKind::Motor {
                    let stop = config
                        .motor_command(id, homesense_core::MotorAction::Stop)
                        .map_err(|e| e.to_string())?;
                    hb_protocol::encode_in_context(module, device.channel, stop, context)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        let mut targets = crate::physical_queries::plan(config, context)?;
        if let Some(id) = selected {
            let (binding, _) = config.binding(id).ok_or("refresh device not enabled")?;
            targets.retain(|target| {
                target.gateway == binding.gateway
                    && target.module.address() == binding.address
                    && target.channels.contains(&binding.channel.get())
            });
            if targets.is_empty() {
                return Err("refresh device has no readable output target".into());
            }
        } else {
            crate::physical_queries::interval(
                &targets,
                self.transport.write_interval_ms(),
                max_age_ms,
            )?;
        }
        for target in targets {
            self.query_physical(
                config,
                &target,
                context,
                timeout_ms,
                max_age_ms,
                selected.is_some(),
            )
            .await?;
        }
        self.expire(self.transport.now_ms(), max_age_ms)?;
        Ok(())
    }
    pub(crate) async fn refresh_physical(
        &mut self,
        config: &homesense_config::Configuration,
        target: &crate::physical_queries::Target,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        self.query_physical(config, target, context, timeout_ms, max_age_ms, true)
            .await
    }
    async fn query_physical(
        &mut self,
        config: &homesense_config::Configuration,
        target: &crate::physical_queries::Target,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
        periodic: bool,
    ) -> Result<(), String> {
        if config.revision() != self.feedback.configuration_revision()
            || self.unavailable
            || timeout_ms == 0
            || timeout_ms > 5000
            || max_age_ms == 0
            || !self.transport.matches_generation(
                &config.gateways().keys().cloned().collect::<Vec<_>>(),
                self.feedback.transport_epoch(),
            )
            || config
                .gateways()
                .get(&target.gateway)
                .and_then(|g| g.modules().get(&target.module.address()))
                != Some(&target.module)
        {
            return Err("physical query mapping or generation changed".into());
        }
        let before = target
            .channels
            .iter()
            .map(|channel| {
                self.feedback
                    .module_truth()
                    .port_observation_count(
                        &target.gateway,
                        target.module.address().get(),
                        *channel,
                    )
                    .map(|count| (*channel, count))
                    .ok_or("physical query port missing".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sent = if target.whole {
            self.transport
                .query_status_scope(
                    &target.gateway,
                    &target.module,
                    target.first_channel(),
                    context,
                    timeout_ms,
                    true,
                )
                .await
        } else {
            self.transport
                .query_status(
                    &target.gateway,
                    &target.module,
                    target.first_channel(),
                    context,
                    timeout_ms,
                )
                .await
        };
        if let Err(error) = sent {
            self.invalidate();
            return Err(error);
        }
        let mut deadline = self
            .transport
            .now_ms()
            .checked_add(timeout_ms)
            .ok_or("query deadline overflow")?;
        let mut retry_available = periodic;
        loop {
            if self.transport.now_ms() >= deadline {
                if retry_available {
                    retry_available = false;
                    let retry_timeout = timeout_ms.min(500);
                    if let Err(error) = self
                        .transport
                        .query_status_scope(
                            &target.gateway,
                            &target.module,
                            target.first_channel(),
                            context,
                            retry_timeout,
                            target.whole,
                        )
                        .await
                    {
                        self.invalidate();
                        return Err(error);
                    }
                    deadline = self
                        .transport
                        .now_ms()
                        .checked_add(retry_timeout)
                        .ok_or("query deadline overflow")?;
                    continue;
                }
                self.invalidate();
                return Err("physical recovery feedback timeout".into());
            }
            self.receive(&target.gateway).await?;
            let truth = self.feedback.module_truth();
            if before.iter().all(|(channel, count)| {
                truth
                    .port_observation_count(
                        &target.gateway,
                        target.module.address().get(),
                        *channel,
                    )
                    .is_some_and(|current| current > *count)
            }) && target.fresh(truth, self.transport.now_ms(), max_age_ms)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        self.expire(self.transport.now_ms(), max_age_ms)?;
        Ok(())
    }
    pub async fn query_recovery(
        &mut self,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if self.pending_recovery.is_none() {
            return Err("no recovery pending".into());
        }
        self.query_mapping(
            config,
            context,
            timeout_ms,
            max_age_ms,
            None,
            Synchronization::CompleteFeedback,
        )
        .await?;
        self.synchronize_recovery(config.revision(), max_age_ms)
            .await
    }
    /// Automation synchronizes queryable outputs; verified keypad observations
    /// remain unknown until fresh events arrive on this connection.
    pub async fn query_automation_recovery(
        &mut self,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        self.query_recovery_with_policy(
            config,
            context,
            timeout_ms,
            max_age_ms,
            Synchronization::OutputsAndKeypadEvents,
        )
        .await
    }
    /// The calibrated motor policy still requires fresh queryable outputs and
    /// verified profiles. No query or state synthesis is performed for motors.
    pub async fn query_recovery_with_policy(
        &mut self,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
        policy: Synchronization,
    ) -> Result<(), String> {
        if self.pending_recovery.is_none() {
            return Err("no recovery pending".into());
        }
        self.query_mapping(config, context, timeout_ms, max_age_ms, None, policy)
            .await?;
        self.synchronize_mapping(config.revision(), max_age_ms, policy)
            .await
    }
    /// Refresh one observation so the supervisor can dispatch control between queries.
    /// Recovery still queries the complete mapping before reopening admission.
    pub(crate) async fn refresh(
        &mut self,
        device: &homesense_core::DeviceId,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if self.pending_recovery.is_some() {
            return Err("recovery requires synchronization first".into());
        }
        self.query_mapping(
            config,
            context,
            timeout_ms,
            max_age_ms,
            Some(device),
            Synchronization::CompleteFeedback,
        )
        .await
    }
    pub(crate) fn invalidate(&mut self) {
        self.unavailable = true;
        if self.feedback.invalidate().is_err() {
            self.projection_failed = true;
        }
    }
    pub fn expire(&mut self, now: u64, max_age_ms: u64) -> Result<(), String> {
        if self.unavailable {
            return Err("observed transport requires recovery".into());
        }
        if let Err(error) = self.feedback.expire(now, max_age_ms) {
            self.invalidate();
            return Err(error);
        }
        Ok(())
    }
    pub fn configuration_revision(&self) -> u64 {
        self.feedback.configuration_revision()
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.feedback.snapshot();
        // Only an exceptional event-counter failure needs a snapshot fallback.
        if self.projection_failed {
            for state in snapshot.devices.values_mut() {
                if state.quality == Quality::Confirmed {
                    state.quality = Quality::Stale;
                }
            }
        }
        snapshot
    }
    pub fn events_since(&self, cursor: Cursor) -> Result<Vec<Event>, String> {
        if self.projection_failed {
            return Err("feedback projection failed; resynchronization required".into());
        }
        self.feedback.events_since(cursor)
    }
    pub async fn receive(&mut self, gateway: &str) -> Result<GatewayRead, String> {
        if self.unavailable {
            return Err("observed transport requires recovery".into());
        }
        let mut operation = Operation {
            feedback: &mut self.feedback,
            unavailable: &mut self.unavailable,
            projection_failed: &mut self.projection_failed,
            complete: false,
        };
        let read = self.transport.receive(gateway).await?;
        operation.feedback.apply(&read)?;
        *operation.unavailable = matches!(read.result, hb_transport::Receive::Disconnected { .. });
        operation.complete = true;
        Ok(read)
    }
    /// Trusted publication coordinator only. The old mapping/streams remain
    /// available for the arbiter's reserved STOP queue after feedback invalidation.
    pub async fn drain_publication(
        &mut self,
        admission: &crate::admission::Admission,
    ) -> Result<Option<WriteOutcome>, String> {
        if !self.belongs_to(admission)
            || !admission
                .publication_in_progress()
                .await
                .map_err(|e| format!("{e:?}"))?
        {
            return Err("active publication barrier required".into());
        }
        self.transport.step().await
    }
    pub async fn step(&mut self) -> Result<Option<WriteOutcome>, String> {
        if self.unavailable {
            return Err("observed transport requires recovery".into());
        }
        let mut operation = Operation {
            feedback: &mut self.feedback,
            unavailable: &mut self.unavailable,
            projection_failed: &mut self.projection_failed,
            complete: false,
        };
        let outcome = self.transport.step().await?;
        if outcome.as_ref().is_none_or(|value| value.result.is_ok()) {
            operation.complete = true;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "module_query_tests.rs"]
mod module_query_tests;

#[cfg(test)]
#[path = "physical_startup_tests.rs"]
mod physical_startup_tests;
