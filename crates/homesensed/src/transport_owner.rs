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
    /// One shared write slot for queries and expanded control frames. Waiting
    /// precedes dispatch so STOP priority and deadlines are checked afterwards.
    async fn wait_write_slot(&self) -> tokio::sync::OwnedMutexGuard<Option<tokio::time::Instant>> {
        let slot = self.admission.write_slot.clone().lock_owned().await;
        if let Some(deadline) = *slot {
            tokio::time::sleep_until(deadline).await;
        }
        slot
    }
    pub(crate) fn belongs_to(&self, admission: &Admission) -> bool {
        self.admission.boot_id() == admission.boot_id()
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
    pub(crate) fn write_interval_ms(&self)->u64 {
        self.admission.write_interval.as_millis() as u64
    }
    pub(crate) async fn idle_for_refresh(&self,controller:uuid::Uuid,generation:crate::admission::TaskGeneration)->bool {
        self.admission.diagnostics(controller,generation).await
            .is_ok_and(|state|state.ready() && state.queued==0 && !state.in_flight)
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
        let mut slot = self.wait_write_slot().await;
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
        let mut slot = self.wait_write_slot().await;
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
            *slot = Some(tokio::time::Instant::now() + clock.write_interval);
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

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[tokio::test]
    async fn queries_and_controls_share_the_same_write_spacing() {
        for interval_ms in [20, 50] {
        use homesense_core::{ExecutionContext,DeviceId,Command,Percent,LevelTransition};
        let path=std::env::temp_dir().join(format!("pacing-{}.sqlite",uuid::Uuid::new_v4()));
        let bytes=include_bytes!("../../../config/simulation.json");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(None,bytes,ExecutionContext::Simulation,&[]).unwrap();
        let controller=uuid::Uuid::new_v4();
        let admission=Admission::open(path.clone(),crate::admission::Options{context:ExecutionContext::Simulation,write_interval:std::time::Duration::from_millis(interval_ms),..Default::default()},move |id|id==controller).await.unwrap();
        admission.synchronized(1,1).await.unwrap();
        let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Stream(std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>);
        impl Read for Stream {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
        impl Write for Stream {
            fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().push(std::time::Instant::now());Ok(bytes.len())}
            fn flush(&mut self)->std::io::Result<()>{Ok(())}
        }
        let mut owner=TransportOwner::new(admission.clone(),[("simulation".into(),Connection::for_simulation(Stream(writes.clone()),1))].into());
        let mut other=TransportOwner::new(admission.clone(),[("simulation".into(),Connection::for_simulation(Stream(writes.clone()),1))].into());
        let config=homesense_config::Configuration::parse(bytes,ExecutionContext::Simulation).unwrap();
        let id=DeviceId::new("simulation:light").unwrap();
        let (device,module)=config.binding(&id).unwrap();
        owner.query_status("simulation",module,device.channel,ExecutionContext::Simulation,100).await.unwrap();
        admission.submit(homesense_hap::requests::Batch{controller,intents:vec![homesense_hap::requests::Intent{device:id.clone(),command:Command::SetLevel{level:Percent::new(30).unwrap(),transition:LevelTransition::Immediate}}],subscriptions:vec![]}).await.unwrap();
        other.step().await.unwrap().unwrap().result.unwrap();
        owner.query_status("simulation",module,device.channel,ExecutionContext::Simulation,100).await.unwrap();
        {
            let times=writes.lock().unwrap();
            assert_eq!(times.len(),3);
            assert!(times.windows(2).all(|pair|pair[1].duration_since(pair[0])>=std::time::Duration::from_millis(interval_ms)));
        }
        admission.shutdown().await.unwrap();
        drop(owner);
        for suffix in ["","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",path.display()));}
        let _=std::fs::remove_file(path.with_extension("runtime.lock"));
        }
    }
    #[tokio::test]
    async fn cancelled_query_keeps_slot_until_blocking_writer_finishes() {
        use homesense_core::{ExecutionContext, DeviceId};
        let path=std::env::temp_dir().join(format!("pacing-cancel-{}.sqlite",uuid::Uuid::new_v4()));
        let bytes=include_bytes!("../../../config/simulation.json");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(None,bytes,ExecutionContext::Simulation,&[]).unwrap();
        let controller=uuid::Uuid::new_v4();
        let admission=Admission::open(path.clone(),crate::admission::Options{context:ExecutionContext::Simulation,..Default::default()},move |id|id==controller).await.unwrap();
        admission.synchronized(1,1).await.unwrap();
        let (started,ready)=tokio::sync::oneshot::channel();
        let (release,blocked)=std::sync::mpsc::channel();
        struct Blocked {
            started:Option<tokio::sync::oneshot::Sender<()>>,
            release:std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Read for Blocked {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
        impl Write for Blocked {
            fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
                if let Some(started)=self.started.take(){let _=started.send(());}
                self.release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).map_err(std::io::Error::other)?;
                Ok(bytes.len())
            }
            fn flush(&mut self)->std::io::Result<()>{Ok(())}
        }
        let mut owner=TransportOwner::new(admission.clone(),[("simulation".into(),Connection::for_simulation(Blocked{started:Some(started),release:std::sync::Mutex::new(blocked)},1))].into());
        let config=homesense_config::Configuration::parse(bytes,ExecutionContext::Simulation).unwrap();
        let task=tokio::spawn(async move {
            let id=DeviceId::new("simulation:light").unwrap();
            let (device,module)=config.binding(&id).unwrap();
            owner.query_status("simulation",module,device.channel,ExecutionContext::Simulation,5000).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2),ready).await.unwrap().unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(admission.write_slot.try_lock().is_err(),"writer must retain slot after caller cancellation");
        release.send(()).unwrap();
        let slot=tokio::time::timeout(std::time::Duration::from_secs(2),admission.write_slot.clone().lock_owned()).await.unwrap();
        assert!(slot.is_some(),"completed writer must record the next allowed write time");
        drop(slot);
        assert!(admission.task_generation().await.is_err(),"cancelled I/O still closes admission");
        admission.shutdown().await.unwrap();
        for suffix in ["","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",path.display()));}
        let _=std::fs::remove_file(path.with_extension("runtime.lock"));
    }

}
