//! One owner routes durable tickets to already-established bounded streams.
//! Construction never opens a socket or marks a gateway synchronized.
use crate::admission::Admission;
use hb_transport::{Connection, Receive, SendError};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

pub struct TransportOwner<S> {
    admission: Admission,
    connections: BTreeMap<String, Connection<S>>,
    failed: bool,
}
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
}
impl<S: Read + Write + Send + 'static> TransportOwner<S> {
    pub fn new(admission: Admission, connections: BTreeMap<String, Connection<S>>) -> Self {
        Self {
            admission,
            connections,
            failed: false,
        }
    }
    pub(crate) fn gateway_ids(&self) -> Vec<String> {
        self.connections.keys().cloned().collect()
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
        if self.failed || timeout_ms == 0 || timeout_ms > 5000 {
            return Err("invalid query owner or timeout".into());
        }
        let epoch = self
            .connections
            .get(gateway)
            .ok_or("missing query gateway")?
            .epoch();
        let frame = hb_transport::PreparedFrame::prepare(
            module,
            channel,
            homesense_core::Command::Query,
            context,
            epoch,
        )
        .map_err(|e| e.to_string())?;
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
            (connection, sent)
        })
        .await
        .map_err(|e| format!("query worker: {e}"))?;
        if let Err(error) = sent {
            let _ = self.admission.transport_failed().await;
            guard.complete = true;
            return Err(format!("query send: {error:?}"));
        }
        self.connections.insert(gateway.to_owned(), connection);
        self.failed = false;
        guard.complete = true;
        Ok(())
    }
    /// One bounded read. Unknown frames remain raw until configuration decoding.
    pub async fn receive(&mut self, gateway: &str) -> Result<GatewayRead, String> {
        if self.failed {
            return Err("transport owner requires recovery".into());
        }
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
                let _ = self.admission.transport_failed().await;
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
        Ok(GatewayRead {
            gateway_id: gateway.to_owned(),
            transport_epoch: epoch,
            received_at_ms,
            result,
        })
    }
    /// Write at most one ticket using a blocking I/O worker, then await audit.
    /// Cancellation poisons this owner; a removed connection is never reused.
    pub async fn step(&mut self) -> Result<Option<WriteOutcome>, String> {
        if self.failed {
            return Err("transport owner requires recovery".into());
        }
        let mut guard = OperationGuard {
            admission: self.admission.clone(),
            complete: false,
        };
        self.failed = true;
        let Some(ticket) = self
            .admission
            .dispatch()
            .await
            .map_err(|e| format!("dispatch: {e:?}"))?
        else {
            self.failed = false;
            guard.complete = true;
            return Ok(None);
        };
        let id = ticket.command.id;
        let device_id = ticket.command.device_id.clone();
        let gateway = ticket.gateway_id;
        let Some(mut connection) = self.connections.remove(&gateway) else {
            let _ = self.admission.finish_write(id, false).await;
            return Err("no connection for configured gateway".into());
        };
        let clock = self.admission.clone();
        let (connection, result) = tokio::task::spawn_blocking(move || {
            let result = connection.send(&ticket.frame, ticket.command.deadline_ms, || {
                clock.monotonic_ms()
            });
            (connection, result)
        })
        .await
        .map_err(|e| format!("transport worker: {e}"))?;
        self.admission
            .finish_write(id, result.is_ok())
            .await
            .map_err(|e| format!("write audit: {e:?}"))?;
        if result.is_ok() {
            self.connections.insert(gateway, connection);
            self.failed = false;
        }
        guard.complete = true;
        Ok(Some(WriteOutcome {
            command_id: id,
            device_id,
            result,
        }))
    }
}
