//! One owner routes durable tickets to already-established bounded streams.
//! Construction never opens a socket or marks a gateway synchronized.
use crate::admission::Admission;
use hb_transport::{Connection, Receive, SendError};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

#[derive(Clone, Copy)]
enum ProjectionRefusal {
    Expired,
    Invalidated,
}
/// Minted only by the synchronous writer after a whole guarded LED or managed
/// Away batch was refused before any wire byte. It cannot be constructed by an API caller.
pub(crate) struct ProjectionDiscard {
    members: Vec<(u64, u64)>,
    reason: ProjectionRefusal,
    away: bool,
}
impl ProjectionDiscard {
    pub(crate) fn away(&self)->bool {self.away}
    pub(crate) fn members(&self) -> &[(u64, u64)] {
        &self.members
    }
    pub(crate) fn reason(&self) -> homesense_core::queue::CancelReason {
        match self.reason {
            ProjectionRefusal::Expired => homesense_core::queue::CancelReason::Expired,
            ProjectionRefusal::Invalidated => homesense_core::queue::CancelReason::RunCancelled,
        }
    }
}
struct SendAttempt {
    result: Result<(), SendError>,
    projection: Option<ProjectionDiscard>,
}
/// TCP completion only enqueues bytes in the gateway. At the documented
/// 9600/8N1 line rate the whole encoded batch must drain before lending the
/// shared write slot to another control or query. Existing longer intervals
/// still win; compressed module frames use their actual five-byte length.
fn post_write_interval(
    tickets: &[crate::admission::DispatchTicket],
    result: &Result<(), SendError>,
    configured: std::time::Duration,
) -> std::time::Duration {
    let bytes=result.as_ref().map_or_else(|error|error.bytes_written,|_| {
        hb_transport::encoded_batch_len(&tickets.iter().map(|ticket|&ticket.frame).collect::<Vec<_>>())
            .unwrap_or(hb_transport::MAX_BATCH_BYTES)
    });
    let serial_ms=(bytes as u64 * 10 * 1000).div_ceil(9600);
    configured.max(std::time::Duration::from_millis(serial_ms))
}
fn is_projection(ticket: &crate::admission::DispatchTicket) -> bool {
    matches!(
        ticket.command.command,
        homesense_core::Command::PanelLed(_) | homesense_core::Command::PanelLedOff
    ) && (ticket.feedback_barrier.is_some() || ticket.source_authorization.as_ref().is_some_and(|source|source.keypad().is_some()))
}
fn send_attempt(
    tickets: &[crate::admission::DispatchTicket],
    result: Result<(), SendError>,
    refusal: Option<ProjectionRefusal>,
) -> SendAttempt {
    // A feedback-gated completion LED has a shorter, sealed projection lease.
    // Its zero-byte proof uses the LED journal path (effective <= original
    // deadline); physical module-Off proofs still require the exact receipt.
    let away=tickets.iter().all(|ticket|ticket.feedback_barrier.is_none() && matches!(ticket.command.command,homesense_core::Command::ModuleAllOff|homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff) && ticket.away_authorization.as_ref().is_some_and(|lease|lease.guarded_operation()));
    let projection = if (away || tickets.iter().all(is_projection))
        && result.as_ref().is_err_and(|e| e.bytes_written == 0)
    {
        let reason = if result
            .as_ref()
            .is_err_and(|e| matches!(e.reason, hb_transport::SendFailure::Expired))
        {
            Some(ProjectionRefusal::Expired)
        } else {
            refusal
        };
        reason.map(|reason| ProjectionDiscard {
            away,
            members: tickets
                .iter()
                .map(|t| (t.command.id, t.command.deadline_ms))
                .collect(),
            reason,
        })
    } else {
        None
    };
    SendAttempt { result, projection }
}
#[cfg(test)]
pub(crate) fn send_ticket<S: Read + Write>(
    ticket: &crate::admission::DispatchTicket,
    connection: &mut Connection<S>,
    now: impl FnMut() -> u64,
) -> Result<(), SendError> {
    send_one(ticket, connection, now).result
}
#[cfg(test)]
pub(crate) fn send_ticket_batch<S: Read + Write>(
    tickets: &[crate::admission::DispatchTicket], connection: &mut Connection<S>,
    now: impl FnMut() -> u64,
) -> Result<(), SendError> { send_tickets(tickets, connection, now).result }
#[cfg(test)]
pub(crate) fn send_guarded_ticket<S:Read+Write>(ticket:&crate::admission::DispatchTicket,connection:&mut Connection<S>,now:impl FnMut()->u64)->(Result<(),SendError>,Option<ProjectionDiscard>) {
    let attempt=send_one(ticket,connection,now);(attempt.result,attempt.projection)
}
fn send_one<S: Read + Write>(
    ticket: &crate::admission::DispatchTicket,
    connection: &mut Connection<S>,
    mut now: impl FnMut() -> u64,
) -> SendAttempt {
    let valid_authorities = match (
        &ticket.panel_authorization,
        &ticket.source_authorization,
        &ticket.publication_stop,
        &ticket.away_authorization,
    ) {
        (Some(_), None, None, None)
        | (None, Some(_), None, None)
        | (None, None, None, Some(_))
        | (None, None, None, None) => true,
        (None, None, Some(lease), None) => lease.matches_write(&ticket.command),
        _ => false,
    };
    if !valid_authorities {
        return SendAttempt {
            result: Err(SendError {
                reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
                bytes_written: 0,
            }),
            projection: None,
        };
    }
    let at = now();
    let refusal = std::cell::Cell::new(None);
    let denied = || {
        refusal.set(Some(if at >= ticket.command.deadline_ms {
            ProjectionRefusal::Expired
        } else {
            ProjectionRefusal::Invalidated
        }));
        Err(SendError {
            reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
            bytes_written: 0,
        })
    };
    let send = || connection.send(&ticket.frame, ticket.command.deadline_ms, now);
    // Preserve feedback -> source lock ordering through the complete write.
    let authorized = || match (
        &ticket.panel_authorization,
        &ticket.source_authorization,
        &ticket.publication_stop,
        &ticket.away_authorization,
    ) {
        (Some(lease), None, None, None) => lease.with_valid(send).unwrap_or_else(denied),
        (None, Some(source), None, None) => source.with_valid_command(&ticket.command,send).unwrap_or_else(denied),
        (None, None, None, Some(lease)) => lease.with_valid(send).unwrap_or_else(denied),
        (None, None, Some(lease), None) if lease.matches_write(&ticket.command) => send(),
        (None, None, None, None) => send(),
        // A malformed authority combination is not a proven lease refusal.
        _ => Err(SendError {
            reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
            bytes_written: 0,
        }),
    };
    let result = match &ticket.feedback_barrier {
        Some(lease) => lease.with_valid(at, authorized).unwrap_or_else(denied),
        None => authorized(),
    };
    send_attempt(std::slice::from_ref(ticket), result, refusal.get())
}
fn send_tickets<S: Read + Write>(
    tickets: &[crate::admission::DispatchTicket],
    connection: &mut Connection<S>,
    mut now: impl FnMut() -> u64,
) -> SendAttempt {
    if tickets.len() == 1 {
        return send_one(&tickets[0], connection, now);
    }
    let first = &tickets[0];
    if (first.source_authorization.is_some() && first.away_authorization.is_some())
        || tickets.iter().any(|ticket| {
            ticket.gateway_id != first.gateway_id
                || ticket.panel_authorization.is_some()
                || ticket.publication_stop.is_some()
                || match (&first.away_authorization, &ticket.away_authorization) {
                    (Some(a), Some(b)) => !a.same_authority(b),
                    (None, None) => false,
                    _ => true,
                }
                || match (&first.source_authorization, &ticket.source_authorization) {
                    (Some(a), Some(b)) => !a.same_authority(b),
                    (None, None) => false,
                    _ => true,
                }
        })
    {
        return SendAttempt {
            result: Err(SendError {
                reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
                bytes_written: 0,
            }),
            projection: None,
        };
    }
    let at = now();
    let refusal = std::cell::Cell::new(None);
    let deadline = tickets.iter().map(|t| t.command.deadline_ms).min().unwrap();
    let denied = || {
        refusal.set(Some(if at >= deadline {
            ProjectionRefusal::Expired
        } else {
            ProjectionRefusal::Invalidated
        }));
        Err(SendError {
            reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
            bytes_written: 0,
        })
    };
    let frames = tickets.iter().map(|t| &t.frame).collect::<Vec<_>>();
    let send = || connection.send_batch(&frames, deadline, now);
    let authorized = || match (&first.source_authorization, &first.away_authorization) {
        (Some(source), None) => source.with_valid_command(&first.command,send).unwrap_or_else(denied),
        (None, Some(away)) => away.with_valid(send).unwrap_or_else(denied),
        (None, None) => send(),
        _ => Err(SendError {
            reason: hb_transport::SendFailure::Io(std::io::ErrorKind::PermissionDenied),
            bytes_written: 0,
        }),
    };
    let result = match &first.feedback_barrier {
        Some(lease) => lease.with_valid(at, authorized).unwrap_or_else(denied),
        None => authorized(),
    };
    send_attempt(tickets, result, refusal.get())
}

