//! In-process capability for a published automation runtime. Network adapters
//! must separately enforce their user/room permissions before using this handle.
use crate::{
    admission::{Admission, TaskGeneration},
    scene_scheduler::SceneScheduler,
};
use homesense_config::rule_document::Document;
use homesense_core::scene::Status;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
#[derive(Debug)]
pub enum Action {
    Start(String),
    Cancel(u64),
    Status(u64),
    Retire(u64),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Started(u64),
    Cancelled(usize),
    Status(Option<Status>),
    Retired(Status),
}
struct Request {
    action: Action,
    deadline: Instant,
    reply: oneshot::Sender<Result<Reply, String>>,
}
#[derive(Clone)]
pub struct Control {
    sender: mpsc::Sender<Request>,
}
impl Control {
    /// Bounded admission, no retry. A lost response after execution is ambiguous;
    /// callers must inspect history rather than resubmit an action automatically.
    pub async fn request(&self, action: Action) -> Result<Reply, String> {
        if matches!(&action, Action::Start(id) if id.is_empty() || id.len()>256) {
            return Err("invalid scene name".into());
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Request {
                action,
                deadline: Instant::now() + Duration::from_secs(5),
                reply,
            })
            .map_err(|_| "automation control busy or closed".to_owned())?;
        tokio::time::timeout(Duration::from_secs(5), receive)
            .await
            .map_err(|_| {
                "automation control response timed out; outcome may be unknown".to_owned()
            })?
            .map_err(|_| "automation control closed; outcome may be unknown".to_owned())?
    }
}
pub(crate) struct Receiver {
    receiver: mpsc::Receiver<Request>,
    controller: Uuid,
    generation: TaskGeneration,
}
pub(crate) fn channel(controller: Uuid, generation: TaskGeneration) -> (Control, Receiver) {
    let (sender, receiver) = mpsc::channel(32);
    (
        Control { sender },
        Receiver {
            receiver,
            controller,
            generation,
        },
    )
}
impl Receiver {
    pub(crate) async fn poll(
        &mut self,
        admission: &Admission,
        document: &Document,
        scenes: &mut SceneScheduler,
    ) {
        // Bound per-cycle work so incoming control cannot starve bus processing.
        for _ in 0..8 {
            let Ok(request) = self.receiver.try_recv() else {
                break;
            };
            if request.reply.is_closed() {
                continue;
            }
            let result = async {
                if Instant::now() >= request.deadline {
                    return Err("automation request expired".into());
                }
                admission
                    .variable_snapshot(self.controller, self.generation.clone(), vec![])
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                // Recheck after authorization yields; no deferred stale request.
                if request.reply.is_closed() || Instant::now() >= request.deadline {
                    return Err("automation request expired or abandoned".into());
                }
                match request.action {
                    Action::Start(id) => scenes
                        .start_document(document, &id, admission.monotonic_ms())
                        .await
                        .map(Reply::Started),
                    Action::Cancel(run) => scenes.cancel(run).await.map(Reply::Cancelled),
                    Action::Status(run) => Ok(Reply::Status(scenes.status(run))),
                    Action::Retire(run) => scenes.retire(run).map(Reply::Retired),
                }
            }
            .await;
            let _ = request.reply.send(result);
        }
    }
    pub(crate) fn close(&mut self) {
        self.receiver.close();
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request.reply.send(Err("automation runtime stopped".into()));
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.close();
    }
}
