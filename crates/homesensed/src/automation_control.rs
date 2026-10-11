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
    LastRun(String),
    Activity(u64),
    SceneActivity(String),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Started(u64),
    Cancelled(usize),
    Status(Option<Status>),
    Retired(Status),
    LastRun(Option<LastRun>),
    Activity(Option<crate::scene_activity::SceneActivity>),
}
/// Persisted execution receipt. Timestamps are monotonic within `boot`, and
/// CommandsWritten is not proof that physical targets were reached.
#[derive(Debug, PartialEq, Eq)]
pub struct LastRun {
    pub boot: String,
    pub run: String,
    pub revision: u64,
    pub started_ms: u64,
    pub updated_ms: u64,
    pub version: u64,
    pub status: homesense_storage::scene_run::Status,
    pub cause: Option<homesense_storage::scene_run::Cause>,
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
        if matches!(&action, Action::Start(id) | Action::LastRun(id) | Action::SceneActivity(id) if id.is_empty() || id.len()>256 || id.chars().any(char::is_control)) {
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
        feedback: &crate::feedback::Feedback,
        max_age_ms: u64,
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
                    Action::Activity(run) => scenes.activity(run,feedback,max_age_ms).await.map(Reply::Activity),
                    Action::SceneActivity(id) => {
                        let scene=document.timed_scenes.get(&id).ok_or("unknown timed scene")?;
                        if !scene.enabled || scene.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {return Err("timed scene disabled or unreviewed".into());}
                        crate::scene_activity::Targets::new(&scene.steps).project(admission,self.controller,&self.generation,feedback,max_age_ms).await.map(|a|Reply::Activity(Some(a)))
                    },
                    Action::Retire(run) => scenes.retire(run).map(Reply::Retired),
                    Action::LastRun(id) => {
                        if !document.timed_scenes.contains_key(&id) { return Err("unknown timed scene".into()); }
                        let page=admission.scene_history(self.controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:id}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::LastRun(page.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})))
                    },
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
