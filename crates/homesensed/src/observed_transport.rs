//! Transport/feedback composition. A cancelled operation invalidates this owner.
use crate::{
    feedback::Feedback,
    transport_owner::{GatewayRead, TransportOwner, WriteOutcome},
};
use homesense_state::{Cursor, Event, Quality, Snapshot};
use std::io::{Read, Write};

pub struct ObservedTransport<S> {
    transport: TransportOwner<S>,
    gateways: Vec<String>,
    feedback: Feedback,
    unavailable: bool,
    projection_failed: bool,
    pending_recovery: Option<u64>,
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
        self.synchronize_mapping(revision,max_age_ms,false).await
    }
    async fn synchronize_mapping(&mut self,revision:u64,max_age_ms:u64,allow_keypads:bool)->Result<(),String> {
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
        let snapshot = self.snapshot();
        if snapshot.devices.is_empty()
            || snapshot.devices.values().filter(|state| !(allow_keypads && state.kind == homesense_core::ModuleKind::Keypad)).any(|state| {
                state.quality != Quality::Confirmed
                    || state.observed.is_none()
                    || !state
                        .observed_at_ms
                        .and_then(|at| now.checked_sub(at))
                        .is_some_and(|age| age < max_age_ms)
            })
        {
            return Err("fresh feedback required for the complete configured mapping".into());
        }
        self.transport.authorize_recovery(epoch, revision).await?;
        self.pending_recovery = None;
        Ok(())
    }
    /// Query supported outputs; only the explicit automation policy permits
    /// verified event-only keypads to remain unknown instead of being queried.
    async fn query_mapping(
        &mut self,
        config: &homesense_config::Configuration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
        selected: Option<&homesense_core::DeviceId>,
        allow_keypads: bool,
    ) -> Result<(), String> {
        if config.revision() != self.feedback.configuration_revision() {
            return Err("query revision does not match feedback".into());
        }
        if self.unavailable || timeout_ms == 0 || timeout_ms > 5000 || max_age_ms == 0 {
            return Err("invalid recovery query state or bounds".into());
        }
        let mut plan = Vec::new();
        let mut keypads = 0;
        for id in config.devices().keys() {
            if let Some((device, module)) = config.binding(id) {
                if allow_keypads && module.kind() == homesense_core::ModuleKind::Keypad {
                    module.validate_context(context).map_err(|e|e.to_string())?;
                    module.validate_channel(device.channel).map_err(|e|e.to_string())?;
                    keypads += 1;
                    continue;
                }
                hb_protocol::encode_in_context(
                    module,
                    device.channel,
                    homesense_core::Command::Query,
                    context,
                )
                .map_err(|e| e.to_string())?;
                plan.push((id.clone(), device, module));
            }
        }
        if plan.is_empty() || plan.len() + keypads != self.snapshot().devices.len() {
            return Err("recovery query mapping does not match feedback".into());
        }
        if let Some(id) = selected
            && !plan.iter().any(|(candidate, _, _)| candidate == id)
        {
            return Err("refresh device is not enabled in the mapping".into());
        }
        for (id, device, module) in plan {
            if selected.is_some_and(|selected| selected != &id) {
                continue;
            }
            let before = self
                .feedback
                .observation_count(&id)
                .ok_or("unknown recovery device")?;
            if let Err(error) = self
                .transport
                .query_status(&device.gateway, module, device.channel, context, timeout_ms)
                .await
            {
                self.invalidate();
                return Err(error);
            }
            let deadline = self
                .transport
                .now_ms()
                .checked_add(timeout_ms)
                .ok_or("query deadline overflow")?;
            let mut reads = 0;
            let mut idle_reads = 0;
            let mut empty_frame_reads = 0;
            let mut frame_count = 0;
            let mut matching_address_frames = 0;
            // Bound raw diagnostic history to the queried module, not the bus.
            let mut recent_frames = std::collections::VecDeque::with_capacity(16);
            let started = self.transport.now_ms();
            loop {
                if self.transport.now_ms() >= deadline || reads >= 128 {
                    self.invalidate();
                    log::warn!(
                        "gateway_query_timeout device={id:?} gateway={:?} timeout_ms={timeout_ms} elapsed_ms={} reads={reads} idle_reads={idle_reads} empty_frame_reads={empty_frame_reads} decoded_frames={frame_count} matching_address_frames={matching_address_frames} recent_module_frames={recent_frames:02X?}",
                        device.gateway,
                        self.transport.now_ms().saturating_sub(started)
                    );
                    return Err("recovery feedback timeout or read limit".into());
                }
                reads += 1;
                let read = self.receive(&device.gateway).await?;
                match &read.result {
                    hb_transport::Receive::Idle => idle_reads += 1,
                    hb_transport::Receive::Frames { frames, .. } => {
                        empty_frame_reads += usize::from(frames.is_empty());
                        frame_count += frames.len();
                        for frame in frames
                            .iter()
                            .filter(|frame| frame.address == device.address.get())
                        {
                            matching_address_frames += 1;
                            if recent_frames.len() == 16 {
                                recent_frames.pop_front();
                            }
                            recent_frames.push_back(frame.bytes());
                        }
                    }
                    hb_transport::Receive::Disconnected { .. } => {}
                }
                let snapshot = self.snapshot();
                let state = snapshot.devices.get(&id).ok_or("unknown recovery device")?;
                if self
                    .feedback
                    .observation_count(&id)
                    .is_some_and(|count| count > before)
                    && state.quality == Quality::Confirmed
                    && state.observed.is_some()
                {
                    log::debug!(
                        "gateway_query_observed device={id:?} gateway={:?} reads={reads}",
                        device.gateway
                    );
                    break;
                }
            }
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
        self.query_mapping(config, context, timeout_ms, max_age_ms, None, false)
            .await?;
        self.synchronize_recovery(config.revision(), max_age_ms)
            .await
    }
    /// Automation synchronizes queryable outputs; verified keypad observations
    /// remain unknown until fresh events arrive on this connection.
    pub async fn query_automation_recovery(
        &mut self, config:&homesense_config::Configuration,
        context:homesense_core::ExecutionContext, timeout_ms:u64,max_age_ms:u64,
    )->Result<(),String> {
        if self.pending_recovery.is_none() {return Err("no recovery pending".into());}
        self.query_mapping(config,context,timeout_ms,max_age_ms,None,true).await?;
        self.synchronize_mapping(config.revision(),max_age_ms,true).await
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
        self.query_mapping(config, context, timeout_ms, max_age_ms, Some(device), false)
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
    pub async fn drain_publication(&mut self, admission: &crate::admission::Admission) -> Result<Option<WriteOutcome>,String> {
        if !self.belongs_to(admission) || !admission.publication_in_progress().await.map_err(|e|format!("{e:?}"))? {
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
