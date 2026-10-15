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
    source:Option<crate::publication_auth::Source>,
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
        self.enqueue(devices,rules,expected_revision,None).await
    }
    pub(crate) async fn publish_authorized(&self,devices:Vec<u8>,rules:Vec<u8>,expected_revision:u64,source:crate::publication_auth::Source)->Result<u64,String> {
        if !source.matches(&devices,Some(&rules),expected_revision){return Err("publication source revoked or request changed".into());}
        self.enqueue(devices,rules,expected_revision,Some(source)).await
    }
    async fn enqueue(&self,devices:Vec<u8>,rules:Vec<u8>,expected_revision:u64,source:Option<crate::publication_auth::Source>)->Result<u64,String> {
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
                source,
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
        self.reply.is_closed() || Instant::now() >= self.deadline || self.source.as_ref().is_some_and(|source|!source.valid())
    }
    /// The actor rechecks the exact source while serializing entry into the
    /// publication barrier. After success callers must finish the handoff,
    /// regardless of source changes, and must not call abandoned() to abort it.
    pub(crate) async fn begin(&self,admission:&crate::admission::Admission,timeout:Duration)->Result<(),hap::server::WriteStatus> {
        admission.begin_authorized_automation_publication(self.devices.clone(),self.rules.clone(),self.expected_revision,timeout,self.source.clone()).await
    }
    pub(crate) async fn begin_before_deadline(&self,admission:&crate::admission::Admission,deadline:tokio::time::Instant)->Result<(),crate::automation_tcp::HandoffPublicationFailure> {
        crate::automation_tcp::begin_authorized_handoff_before_deadline(admission,&self.devices,&self.rules,self.expected_revision,deadline,self.source.clone()).await
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

#[cfg(test)]
#[path = "publication_auth_tests.rs"]
mod source_auth_tests;
