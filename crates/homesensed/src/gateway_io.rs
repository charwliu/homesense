//! Owned bounded work for independently connected buses. One Admission and
//! one shared physical writer remain authoritative; readers never reserve it.
use super::*;
use homesense_core::gateway_scope::WireScope;
use homesense_storage::gateway_audit::FaultAudit;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) type Connector<S> = Arc<dyn Fn(&str, u64) -> io::Result<Connection<S>> + Send + Sync>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Ready,
    Synchronizing,
    Offline,
    Connecting,
}
type ReadResult<S> = (
    Connection<S>,
    io::Result<Receive>,
    u64,
    Option<Result<crate::gateway_runtime::FaultReport, hap::server::WriteStatus>>,
);
struct ReadWork<S> {
    job: tokio::task::JoinHandle<ReadResult<S>>,
    guard: OperationGuard,
    wire: WireScope,
    retired: Arc<AtomicBool>,
    written_before_receive: Vec<u64>,
}
struct ConnectWork<S> {
    job: tokio::task::JoinHandle<io::Result<Connection<S>>>,
    guard: OperationGuard,
    wire: WireScope,
}
pub(super) struct Local<S> {
    pub(super) epochs: BTreeMap<String, u64>,
    phases: BTreeMap<String, Phase>,
    reads: BTreeMap<String, ReadWork<S>>,
    connects: BTreeMap<String, ConnectWork<S>>,
    retired_reads: Vec<ReadWork<S>>,
    retired_connects: Vec<ConnectWork<S>>,
    due: BTreeMap<String, u64>,
    attempts: BTreeMap<String, u32>,
    connector: Connector<S>,
    faults: Vec<(WireScope, FaultAudit)>,
    written: BTreeMap<String, std::collections::VecDeque<u64>>,
}
pub(crate) struct IoEvents {
    pub reads: Vec<GatewayRead>,
    pub read_fences: BTreeMap<String, Vec<u64>>,
    pub faults: Vec<(WireScope, FaultAudit)>,
    pub connected: Vec<WireScope>,
}
pub(crate) fn network_read_failure(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
    )
}
pub(crate) fn network_send_failure(error: &SendError) -> bool {
    match error.reason {
        hb_transport::SendFailure::Disconnected | hb_transport::SendFailure::Expired => true,
        hb_transport::SendFailure::Io(kind) => {
            network_read_failure(kind)
                || matches!(
                    kind,
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::WriteZero
                )
        }
        _ => false,
    }
}
fn connect_unavailable(kind: io::ErrorKind) -> bool {
    network_read_failure(kind)
        || matches!(
            kind,
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::TimedOut
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::AddrNotAvailable
        )
}
impl<S: Read + Write + Send + 'static> TransportOwner<S> {
    pub(crate) fn local_mode(&self) -> bool {
        self.local.is_some()
    }
    pub(crate) fn connection_wires(&self) -> Result<Vec<WireScope>, String> {
        self.connections
            .iter()
            .map(|(id, connection)| {
                WireScope::from_persisted(id.clone(), connection.epoch()).map_err(|e| e.to_string())
            })
            .collect()
    }
    pub(crate) fn enable_local_io(&mut self, connector: Connector<S>) -> Result<(), String> {
        if self.failed || self.local.is_some() || self.connections.len() < 2 {
            return Err("invalid local IO activation".into());
        }
        let epochs = self
            .connections
            .iter()
            .map(|(id, c)| (id.clone(), c.epoch()))
            .collect::<BTreeMap<_, _>>();
        self.local = Some(Local {
            phases: epochs.keys().map(|id| (id.clone(), Phase::Ready)).collect(),
            epochs,
            reads: BTreeMap::new(),
            connects: BTreeMap::new(),
            retired_reads: vec![],
            retired_connects: vec![],
            due: BTreeMap::new(),
            attempts: BTreeMap::new(),
            connector,
            faults: vec![],
            written: BTreeMap::new(),
        });
        Ok(())
    }
    pub(crate) fn local_available_writers(&self, blocked: &BTreeSet<String>) -> BTreeSet<String> {
        self.connections
            .keys()
            .filter(|id| {
                !blocked.contains(*id)
                    && self
                        .local
                        .as_ref()
                        .is_none_or(|local| local.phases.get(*id) == Some(&Phase::Ready))
            })
            .cloned()
            .collect()
    }
    pub(crate) fn local_connected(&self, gateway: &str) -> bool {
        self.local.as_ref().is_some_and(|local| {
            matches!(
                local.phases.get(gateway),
                Some(Phase::Ready | Phase::Synchronizing)
            )
        })
    }
    pub(crate) fn local_syncing(&self, gateway: &str) -> bool {
        self.local
            .as_ref()
            .is_some_and(|local| local.phases.get(gateway) == Some(&Phase::Synchronizing))
    }
    pub(crate) fn local_idle_stream(&self, gateway: &str) -> bool {
        self.connections.contains_key(gateway)
    }
    pub(crate) fn local_wire(&self, gateway: &str) -> Result<WireScope, String> {
        let epoch = self
            .local
            .as_ref()
            .and_then(|local| local.epochs.get(gateway))
            .ok_or("unknown local gateway")?;
        WireScope::from_persisted(gateway.into(), *epoch).map_err(|e| e.to_string())
    }
    pub(crate) fn local_ready(&mut self, wire: &WireScope) -> Result<(), String> {
        let local = self.local.as_mut().ok_or("local IO not enabled")?;
        if local.epochs.get(wire.gateway()) != Some(&wire.epoch())
            || local.phases.get(wire.gateway()) != Some(&Phase::Synchronizing)
        {
            return Err("stale local synchronization result".into());
        }
        local.phases.insert(wire.gateway().into(), Phase::Ready);
        local.attempts.remove(wire.gateway());
        Ok(())
    }
    pub(crate) fn local_record_fault(
        &mut self,
        wire: WireScope,
        audit: FaultAudit,
    ) -> Result<(), String> {
        self.local_offline(&wire)?;
        self.local
            .as_mut()
            .ok_or("local IO missing")?
            .faults
            .push((wire, audit));
        Ok(())
    }
    pub(crate) fn has_local_fault(&self) -> bool {
        self.local
            .as_ref()
            .is_some_and(|local| !local.faults.is_empty())
    }
    fn local_offline(&mut self, wire: &WireScope) -> Result<(), String> {
        let now = self.now_ms();
        let local = self.local.as_mut().ok_or("local IO not enabled")?;
        if local.epochs.get(wire.gateway()) != Some(&wire.epoch()) {
            return Err("stale local fault".into());
        }
        // A successful actor fault audit positively retires this old ownership.
        // Its late completion may be dropped, but cannot replace a new stream.
        if let Some(mut work) = local.reads.remove(wire.gateway()) {
            work.retired.store(true, Ordering::Release);
            work.guard.complete = true;
            local.retired_reads.push(work);
        }
        if let Some(mut work) = local.connects.remove(wire.gateway()) {
            work.guard.complete = true;
            local.retired_connects.push(work);
        }
        if local.retired_reads.len() + local.retired_connects.len()
            > homesense_core::gateway_scope::MAX_GATEWAYS * 2
        {
            return Err("bounded retired IO capacity exceeded".into());
        }
        local.written.remove(wire.gateway());
        local.phases.insert(wire.gateway().into(), Phase::Offline);
        let attempts = local.attempts.entry(wire.gateway().into()).or_default();
        *attempts = attempts.saturating_add(1);
        let delay = 250u64
            .saturating_mul(1u64 << (*attempts - 1).min(4))
            .min(4000);
        local.due.insert(
            wire.gateway().into(),
            now.checked_add(delay).ok_or("local retry time overflow")?,
        );
        self.connections.remove(wire.gateway());
        Ok(())
    }
    pub(crate) fn start_local_reads(&mut self) -> Result<(), String> {
        if self.failed {
            return Err("global transport failure".into());
        }
        let ids = self.connections.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            if !self.local_connected(&id) {
                continue;
            }
            let wire = self.local_wire(&id)?;
            let mut connection = self
                .connections
                .remove(&id)
                .ok_or("owned reader stream missing")?;
            let admission = self.admission.clone();
            let scope = wire.clone();
            let retired = Arc::new(AtomicBool::new(false));
            let worker_retired = retired.clone();
            let job = tokio::task::spawn_blocking(move || {
                let result = connection.receive();
                let at = admission.monotonic_ms();
                let failed = matches!(&result, Ok(Receive::Disconnected { .. }))
                    || result
                        .as_ref()
                        .err()
                        .is_some_and(|e| network_read_failure(e.kind()));
                // Fence and audit before this worker publishes a disconnect.
                // If the mailbox/audit fails, stop synchronously; no A writer
                // can acquire this loaned stream in the meantime.
                let audit = (failed && !worker_retired.load(Ordering::Acquire))
                    .then(|| admission.gateway_fault_blocking(scope));
                if audit.as_ref().is_some_and(Result::is_err)
                    || result
                        .as_ref()
                        .err()
                        .is_some_and(|e| !network_read_failure(e.kind()))
                {
                    admission.stop_now();
                }
                (connection, result, at, audit)
            });
            let guard = OperationGuard {
                admission: self.admission.clone(),
                complete: false,
            };
            let local = self.local.as_mut().ok_or("local IO missing")?;
            let written_before_receive = local
                .written
                .get(&id)
                .map_or_else(Vec::new, |ids| ids.iter().copied().collect());
            if local
                .reads
                .insert(
                    id,
                    ReadWork {
                        job,
                        guard,
                        wire,
                        retired,
                        written_before_receive,
                    },
                )
                .is_some()
            {
                return Err("duplicate local reader ownership".into());
            }
        }
        Ok(())
    }
    /// Poll completed owned jobs only. A pending A read/connect never waits in
    /// the central runtime and cannot discard B's independent connection.
    pub(crate) async fn poll_local_io(&mut self, start_connects: bool) -> Result<IoEvents, String> {
        let faults = std::mem::take(&mut self.local.as_mut().ok_or("local IO missing")?.faults);
        let mut events = IoEvents {
            reads: vec![],
            read_fences: BTreeMap::new(),
            faults,
            connected: vec![],
        };
        let readers = self
            .local
            .as_ref()
            .ok_or("local IO not enabled")?
            .reads
            .iter()
            .filter(|(_, work)| work.job.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in readers {
            let mut work = self
                .local
                .as_mut()
                .ok_or("local IO missing")?
                .reads
                .remove(&id)
                .ok_or("local read job missing")?;
            let (connection, result, at, audit) = (&mut work.job)
                .await
                .map_err(|e| format!("local read worker: {e}"))?;
            let current = self
                .local
                .as_ref()
                .ok_or("local IO missing")?
                .epochs
                .get(&id)
                .copied();
            let retired = work.retired.load(Ordering::Acquire);
            if retired {
                work.guard.complete = true;
                continue;
            }
            if current != Some(work.wire.epoch()) {
                return Err("unretired reader returned foreign generation".into());
            }
            if let Some(audit) = audit {
                let report = audit.map_err(|e| format!("local fault audit: {e:?}"))?;
                work.guard.complete = true;
                if report.retired
                    && self.local.as_ref().is_some_and(|local| {
                        local.phases.get(&id) != Some(&Phase::Ready)
                            && local.phases.get(&id) != Some(&Phase::Synchronizing)
                    })
                {
                    continue;
                }
                self.local_offline(&work.wire)?;
                events.faults.push((work.wire, report.audit));
                continue;
            }
            if !self.local_connected(&id) {
                return Err("unretired read no longer owns its slot".into());
            }
            let result = result.map_err(|e| format!("non-network local receive: {e}"))?;
            if connection.epoch() != work.wire.epoch() {
                return Err("read worker returned wrong wire epoch".into());
            }
            self.connections.insert(id.clone(), connection);
            work.guard.complete = true;
            events
                .read_fences
                .insert(id.clone(), work.written_before_receive);
            events.reads.push(GatewayRead {
                gateway_id: id,
                transport_epoch: work.wire.epoch(),
                received_at_ms: at,
                result,
            });
        }
        let connected = self
            .local
            .as_ref()
            .ok_or("local IO missing")?
            .connects
            .iter()
            .filter(|(_, work)| work.job.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in connected {
            let mut work = self
                .local
                .as_mut()
                .ok_or("local IO missing")?
                .connects
                .remove(&id)
                .ok_or("local connect job missing")?;
            let result = (&mut work.job)
                .await
                .map_err(|e| format!("local connect worker: {e}"))?;
            let local = self.local.as_ref().ok_or("local IO missing")?;
            if local.epochs.get(&id) != Some(&work.wire.epoch())
                || local.phases.get(&id) != Some(&Phase::Connecting)
            {
                return Err("unretired connector returned foreign generation".into());
            }
            match result {
                Ok(connection) => {
                    if connection.epoch() != work.wire.epoch() {
                        return Err("connector returned wrong local epoch".into());
                    }
                    self.connections.insert(id.clone(), connection);
                    self.local
                        .as_mut()
                        .ok_or("local IO missing")?
                        .phases
                        .insert(id, Phase::Synchronizing);
                    work.guard.complete = true;
                    events.connected.push(work.wire);
                }
                Err(error) if connect_unavailable(error.kind()) => {
                    let audit = self
                        .admission
                        .gateway_fault(work.wire.clone())
                        .await
                        .map_err(|e| format!("connect fault audit: {e:?}"))?;
                    work.guard.complete = true;
                    self.local_offline(&work.wire)?;
                    events.faults.push((work.wire, audit));
                }
                Err(error) => return Err(format!("non-network connect error: {error}")),
            }
        }
        // Preserve retired jobs until their bounded workers finish; no reuse,
        // task abort or detached reader can publish old bytes into a new epoch.
        let mut retired_reads =
            std::mem::take(&mut self.local.as_mut().ok_or("local IO missing")?.retired_reads);
        for mut work in retired_reads.drain(..) {
            if work.job.is_finished() {
                let (_, result, _, audit) = (&mut work.job)
                    .await
                    .map_err(|e| format!("retired read worker invariant: {e}"))?;
                if let Err(error) = result
                    && !network_read_failure(error.kind())
                {
                    return Err(format!("retired non-network read: {error}"));
                }
                if audit.is_some_and(|audit| audit.is_err()) {
                    return Err("retired fault audit failed".into());
                }
            } else {
                self.local
                    .as_mut()
                    .ok_or("local IO missing")?
                    .retired_reads
                    .push(work);
            }
        }
        let mut retired_connects = std::mem::take(
            &mut self
                .local
                .as_mut()
                .ok_or("local IO missing")?
                .retired_connects,
        );
        for mut work in retired_connects.drain(..) {
            if work.job.is_finished() {
                let result = (&mut work.job)
                    .await
                    .map_err(|e| format!("retired connect worker invariant: {e}"))?;
                if let Err(error) = result
                    && !connect_unavailable(error.kind())
                {
                    return Err(format!("retired non-network connect: {error}"));
                }
            } else {
                self.local
                    .as_mut()
                    .ok_or("local IO missing")?
                    .retired_connects
                    .push(work);
            }
        }
        if start_connects {
            let now = self.now_ms();
            let due = self
                .local
                .as_ref()
                .ok_or("local IO missing")?
                .due
                .iter()
                .filter(|(id, at)| {
                    **at <= now
                        && self
                            .local
                            .as_ref()
                            .is_some_and(|local| local.phases.get(*id) == Some(&Phase::Offline))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in due {
                let wire = self
                    .admission
                    .gateway_attempt(id.clone())
                    .await
                    .map_err(|e| format!("local retry admission: {e:?}"))?;
                let local = self.local.as_mut().ok_or("local IO missing")?;
                if local.epochs.get(&id).is_none_or(|old| *old >= wire.epoch()) {
                    return Err("reused local retry epoch".into());
                }
                local.epochs.insert(id.clone(), wire.epoch());
                local.phases.insert(id.clone(), Phase::Connecting);
                local.due.remove(&id);
                let connector = local.connector.clone();
                let scope = wire.clone();
                let job =
                    tokio::task::spawn_blocking(move || connector(scope.gateway(), scope.epoch()));
                local.connects.insert(
                    id,
                    ConnectWork {
                        job,
                        guard: OperationGuard {
                            admission: self.admission.clone(),
                            complete: false,
                        },
                        wire,
                    },
                );
            }
        }
        events.reads.sort_by_key(|read| read.received_at_ms);
        Ok(events)
    }
    pub(crate) fn local_jobs_pending(&self) -> bool {
        self.local.as_ref().is_some_and(|local| {
            !local.reads.is_empty()
                || !local.connects.is_empty()
                || !local.retired_reads.is_empty()
                || !local.retired_connects.is_empty()
        })
    }
    pub(crate) async fn local_step(
        &mut self,
        blocked: &BTreeSet<String>,
    ) -> Result<Option<WriteOutcome>, String> {
        let available = self.local_available_writers(blocked);
        if available.is_empty() {
            return Ok(None);
        }
        let mut slot = self.wait_write_slot().await;
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        let Some(ticket) = self
            .admission
            .dispatch_available(available)
            .await
            .map_err(|e| format!("local dispatch: {e:?}"))?
        else {
            guard.complete = true;
            return Ok(None);
        };
        let tickets = self.collect_tickets(ticket,hb_transport::MAX_BATCH_FRAMES).await?;
        let wire = self.local_wire(&tickets[0].gateway_id)?;
        let id = tickets[0].command.id;
        let device_id = tickets[0].command.device_id.clone();
        let mut connection = self
            .connections
            .remove(&tickets[0].gateway_id)
            .ok_or("available local stream disappeared")?;
        let clock = self.admission.clone();
        let (connection, tickets, result, _slot) = tokio::task::spawn_blocking(move || {
            let result = send_tickets(&tickets, &mut connection, || clock.monotonic_ms());
            *slot = Some(tokio::time::Instant::now() + clock.write_interval);
            (connection, tickets, result, slot)
        })
        .await
        .map_err(|e| format!("local batch writer: {e}"))?;
        let SendAttempt {result,projection}=result;
        let projection_cancelled=projection.is_some();
        let completed=if let Some(proof)=projection {
            self.admission.finish_projection_discard(proof).await.map_err(|e|format!("local projection cancellation audit: {e:?}"))?;
            vec![]
        } else {self.audit_batch_progress(&tickets,&result).await?};
        match &result {
            Err(_) if projection_cancelled => {self.connections.insert(wire.gateway().into(),connection);},
            Ok(()) => {
                self.connections.insert(wire.gateway().into(), connection);
                let written = self
                    .local
                    .as_mut()
                    .ok_or("local IO missing")?
                    .written
                    .entry(wire.gateway().into())
                    .or_default();
                for (id, _) in &completed {
                    written.push_back(*id);
                }
                while written.len() > 256 {
                    written.pop_front();
                }
            }
            Err(error) if network_send_failure(error) => {
                let mut end = 0;
                let partial = tickets.iter().find_map(|ticket| {
                    let start = end;
                    end += ticket.frame.encoded_len();
                    (error.bytes_written > start && error.bytes_written < end)
                        .then_some(ticket.command.id)
                });
                let audit = match partial {
                    Some(id) => self.admission.gateway_write_fault(id).await,
                    None => self.admission.gateway_fault(wire.clone()).await,
                }
                .map_err(|e| format!("local batch fault: {e:?}"))?;
                self.local_record_fault(wire.clone(), audit)?;
            }
            Err(error) => {
                let _ = self.admission.transport_failed().await;
                return Err(format!("non-network batch failure: {error:?}"));
            }
        }
        guard.complete = true;
        Ok(Some(WriteOutcome {
            device_id,
            command_id: id,
            result,
            projection_cancelled,
            completed: completed.clone(),
            physical_frames: tickets.iter().map(|t| t.frame.physical_frames()).sum(),
            refresh_devices: tickets
                .iter()
                .filter(|t| completed.iter().any(|(id, _)| *id == t.command.id))
                .flat_map(|t| t.refresh_devices.clone())
                .collect(),
        }))
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use crate::gateway_runtime::{Initial, Sample};
    use homesense_core::{
        Command, DeviceId, ExecutionContext, LevelTransition, Percent,
        gateway_scope::{Port, ReadRequirement},
    };
    struct Stream {
        wait: Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
        outcome: u8,
        writes: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }
    impl Read for Stream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let Some((started, release)) = self.wait.take() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            let _ = started.send(());
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(io::Error::other)?;
            match self.outcome {
                0 => {
                    bytes[0] = 0;
                    Ok(1)
                }
                1 => Ok(0),
                _ => Err(io::ErrorKind::ConnectionReset.into()),
            }
        }
    }
    impl Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn proof() -> Vec<Sample> {
        (1..=8)
            .map(|channel| Sample {
                requirement: ReadRequirement {
                    port: Port {
                        address: 0x15,
                        channel,
                    },
                    before_count: 0,
                    sent_at_ms: 0,
                    deadline_ms: 100,
                },
                count: 1,
                at: 0,
            })
            .collect()
    }
    async fn bounded_poll(owner: &mut TransportOwner<Stream>) -> IoEvents {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let event = owner.poll_local_io(false).await.unwrap();
                if !event.reads.is_empty() || !owner.local_jobs_pending() {
                    return event;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn run(outcome: u8) {
        let path =
            std::env::temp_dir().join(format!("local-retirement-{}.sqlite", uuid::Uuid::new_v4()));
        let mut json: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        json["gateways"][0]["id"] = serde_json::json!("a");
        let mut second = json["gateways"][0].clone();
        second["id"] = serde_json::json!("b");
        second["endpoint"] = serde_json::json!("127.0.0.1:50008");
        json["gateways"].as_array_mut().unwrap().push(second);
        json["devices"][0]["gateway"] = serde_json::json!("a");
        json["devices"][0]["id"] = serde_json::json!("a-light");
        let mut second = json["devices"][0].clone();
        second["id"] = serde_json::json!("b-light");
        second["gateway"] = serde_json::json!("b");
        json["devices"].as_array_mut().unwrap().push(second);
        let bytes = serde_json::to_vec(&json).unwrap();
        homesense_storage::Store::open(&path)
            .unwrap()
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
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
        let initial = |gateway: &str| Initial {
            wire: WireScope::from_persisted(gateway.into(), 1).unwrap(),
            samples: proof(),
        };
        admission
            .activate_gateways(vec![initial("a"), initial("b")])
            .await
            .unwrap();
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let a_writes = Arc::new(std::sync::Mutex::new(vec![]));
        let b_writes = Arc::new(std::sync::Mutex::new(vec![]));
        let mut owner = TransportOwner::new(
            admission.clone(),
            [
                (
                    "a".into(),
                    Connection::for_simulation(
                        Stream {
                            wait: Some((started, blocked)),
                            outcome,
                            writes: a_writes.clone(),
                        },
                        1,
                    ),
                ),
                (
                    "b".into(),
                    Connection::for_simulation(
                        Stream {
                            wait: None,
                            outcome: 0,
                            writes: b_writes.clone(),
                        },
                        1,
                    ),
                ),
            ]
            .into(),
        );
        owner
            .enable_local_io(Arc::new(
                |_, _| Err(io::ErrorKind::ConnectionRefused.into()),
            ))
            .unwrap();
        owner.start_local_reads().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        let old = owner.local_wire("a").unwrap();
        let audit = admission.gateway_fault(old.clone()).await.unwrap();
        owner.local_record_fault(old.clone(), audit).unwrap();
        // B completes while the retired A worker is still blocked.
        let events = bounded_poll(&mut owner).await;
        assert!(events.reads.iter().any(|read| read.gateway_id == "b"));
        let submit = |level| homesense_hap::requests::Batch {
            controller,
            intents: vec![homesense_hap::requests::Intent {
                device: DeviceId::new("b-light").unwrap(),
                command: Command::SetLevel {
                    level: Percent::new(level).unwrap(),
                    transition: LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        };
        admission.submit(submit(30)).await.unwrap();
        owner
            .local_step(&BTreeSet::new())
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(b_writes.lock().unwrap().len(), 1);
        let new = admission.gateway_attempt("a".into()).await.unwrap();
        assert!(new.epoch() > old.epoch());
        assert!(admission.gateway_ready(new.clone(), proof()).await.unwrap());
        owner.connections.insert(
            "a".into(),
            Connection::for_simulation(
                Stream {
                    wait: None,
                    outcome: 0,
                    writes: a_writes.clone(),
                },
                new.epoch(),
            ),
        );
        let local = owner.local.as_mut().unwrap();
        local.epochs.insert("a".into(), new.epoch());
        local.phases.insert("a".into(), Phase::Synchronizing);
        owner.local_ready(&new).unwrap();
        // A's actual late bytes, EOF or RST must not replace the new connection
        // or poison the healthy B writer after its exact old scope was audited.
        release.send(()).unwrap();
        let events = bounded_poll(&mut owner).await;
        assert!(events.reads.is_empty());
        assert!(
            events
                .faults
                .iter()
                .all(|(wire, _)| wire.epoch() == old.epoch())
        );
        assert_eq!(owner.connections["a"].epoch(), new.epoch());
        assert_eq!(owner.connections["b"].epoch(), 1);
        admission.submit(submit(40)).await.unwrap();
        owner
            .local_step(&BTreeSet::new())
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(b_writes.lock().unwrap().len(), 2);
        assert!(a_writes.lock().unwrap().is_empty());
        let generation = admission.task_generation().await.unwrap();
        assert!(
            !admission
                .diagnostics(controller, generation)
                .await
                .unwrap()
                .failed
        );
        admission.shutdown().await.unwrap();
        drop(owner);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
    }
    #[tokio::test]
    async fn retired_a_bytes_cannot_replace_recovered_a_or_stop_b() {
        run(0).await;
    }
    #[tokio::test]
    async fn retired_a_eof_cannot_replace_recovered_a_or_stop_b() {
        run(1).await;
    }
    #[tokio::test]
    async fn retired_a_rst_cannot_replace_recovered_a_or_stop_b() {
        run(2).await;
    }
}
