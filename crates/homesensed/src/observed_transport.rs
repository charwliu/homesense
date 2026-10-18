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
    recovery_query_offset: usize,
    output_refresh: Option<OutputRefresh>,
    local_refresh:Option<local_refresh::Refresh>,
    local_fault_operations:Vec<homesense_storage::gateway_audit::OperationKey>,
    background_led_phase:bool,
}
#[path="refresh_round.rs"]
mod refresh_round;
use refresh_round::RoundTracker;
pub(crate) use refresh_round::CompletedRound;
#[path = "local_refresh.rs"]
mod local_refresh;
#[path = "periodic_refresh.rs"]
mod periodic_refresh;
use periodic_refresh::OutputRefresh;
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
    /// Drain the selected bus before a new query, even when its write slot is
    /// already ready. Unrelated buses must not add blocking reads to its lease.
    /// A received frame returns to planning before any query is admitted.
    async fn refresh_slot_receive_window(&mut self,gateway:&str)->Result<bool,(String,String)> {
        let until=tokio::time::Instant::now()
            + std::time::Duration::from_millis(self.transport.write_interval_ms().max(1));
        loop {
            let read=self.receive(gateway).await.map_err(|e|(gateway.into(),e))?;
            if matches!(read.result,hb_transport::Receive::Disconnected{..}) {
                return Err((gateway.into(),"gateway disconnected during refresh receive window".into()));
            }
            if !matches!(read.result,hb_transport::Receive::Idle) {return Ok(false);}
            if self.transport.query_write_slot_ready() {return Ok(true);}
            if tokio::time::Instant::now()>=until {return Ok(false);}
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
    pub(crate) async fn protected_controls(&self,controller:uuid::Uuid)->Result<bool,String> {
        let admission=self.transport.admission();let generation=admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
        admission.diagnostics(controller,generation).await.map(|state|state.ready() && state.protected_queued).map_err(|e|format!("{e:?}"))
    }
    pub(crate) fn pause_refresh_for_background_leds(&mut self,pause:bool) {self.background_led_phase=pause;}
    pub(crate) fn take_completed_refresh_round(&mut self)->Option<CompletedRound> {
        if let Some(plan)=&mut self.local_refresh {plan.round.take()}else{self.output_refresh.as_mut().and_then(|plan|plan.round.take())}
    }
    pub fn new(transport: TransportOwner<S>, feedback: Feedback) -> Self {
        let gateways = transport.gateway_ids();
        Self {
            transport,
            gateways,
            feedback,
            unavailable: false,
            projection_failed: false,
            pending_recovery: None,
            recovery_query_offset: 0,
            output_refresh: None,
            local_refresh:None,
            local_fault_operations:vec![],
            background_led_phase:false,
        }
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
        self.output_refresh = None;
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
        if selected.is_none() && self.pending_recovery.is_some() && !targets.is_empty() {
            let offset=self.recovery_query_offset%targets.len();
            targets.rotate_left(offset);
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
    fn physical_query_counts(
        &self,
        config: &homesense_config::Configuration,
        target: &crate::physical_queries::Target,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<Vec<(u8, u64)>, String> {
        if config.revision() != self.feedback.configuration_revision()
            || self.unavailable
            || timeout_ms == 0
            || timeout_ms > 5000
            || max_age_ms == 0
            || (if self.transport.local_mode() {
                self.transport.gateway_ids()!=config.gateways().keys().cloned().collect::<Vec<_>>()
                    ||!self.transport.local_connected(&target.gateway)
            }else{!self.transport.matches_generation(
                &config.gateways().keys().cloned().collect::<Vec<_>>(),
                self.feedback.transport_epoch(),
            )})
            || config
                .gateways()
                .get(&target.gateway)
                .and_then(|g| g.modules().get(&target.module.address()))
                != Some(&target.module)
        {
            return Err("physical query mapping or generation changed".into());
        }
        target
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
            .collect::<Result<Vec<_>, _>>()
    }
    async fn wait_for_query_slot(&mut self,gateway:&str)->Result<(),String> {
        while self.feedback.query_deferred(gateway,self.transport.now_ms(),true) || self.transport.query_response_pending(gateway) {
            if self.feedback.query_deferred(gateway,self.transport.now_ms(),true) || self.transport.awaiting_query_response(gateway) {
                self.receive(gateway).await?;
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            } else {
                // Completed replies only need turnaround time, not another
                // read that would turn an otherwise finished transaction into
                // a new response wait.
                self.transport.finish_reply_turnaround().await;
            }
        }
        Ok(())
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
        // Startup/recovery also protect an already-received press. Waiting
        // happens before query admission; no response deadline is renewed.
        self.wait_for_query_slot(&target.gateway).await?;
        let before = self.physical_query_counts(config, target, timeout_ms, max_age_ms)?;
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
                    self.wait_for_query_slot(&target.gateway).await?;
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
                let missing=before.iter().filter_map(|(channel,count)|self.feedback.module_truth().port_observation_count(&target.gateway,target.module.address().get(),*channel).is_none_or(|current|current<=*count).then_some(*channel)).collect::<Vec<_>>();
                log::warn!("physical_query_timeout gateway={:?} address={:02X} whole={} required={:?} missing={:?} transport_epoch={} configuration_revision={} timeout_ms={}",target.gateway,target.module.address().get(),target.whole,target.channels,missing,self.feedback.transport_epoch(),config.revision(),timeout_ms);
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
        self.transport.finish_status_read(&target.gateway);
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
    /// Retry fairness only. A new owner still needs every registered output at
    /// its own epoch; rotating order never reuses another owner's partial truth.
    pub(crate) fn set_recovery_query_offset(&mut self,offset:usize)->Result<(),String>{
        if self.pending_recovery.is_none() || self.unavailable || self.projection_failed {return Err("query rotation requires intact pending recovery".into());}
        self.recovery_query_offset=offset;Ok(())
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
    /// Only loss of transport/feedback is retryable. Invalid projection state
    /// is an invariant failure and must not be hidden by network retries.
    pub(crate) fn retryable_failure(&self)->bool {self.unavailable && !self.projection_failed}
    /// Only a completed durable receive-fault audit permits runtime handoff.
    /// Projection invariants, worker failures and cancelled operations stay fatal.
    pub(crate) fn audited_receive_failure_gateway(&self) -> Option<&str> {
        if self.retryable_failure() { self.transport.audited_receive_failure_gateway() } else { None }
    }

    pub(crate) fn invalidate(&mut self) {
        self.transport.finish_all_status_reads();
        self.background_led_phase=false;
        self.output_refresh = None;
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
        let activity=match &read.result {
            hb_transport::Receive::Frames {frames,..}=>frames.iter().filter_map(|frame| {
                let (_,channel,count)=crate::adaptive_refresh::candidate(operation.feedback.module_truth(),gateway,frame)?;
                (!self.output_refresh.as_ref().is_some_and(|plan|plan.solicited(gateway,frame.address,channel,read.received_at_ms)) && !self.transport.queued_query_solicited(gateway,*frame,read.received_at_ms)).then_some((frame.address,channel,count))
            }).collect::<Vec<_>>(),
            _=>vec![],
        };
        operation.feedback.apply(&read)?;
        let activity=activity.iter().any(|(address,channel,before)|operation.feedback.module_truth().port_observation_count(gateway,*address,*channel).is_some_and(|count|count>*before));
        *operation.unavailable = matches!(read.result, hb_transport::Receive::Disconnected { .. });
        operation.complete = true;
        if *operation.unavailable {self.output_refresh=None;self.transport.finish_status_read(gateway);}
        else if activity && let Some(plan)=&mut self.output_refresh {plan.activity(read.received_at_ms);}
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
        self.receive_pending_output().await?;
        self.step_after_refresh_read().await
    }
    /// The runtime has already classified the bounded pending read, including
    /// EOF, before this write attempt. Other callers use step().
    pub(crate) async fn step_after_refresh_read(&mut self) -> Result<Option<WriteOutcome>, String> {
        if self.unavailable {
            return Err("observed transport requires recovery".into());
        }
        let now=self.transport.now_ms();
        self.transport.pause_queries(self.gateways.iter().filter(|gateway|self.feedback.query_deferred(gateway,now,true)).cloned().collect());
        if self.transport.local_mode() {
            let mut blocked=self.local_refresh.as_ref().map_or_else(std::collections::BTreeSet::new,|plan|plan.blocked(self.transport.now_ms(),self.transport.write_interval_ms(),self.background_led_phase,&self.feedback));
            if !blocked.is_empty() && let Some(plan)=&self.local_refresh
                && self.transport.protected_controls_ready(plan.controller,plan.generation.clone()).await {
                // Yield only the next autonomous polling turn. Existing pending
                // responses remain blocked, and local stream/recovery plus key
                // and ramp read fences still apply to the queued dispatch.
                blocked=plan.blocked(self.transport.now_ms(),self.transport.write_interval_ms(),true,&self.feedback);
            }
            let outcome=self.transport.local_step(&blocked).await?;
            if self.transport.has_local_fault() {self.poll_local_events(false).await?;}
            if let Some(write)=&outcome && let Some((gateway,devices))=&write.managed_away_outputs {
                self.feedback.bulk_module_written(gateway.clone(),devices,self.transport.now_ms());
            }
            if let Some(write)=&outcome && write.result.is_ok() && let Some(plan)=&mut self.local_refresh {for (device,ramped) in &write.refresh_schedule{plan.written(device,self.transport.now_ms(),*ramped);}}
            return Ok(outcome);
        }
        // Keep a pending response serialized with later writes on this bus.
        // The runtime continues receiving, publishing and accepting requests;
        // accepted control tickets stay in Admission until the read resolves.
        if let Some(plan)=&self.output_refresh {
            if plan.awaiting_response() {return Ok(None);}
            if !self.background_led_phase && plan.defer_control(self.transport.now_ms(),self.transport.write_interval_ms(),&self.feedback)
                && !self.transport.protected_controls_ready(plan.controller,plan.generation.clone()).await {return Ok(None);}
        }
        let mut operation = Operation {
            feedback: &mut self.feedback,
            unavailable: &mut self.unavailable,
            projection_failed: &mut self.projection_failed,
            complete: false,
        };
        let outcome = self.transport.step().await?;
        if let Some(write)=&outcome && let Some((gateway,devices))=&write.managed_away_outputs {
            operation.feedback.bulk_module_written(gateway.clone(),devices,self.transport.now_ms());
            if let Some(plan)=&mut self.output_refresh {plan.away_written(gateway,devices,operation.feedback.module_truth(),self.transport.now_ms());}
        }
        if let Some(write)=&outcome && write.result.is_ok() && let Some(plan)=&mut self.output_refresh {for (device,ramped) in &write.refresh_schedule{plan.written(device,self.transport.now_ms(),*ramped);}}
        if outcome.as_ref().is_none_or(|value| value.result.is_ok() || value.projection_cancelled) {
            operation.complete = true;
        }
        if let Some(plan) = &mut self.output_refresh {
            plan.control_checked();
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
#[cfg(test)]
#[path="panel_activity_tests.rs"]
mod panel_activity_tests;
#[cfg(test)]
#[path="mixed_feedback_refresh_tests.rs"]
mod mixed_feedback_refresh_tests;