pub struct TransportOwner<S> {
    read_permits: BTreeMap<String, crate::admission::QueryReadPermit>,
    query_replies:crate::query_response::Replies,
    query_paused:std::collections::BTreeSet<String>,
    admission: Admission,
    connections: BTreeMap<String, Connection<S>>,
    failed: bool,
    audited_receive_failure: Option<String>,
    audited_write_failure: Option<String>,
    local: Option<gateway_io::Local<S>>,
    #[cfg(test)]
    worker_spawned: Option<tokio::sync::oneshot::Sender<()>>,
}
#[path = "gateway_io.rs"]
mod gateway_io;
pub(crate) use gateway_io::{Connector, network_send_failure};
struct OperationGuard {
    admission: Admission,
    complete: bool,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.admission.stop_now();
        }
    }
}
pub struct GatewayRead {
    pub gateway_id: String,
    pub transport_epoch: u64,
    pub received_at_ms: u64,
    pub result: Receive,
}
pub struct WriteOutcome {
    pub device_id: homesense_core::DeviceId,
    pub command_id: u64,
    pub result: Result<(), SendError>,
    /// Proven zero-byte guarded LED/Away cancellation, not a successful write.
    pub projection_cancelled: bool,
    pub completed: Vec<(u64, homesense_core::DeviceId)>,
    pub physical_frames: usize,
    pub refresh_devices: Vec<homesense_core::DeviceId>,
    pub(crate) refresh_schedule: Vec<(homesense_core::DeviceId,bool)>,
    pub(crate) managed_away_outputs: Option<(String, Vec<homesense_core::DeviceId>)>,
}
pub(crate) fn managed_away_outputs(tickets:&[crate::admission::DispatchTicket],success:bool)->Option<(String,Vec<homesense_core::DeviceId>)> {
    let modules=tickets.iter().filter(|ticket|ticket.command.command==homesense_core::Command::ModuleAllOff
        && ticket.away_authorization.as_ref().is_some_and(|lease|lease.guarded_operation())).collect::<Vec<_>>();
    (success && !modules.is_empty()).then(||(modules[0].gateway_id.clone(),modules.iter().flat_map(|ticket|ticket.refresh_devices.iter().cloned()).collect()))
}
impl<S: Read + Write + Send + 'static> TransportOwner<S> {
    pub fn new(admission: Admission, connections: BTreeMap<String, Connection<S>>) -> Self {
        Self {
            read_permits: Default::default(),
            query_replies:Default::default(),
            query_paused:Default::default(),
            admission,
            connections,
            failed: false,
            audited_receive_failure: None,
            audited_write_failure: None,
            local: None,
            #[cfg(test)]
            worker_spawned: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn notify_next_worker_spawned(&mut self, notification:tokio::sync::oneshot::Sender<()>) {
        self.worker_spawned=Some(notification);
    }
    async fn collect_tickets(
        &self,
        first: crate::admission::DispatchTicket,
        frame_budget: usize,
    ) -> Result<Vec<crate::admission::DispatchTicket>, String> {
        let id = first.command.id;
        let protected = first.command.command==homesense_core::Command::Query || first.panel_authorization.is_some() || first.publication_stop.is_some();
        let mut frames = first.frame.physical_frames();
        if frames > frame_budget {
            self.admission
                .finish_unsent(id)
                .await
                .map_err(|e| format!("unfit operation audit: {e:?}"))?;
            return Err("complete operation exceeds remaining physical frame budget".into());
        }
        let mut tickets = vec![first];
        if protected {
            return Ok(tickets);
        }
        if frames < frame_budget {
            let tail=self.admission.extend_write_batch(id,frame_budget-frames).await
                .map_err(|e|format!("batch extension: {e:?}"))?;
            for ticket in tail {
                frames+=ticket.frame.physical_frames();
                if frames>frame_budget{return Err("batch physical frame budget exceeded".into());}
                tickets.push(ticket);
            }
        }
        Ok(tickets)
    }
    /// Audit every whole-operation prefix, then zero-byte suffixes. Partial
    /// operation ownership remains Writing for the transport fault transaction.
    async fn audit_batch_progress(
        &self,
        tickets: &[crate::admission::DispatchTicket],
        result: &Result<(), SendError>,
    ) -> Result<Vec<(u64, homesense_core::DeviceId)>, String> {
        let refs = tickets.iter().map(|t| &t.frame).collect::<Vec<_>>();
        let written = result.as_ref().map_or_else(
            |e| e.bytes_written,
            |_| hb_transport::encoded_batch_len(&refs).unwrap_or(usize::MAX),
        );
        let progress = hb_transport::operation_progress(&refs, written)
            .ok_or("invalid batch byte progress")?;
        let complete_ids = tickets.iter().zip(&progress)
            .filter_map(|(ticket, state)| matches!(state, hb_transport::OperationProgress::Complete)
                .then_some(ticket.command.id)).collect::<Vec<_>>();
        if !complete_ids.is_empty() {
            self.admission.finish_writes(complete_ids).await
                .map_err(|e| format!("complete transport batch: {e:?}"))?;
        }
        let mut completed = vec![];
        for (ticket, state) in tickets.iter().zip(progress) {
            match state {
                hb_transport::OperationProgress::Complete => {
                    completed.push((ticket.command.id, ticket.command.device_id.clone()));
                }
                hb_transport::OperationProgress::Unsent => self
                    .admission
                    .finish_unsent(ticket.command.id)
                    .await
                    .map_err(|e| format!("unsent batch member: {e:?}"))?,
                hb_transport::OperationProgress::Partial => {}
            }
        }
        Ok(completed)
    }
    /// One shared write slot for queries and expanded control frames. Waiting
    /// precedes dispatch so STOP priority and deadlines are checked afterwards.
    async fn wait_write_slot(&self) -> tokio::sync::OwnedMutexGuard<Option<tokio::time::Instant>> {
        let slot = self.admission.write_slot.clone().lock_owned().await;
        if let Some(deadline) = *slot {
            tokio::time::sleep_until(deadline).await;
        }
        slot
    }
    pub(crate) fn admission(&self) -> &Admission {
        &self.admission
    }
    pub(crate) fn belongs_to(&self, admission: &Admission) -> bool {
        self.admission.boot_id() == admission.boot_id()
    }
    pub(crate) fn audited_receive_failure_gateway(&self) -> Option<&str> {
        self.audited_receive_failure.as_deref()
    }
    pub(crate) fn audited_write_failure_gateway(&self) -> Option<&str> {
        self.audited_write_failure.as_deref()
    }
    pub(crate) fn gateway_ids(&self) -> Vec<String> {
        self.local.as_ref().map_or_else(
            || self.connections.keys().cloned().collect(),
            |local| local.epochs.keys().cloned().collect(),
        )
    }
    pub(crate) fn same_runtime(&self, other: &Self) -> bool {
        self.admission.boot_id() == other.admission.boot_id()
    }
    pub(crate) fn now_ms(&self) -> u64 {
        self.admission.monotonic_ms()
    }
    pub(crate) fn matches_generation(&self, gateways: &[String], epoch: u64) -> bool {
        !self.failed
            && self.gateway_ids() == gateways
            && !self.connections.is_empty()
            && self
                .connections
                .values()
                .all(|connection| connection.epoch() == epoch)
    }
    pub(crate) async fn authorize_recovery(&self, epoch: u64, revision: u64) -> Result<(), String> {
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        let result = self
            .admission
            .recover_synchronized(epoch, revision)
            .await
            .map_err(|e| format!("recovery admission: {e:?}"));
        guard.complete = true;
        result
    }
    pub(crate) fn write_interval_ms(&self) -> u64 {
        self.admission.write_interval.as_millis() as u64
    }
    pub(crate) fn query_write_slot_ready(&self) -> bool {
        self.admission.write_slot.try_lock().is_ok_and(|slot|
            slot.is_none_or(|until| tokio::time::Instant::now() >= until))
    }
    pub(crate) async fn idle_for_refresh(
        &self,
        controller: uuid::Uuid,
        generation: crate::admission::TaskGeneration,
    ) -> bool {
        self.admission
            .diagnostics(controller, generation)
            .await
            .is_ok_and(|state| state.ready() && !state.in_flight && !state.protected_queued)
    }
    pub(crate) async fn protected_controls_ready(
        &self,
        controller: uuid::Uuid,
        generation: crate::admission::TaskGeneration,
    ) -> bool {
        self.admission
            .diagnostics(controller, generation)
            .await
            .is_ok_and(|state| state.ready() && !state.in_flight && state.protected_queued)
    }
    /// Retrying the same bounded read must not be blocked by a control queued
    /// after that read began. New reads still defer to protected controls.
    pub(crate) async fn can_retry_read(
        &self,
        controller: uuid::Uuid,
        generation: crate::admission::TaskGeneration,
    ) -> bool {
        self.admission
            .diagnostics(controller, generation)
            .await
            .is_ok_and(|state| state.ready() && !state.in_flight)
    }
    /// Internal synchronization query only. PreparedFrame validates capability,
    /// channel, verification and execution context before any bytes are sent.
    pub(crate) async fn query_status(
        &mut self,
        gateway: &str,
        module: &homesense_core::ModuleSpec,
        channel: homesense_core::Channel,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
    ) -> Result<(), String> {
        self.query_status_scope(gateway, module, channel, context, timeout_ms, false)
            .await
    }
    pub(crate) fn pause_queries(&mut self,paused:std::collections::BTreeSet<String>) { self.query_paused=paused; }
    pub(crate) fn queued_query_gateway(&self)->Option<String> { self.query_replies.next(self.now_ms()) }
    pub(crate) fn query_response_pending(&self,gateway:&str)->bool {self.query_replies.waiting(gateway,self.now_ms())}
    pub(crate) fn awaiting_query_response(&self,gateway:&str)->bool {self.query_replies.awaiting(gateway,self.now_ms())}
    pub(crate) async fn finish_reply_turnaround(&mut self) {
        if let Some(delay)=self.query_replies.turnaround(self.now_ms()) {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    }
    pub(crate) fn queued_query_solicited(&self,gateway:&str,frame:hb_protocol::RawFrame,at:u64)->bool {self.query_replies.solicited(gateway,frame,at)}
    fn track_query_reply(&mut self,tickets:&mut [crate::admission::DispatchTicket]) {
        for ticket in tickets { if let Some(scope)=ticket.query_reply.take() {
            self.query_replies.start(ticket.gateway_id.clone(),scope,self.now_ms(),self.write_interval_ms());
        } }
    }
    pub(crate) fn finish_status_read(&mut self, gateway: &str) {
        self.read_permits.remove(gateway);
        // Keep the completed reply's bounded turnaround window. Dropping the
        // query permit must not immediately authorize a colliding writer.
    }
    pub(crate) fn finish_all_status_reads(&mut self) {
        self.read_permits.clear();
        self.query_replies.clear();
        self.query_paused.clear();
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_query_status_scope(
        &mut self,
        gateway: &str,
        module: &homesense_core::ModuleSpec,
        channel: homesense_core::Channel,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        whole_module: bool,
    ) -> Result<bool, String> {
        self.send_status_scope(
            gateway,
            module,
            channel,
            context,
            timeout_ms,
            whole_module,
            true,
            false,
        )
        .await
    }
    /// A new refresh query yields its write-slot wait to the receive loop.
    /// Retrying an already issued query uses the original bounded path instead.
    pub(crate) async fn poll_query_status_scope(
        &mut self,
        gateway: &str,
        module: &homesense_core::ModuleSpec,
        channel: homesense_core::Channel,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        whole_module: bool,
    ) -> Result<bool, String> {
        self.send_status_scope(gateway, module, channel, context, timeout_ms,
            whole_module, true, true).await
    }
    pub(crate) async fn query_status_scope(
        &mut self,
        gateway: &str,
        module: &homesense_core::ModuleSpec,
        channel: homesense_core::Channel,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        whole_module: bool,
    ) -> Result<(), String> {
        if self
            .send_status_scope(
                gateway,
                module,
                channel,
                context,
                timeout_ms,
                whole_module,
                false,
                false,
            )
            .await?
        {
            Ok(())
        } else {
            Err("status read deferred".into())
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn send_status_scope(
        &mut self,
        gateway: &str,
        module: &homesense_core::ModuleSpec,
        channel: homesense_core::Channel,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        whole_module: bool,
        deferrable: bool,
        yield_slot: bool,
    ) -> Result<bool, String> {
        if self.failed || timeout_ms == 0 || timeout_ms > 5000 {
            return Err("invalid query owner or timeout".into());
        }
        if self.query_replies.waiting(gateway,self.now_ms()) {
            return if deferrable {Ok(false)} else {Err("query reply or release still pending".into())};
        }
        let epoch = self
            .connections
            .get(gateway)
            .ok_or("missing query gateway")?
            .epoch();
        let frame = if whole_module {
            hb_transport::PreparedFrame::prepare_module_query(module, context, epoch)
        } else {
            hb_transport::PreparedFrame::prepare(
                module,
                channel,
                homesense_core::Command::Query,
                context,
                epoch,
            )
        }
        .map_err(|e| e.to_string())?;
        // Periodic/local refresh is called by the same task that applies
        // incoming button frames. Do not park that receiver while a previous
        // writer still owns the slot or its turnaround interval has not ended.
        let mut slot = if yield_slot {
            let Ok(slot) = self.admission.write_slot.clone().try_lock_owned() else {
                return Ok(false);
            };
            if slot.is_some_and(|until| tokio::time::Instant::now() < until) {
                return Ok(false);
            }
            slot
        } else {
            self.wait_write_slot().await
        };
        let budget_end_ms = self
            .now_ms()
            .checked_add(timeout_ms)
            .and_then(|end| end.checked_add(timeout_ms.min(500)))
            .ok_or("query budget overflow")?;
        if !self.read_permits.contains_key(gateway) {
            match self
                .admission
                .reserve_module_read(gateway.into(), Some(module.address().get()), budget_end_ms)
                .await
            {
                Ok(permit) => {
                    self.read_permits.insert(gateway.into(), permit);
                }
                Err(hap::server::WriteStatus::ResourceBusy) if deferrable => return Ok(false),
                Err(error) => return Err(format!("status read reservation: {error:?}")),
            }
        }
        let deadline = self
            .now_ms()
            .checked_add(timeout_ms)
            .ok_or("query deadline overflow")?;
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        self.failed = true;
        let mut connection = self
            .connections
            .remove(gateway)
            .ok_or("missing query gateway")?;
        let clock = self.admission.clone();
        let (connection, sent) = tokio::task::spawn_blocking(move || {
            let sent = connection.send(&frame, deadline, || clock.monotonic_ms());
            *slot = Some(tokio::time::Instant::now() + clock.write_interval);
            (connection, sent)
        })
        .await
        .map_err(|e| format!("query worker: {e}"))?;
        if let Err(error) = sent {
            if self.local_mode() && network_send_failure(&error) {
                let wire =
                    homesense_core::gateway_scope::WireScope::from_persisted(gateway.into(), epoch)
                        .map_err(|e| e.to_string())?;
                let audit = self
                    .admission
                    .gateway_fault(wire.clone())
                    .await
                    .map_err(|e| format!("local query audit: {e:?}"))?;
                self.local_record_fault(wire, audit)?;
                self.failed = false;
                guard.complete = true;
                return Err(format!("local query send: {error:?}"));
            }
            let _ = self.admission.transport_failed().await;
            guard.complete = !self.local_mode();
            return Err(format!("query send: {error:?}"));
        }
        self.connections.insert(gateway.to_owned(), connection);
        self.query_replies.start_with_timeout(gateway.to_owned(),
            crate::query_response::Scope::new(module,if whole_module {None}else{Some(channel.get())},context,epoch)?,self.now_ms(),timeout_ms,self.write_interval_ms());
        self.failed = false;
        guard.complete = true;
        Ok(true)
    }
    /// One bounded read. Unknown frames remain raw until configuration decoding.
    pub async fn receive(&mut self, gateway: &str) -> Result<GatewayRead, String> {
        if self.failed {
            return Err("transport owner requires recovery".into());
        }
        self.audited_receive_failure = None;
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        self.failed = true;
        let Some(mut connection) = self.connections.remove(gateway) else {
            let _ = self.admission.transport_failed().await;
            return Err("unknown gateway connection".into());
        };
        let epoch = connection.epoch();
        let joined = tokio::task::spawn_blocking(move || {
            let result = connection.receive();
            (connection, result)
        })
        .await;
        let (connection, result) = match joined {
            Ok(value) => value,
            Err(error) => {
                let _ = self.admission.transport_failed().await;
                return Err(format!("receive worker: {error}"));
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // An audited network disconnect requires a fresh owner, not permanent
                // cancellation of the admission actor. Audit failure or actual
                // future cancellation still leaves the guard incomplete.
                let audited = self.admission.transport_failed().await.is_ok();
                guard.complete = audited
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::NotConnected
                    );
                if guard.complete {
                    self.audited_receive_failure = Some(gateway.to_owned());
                }
                return Err(format!("receive: {error}"));
            }
        };
        let received_at_ms = self.admission.monotonic_ms();
        if matches!(result, Receive::Disconnected { .. }) {
            self.admission
                .transport_failed()
                .await
                .map_err(|e| format!("disconnect audit: {e:?}"))?;
        } else {
            self.connections.insert(gateway.to_owned(), connection);
            self.failed = false;
        }
        guard.complete = true;
        let read=GatewayRead {gateway_id:gateway.to_owned(),transport_epoch:epoch,received_at_ms,result};
        self.query_replies.observe(&read);
        if matches!(read.result,Receive::Disconnected {..}) {self.query_replies.reset(gateway);}
        Ok(read)
    }
    /// Write one bounded admitted phase using a blocking I/O worker, then audit.
    /// Cancellation poisons this owner; a removed connection is never reused.
    pub async fn step(&mut self) -> Result<Option<WriteOutcome>, String> {
        self.step_with_frame_budget(hb_transport::MAX_BATCH_FRAMES)
            .await
    }
    /// Explicit scheduling budget, also useful for observing exact member
    /// cancellation boundaries. A logical TS6 triplet cannot be split; a budget
    /// too small for a complete operation fails closed before sending bytes.
    pub async fn step_with_frame_budget(
        &mut self,
        frame_budget: usize,
    ) -> Result<Option<WriteOutcome>, String> {
        if !(1..=hb_transport::MAX_BATCH_FRAMES).contains(&frame_budget) {
            return Err("invalid physical frame budget".into());
        }
        if self.failed {
            return Err("transport owner requires recovery".into());
        }
        self.audited_write_failure = None;
        let mut available=self.connections.keys().filter(|gateway|!self.query_replies.waiting(gateway,self.now_ms())).cloned().collect::<std::collections::BTreeSet<_>>();
        if available.is_empty() {
            self.finish_reply_turnaround().await;
            available=self.connections.keys().filter(|gateway|!self.query_replies.waiting(gateway,self.now_ms())).cloned().collect();
            if available.is_empty() {return Ok(None);}
        }
        let mut slot = self.wait_write_slot().await;
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        self.failed = true;
        let Some(ticket) = self
            .admission
            .dispatch_query_filtered(available,self.query_paused.clone())
            .await
            .map_err(|e| format!("dispatch: {e:?}"))?
        else {
            self.failed = false;
            guard.complete = true;
            return Ok(None);
        };
        let mut tickets = self.collect_tickets(ticket, frame_budget).await?;
        if tickets.iter().any(|t|t.command.command==homesense_core::Command::ModuleAllOff)
            && tickets.iter().all(|t|t.away_authorization.as_ref().is_some_and(|a|a.guarded_operation())
                && t.feedback_barrier.is_none()
                && matches!(t.command.command,homesense_core::Command::ModuleAllOff|homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff)) {
            // The current-key cue stays in the same write, ahead of the Off
            // request. Nothing trails the module request when its passive
            // replies start. Audit byte ranges follow this exact ticket order.
            tickets.sort_by_key(|t|t.command.command==homesense_core::Command::ModuleAllOff);
        }
        let id = tickets[0].command.id;
        let device_id = tickets[0].command.device_id.clone();
        let gateway = tickets[0].gateway_id.clone();
        let Some(mut connection) = self.connections.remove(&gateway) else {
            let _ = self.admission.transport_failed().await;
            return Err("no connection for configured gateway".into());
        };
        let clock = self.admission.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let result = send_tickets(&tickets, &mut connection, || clock.monotonic_ms());
            *slot = Some(tokio::time::Instant::now() + post_write_interval(&tickets, &result.result, clock.write_interval));
            (connection, tickets, result, slot)
        });
        #[cfg(test)]
        if let Some(notification)=self.worker_spawned.take(){let _=notification.send(());}
        let (connection, mut tickets, result, _slot) = worker.await
        .map_err(|e| format!("transport batch worker: {e}"))?;
        let SendAttempt {result,projection}=result;
        let projection_cancelled=projection.is_some();
        let completed=if let Some(proof)=projection {
            self.admission.finish_projection_discard(proof).await.map_err(|e|format!("projection cancellation audit: {e:?}"))?;
            vec![]
        } else {self.audit_batch_progress(&tickets,&result).await?};
        if result.is_ok() {self.track_query_reply(&mut tickets);}
        if result.is_ok() || projection_cancelled {
            self.connections.insert(gateway, connection);
            self.failed = false;
        } else {
            self.admission
                .transport_failed()
                .await
                .map_err(|e| format!("batch failure audit: {e:?}"))?;
            // Only a completed durable fault audit permits supervisor recovery.
            // Expired, invalid-authority and invariant failures stay fatal.
            if result.as_ref().is_err_and(|error|
                network_send_failure(error)
                    && !matches!(error.reason, hb_transport::SendFailure::Expired)
            ) {
                self.audited_write_failure = Some(gateway);
            }
        }
        guard.complete = true;
        Ok(Some(WriteOutcome {
            command_id: id,
            device_id,
            managed_away_outputs: managed_away_outputs(&tickets,result.is_ok()),
            result,
            projection_cancelled,
            completed: completed.clone(),
            physical_frames: hb_transport::encoded_batch_len(&tickets.iter().map(|t| &t.frame).collect::<Vec<_>>())
                .ok_or("invalid completed transport batch")? / hb_protocol::FRAME_LEN,
            refresh_schedule: tickets.iter()
                .filter(|t| t.command.command!=homesense_core::Command::Query && completed.iter().any(|(id,_)|*id==t.command.id))
                .flat_map(|t|t.refresh_devices.iter().cloned().map(|id|(id,t.ramp_confirmation))).collect(),
            refresh_devices: tickets
                .iter()
                .filter(|t| completed.iter().any(|(id, _)| *id == t.command.id))
                .flat_map(|t| t.refresh_devices.clone())
                .collect(),
        }))
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[tokio::test]
    async fn queries_and_controls_share_the_same_write_spacing() {
        for interval_ms in [20, 50] {
            use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent};
            let path = std::env::temp_dir().join(format!("pacing-{}.sqlite", uuid::Uuid::new_v4()));
            let bytes = include_bytes!("../../../config/simulation.json");
            homesense_storage::Store::open(&path)
                .unwrap()
                .publish_configuration(None, bytes, ExecutionContext::Simulation, &[])
                .unwrap();
            let controller = uuid::Uuid::new_v4();
            let admission = Admission::open(
                path.clone(),
                crate::admission::Options {
                    context: ExecutionContext::Simulation,
                    write_interval: std::time::Duration::from_millis(interval_ms),
                    ..Default::default()
                },
                move |id| id == controller,
            )
            .await
            .unwrap();
            admission.synchronized(1, 1).await.unwrap();
            let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            struct Stream(std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>);
            impl Read for Stream {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    Err(std::io::ErrorKind::WouldBlock.into())
                }
            }
            impl Write for Stream {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().push(std::time::Instant::now());
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut owner = TransportOwner::new(
                admission.clone(),
                [(
                    "simulation".into(),
                    Connection::for_simulation(Stream(writes.clone()), 1),
                )]
                .into(),
            );
            let mut other = TransportOwner::new(
                admission.clone(),
                [(
                    "simulation".into(),
                    Connection::for_simulation(Stream(writes.clone()), 1),
                )]
                .into(),
            );
            let config =
                homesense_config::Configuration::parse(bytes, ExecutionContext::Simulation)
                    .unwrap();
            let id = DeviceId::new("simulation:light").unwrap();
            let (device, module) = config.binding(&id).unwrap();
            owner
                .query_status(
                    "simulation",
                    module,
                    device.channel,
                    ExecutionContext::Simulation,
                    100,
                )
                .await
                .unwrap();
            admission
                .submit(homesense_hap::requests::Batch {
                    controller,
                    intents: vec![homesense_hap::requests::Intent {
                        device: id.clone(),
                        command: Command::SetLevel {
                            level: Percent::new(30).unwrap(),
                            transition: LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                })
                .await
                .unwrap();
            // No reply is provided by this pacing fixture. The first owner's
            // read must expire before the next writer uses the physical bus.
            tokio::time::sleep(std::time::Duration::from_millis(110)).await;
            owner.finish_status_read("simulation");
            other.step().await.unwrap().unwrap().result.unwrap();
            owner
                .query_status(
                    "simulation",
                    module,
                    device.channel,
                    ExecutionContext::Simulation,
                    100,
                )
                .await
                .unwrap();
            {
                let times = writes.lock().unwrap();
                assert_eq!(times.len(), 3);
                assert!(times.windows(2).all(|pair| pair[1].duration_since(pair[0])
                    >= std::time::Duration::from_millis(interval_ms)));
            }
            admission.shutdown().await.unwrap();
            drop(owner);
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
            let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
        }
    }
    #[tokio::test]
    async fn cancelled_query_keeps_slot_until_blocking_writer_finishes() {
        use homesense_core::{DeviceId, ExecutionContext};
        let path =
            std::env::temp_dir().join(format!("pacing-cancel-{}.sqlite", uuid::Uuid::new_v4()));
        let bytes = include_bytes!("../../../config/simulation.json");
        homesense_storage::Store::open(&path)
            .unwrap()
            .publish_configuration(None, bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        let controller = uuid::Uuid::new_v4();
        let admission = Admission::open(
            path.clone(),
            crate::admission::Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        struct Blocked {
            started: Option<tokio::sync::oneshot::Sender<()>>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Read for Blocked {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }
        impl Write for Blocked {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(started) = self.started.take() {
                    let _ = started.send(());
                }
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .map_err(std::io::Error::other)?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut owner = TransportOwner::new(
            admission.clone(),
            [(
                "simulation".into(),
                Connection::for_simulation(
                    Blocked {
                        started: Some(started),
                        release: std::sync::Mutex::new(blocked),
                    },
                    1,
                ),
            )]
            .into(),
        );
        let config =
            homesense_config::Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
        let task = tokio::spawn(async move {
            let id = DeviceId::new("simulation:light").unwrap();
            let (device, module) = config.binding(&id).unwrap();
            owner
                .query_status(
                    "simulation",
                    module,
                    device.channel,
                    ExecutionContext::Simulation,
                    5000,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            admission.write_slot.try_lock().is_err(),
            "writer must retain slot after caller cancellation"
        );
        release.send(()).unwrap();
        let slot = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            admission.write_slot.clone().lock_owned(),
        )
        .await
        .unwrap();
        assert!(
            slot.is_some(),
            "completed writer must record the next allowed write time"
        );
        drop(slot);
        assert!(
            admission.task_generation().await.is_err(),
            "cancelled I/O still closes admission"
        );
        admission.shutdown().await.unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
    }
}

#[cfg(test)]
#[path = "transport_receive_error_tests.rs"]
mod receive_error_guard;
