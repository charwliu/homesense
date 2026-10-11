//! Bounded in-process publication capability. Network adapters must authorize
//! administrators before exposing it. Receiving a request does not publish it.
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

const MAX_DOCUMENT: usize = 4 * 1024 * 1024;
const REQUEST_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Publisher {
    sender: mpsc::Sender<Pending>,
}
pub struct Receiver {
    receiver: mpsc::Receiver<Pending>,
}
pub struct Pending {
    devices: Vec<u8>,
    rules: Vec<u8>,
    expected_revision: u64,
    deadline: Instant,
    reply: oneshot::Sender<Result<u64, String>>,
}
/// Capacity two bounds queued document bytes to 16 MiB. No background task,
/// database mutation or network operation is started by creating this channel.
pub fn channel() -> (Publisher, Receiver) {
    let (sender, receiver) = mpsc::channel(2);
    (Publisher { sender }, Receiver { receiver })
}
impl Publisher {
    /// No automatic retries: timeout after acceptance can have an unknown outcome.
    pub async fn publish(
        &self,
        devices: Vec<u8>,
        rules: Vec<u8>,
        expected_revision: u64,
    ) -> Result<u64, String> {
        if expected_revision == 0
            || devices.is_empty()
            || rules.is_empty()
            || devices.len() > MAX_DOCUMENT
            || rules.len() > MAX_DOCUMENT
        {
            return Err("invalid publication size or expected revision".into());
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Pending {
                devices,
                rules,
                expected_revision,
                deadline: Instant::now() + REQUEST_LIFETIME,
                reply,
            })
            .map_err(|_| "publication busy or closed".to_owned())?;
        tokio::time::timeout(REQUEST_LIFETIME, receive)
            .await
            .map_err(|_| {
                "publication response timed out; inspect active revision before retrying".to_owned()
            })?
            .map_err(|_| {
                "publication response lost; inspect active revision before retrying".to_owned()
            })?
    }
}
impl Pending {
    pub fn devices(&self) -> &[u8] {
        &self.devices
    }
    pub fn rules(&self) -> &[u8] {
        &self.rules
    }
    pub fn expected_revision(&self) -> u64 {
        self.expected_revision
    }
    /// Check immediately before entering the publication barrier. Cancellation
    /// after that boundary must not interrupt STOP draining or leave a half-switch.
    pub fn abandoned(&self) -> bool {
        self.reply.is_closed() || Instant::now() >= self.deadline
    }
    pub fn complete(self, result: Result<u64, String>) {
        let _ = self.reply.send(result);
    }
}
impl Receiver {
    /// Skip abandoned queued requests; the coordinator performs semantic validation
    /// and compares expected_revision atomically at the publication barrier.
    pub async fn receive(&mut self) -> Option<Pending> {
        while let Some(request) = self.receiver.recv().await {
            if request.abandoned() {
                request.complete(Err("publication request expired or abandoned".into()));
                continue;
            }
            return Some(request);
        }
        None
    }
    pub fn close(&mut self) {
        self.receiver.close();
        while let Ok(request) = self.receiver.try_recv() {
            request.complete(Err("publication service closed before acceptance".into()));
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.close();
    }
}
