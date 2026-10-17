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
    AwayOff{controller:Uuid,revision:u64,lease:crate::away_auth::Authorization,submit:bool},
    ModuleQuery{controller:Uuid,revision:u64,module_id:String,scope:crate::module_queries::Scope,submit:bool},
    ModuleQueryStatus{controller:Uuid,revision:u64,module_id:String,scope:crate::module_queries::Scope,commands:Vec<u64>},
    MotorBinding {controller:Uuid,revision:u64,device:homesense_core::DeviceId},
    ModuleBinding {controller:Uuid,revision:u64,module_id:String,channel:u8},
    ModuleCommand {controller:Uuid,revision:u64,module_id:String,channel:u8,allowed_devices:std::collections::BTreeSet<String>,action:crate::management_devices::DeviceAction},
    Modules {controller:Uuid,revision:u64,offset:usize,limit:usize},
    RunDetails {controller:Uuid,revision:u64,query:homesense_storage::scene_run::Query},
    SceneHistory {controller:Uuid,revision:u64,scene:String,before:Option<homesense_storage::scene_history::Cursor>,limit:usize},
    RunSummary {controller:Uuid,revision:u64,boot:String,run:String},
    Diagnostics {controller:Uuid,revision:u64},
    ConfigurationHistory {controller:Uuid,revision:u64,before:Option<u64>,limit:usize},
    ConfigurationExport {controller:Uuid,revision:u64,selected:u64},
    DeviceEvents {controller:Uuid,revision:u64,ids:Vec<homesense_core::DeviceId>,cursor:Option<crate::management_events::Cursor>,scope:String},
    ConfigurationPreview {controller:Uuid,revision:u64,candidate:homesense_config::Configuration},
    CalendarStatus,
    ManagedCalendarStatus { controller: Uuid, revision: u64 },
    Devices {controller:Uuid,revision:u64,ids:Vec<homesense_core::DeviceId>},
    KeypadSetting {controller:Uuid,revision:u64,keypad:String,lease:crate::web_auth::ModuleOffLease,setting:crate::management_keypads::Setting,submit:bool},
    PanelDeviceCommand{controller:Uuid,revision:u64,device:homesense_core::DeviceId,action:crate::management_devices::DeviceAction,lease:crate::web_auth::ModuleOffLease},
    DeviceCommand {controller:Uuid,revision:u64,device:homesense_core::DeviceId,action:crate::management_devices::DeviceAction},
    SceneCancel { controller: Uuid, revision: u64, run: u64 },
    SceneSwitchStates {controller:Uuid,revision:u64},
    SceneSelectionDetails {controller:Uuid,revision:u64},
    SceneWrites {controller:Uuid,revision:u64,scenes:Vec<(String,Option<bool>)>},
    SceneCatalog {controller:Uuid,revision:u64,scenes:Vec<String>},
    SceneDefinition {controller:Uuid,revision:u64,scene:String},
    SceneAvailability {controller:Uuid,revision:u64,scene:String},
    SceneRelative {controller:Uuid,revision:u64,scene:String,request_id:String},
    SceneRelativeStatus {controller:Uuid,revision:u64,request_id:String},
    SceneActions { controller: Uuid, revision: u64, scenes: Vec<(String,bool)> },
    SceneAction { controller: Uuid, revision: u64, scene: String, execute: bool },
    Start(String),
    Cancel(u64),
    Status(u64),
    Retire(u64),
    LastRun(String),
    Activity(u64),
    SceneActivity(String),
    SceneTracking {controller:Uuid,revision:u64,scene:String},
    SceneActivityWrites {controller:Uuid,revision:u64,scenes:Vec<(String,Option<bool>)>},
}
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    ModuleQuery(crate::module_queries::Accepted),
    ModuleQueryStatus(crate::module_queries::Status),
    ModuleBinding(homesense_core::DeviceId),
    Modules(crate::management_modules::Page),
    RunDetails(Option<(String,serde_json::Value)>),
    SceneHistory(homesense_storage::scene_history::Page),
    RunSummary(Option<(String,LastRun)>),
    Diagnostics(crate::admission::Diagnostics),
    ConfigurationHistory(homesense_storage::configuration_history::Page),
    ConfigurationExport(Option<homesense_storage::configuration_history::Export>),
    DeviceEvents(crate::management_events::Update),
    ConfigurationPreview(crate::configuration_preview::Preview),
    Devices(crate::management_devices::DeviceSnapshot),
    CommandsAccepted(Vec<u64>),
    SceneAccepted(Option<u64>),
    ImmediateAccepted(crate::immediate_scene::Receipt),
    SceneCatalog(std::collections::BTreeMap<String,SceneCatalogEntry>),
    SceneDefinition(homesense_config::rule_document::SceneExecutionKind),
    SceneAvailability(crate::gateway_runtime::ActionGate),
    ScenesAccepted(Vec<u64>),
    SceneSwitchStates(std::collections::BTreeMap<String,Option<bool>>),
    SceneSelectionDetails(std::collections::BTreeMap<String,crate::scene_switches::Details>),
    SceneRelative(crate::native_scene::Receipt),
    CalendarStatus { boot: String, revision: u64, status: Option<crate::event_rules::CalendarStatus> },
    Started(u64),
    Cancelled(usize),
    Status(Option<Status>),
    Retired(Status),
    LastRun(Option<LastRun>),
    Activity(Option<crate::scene_activity::SceneActivity>),
    SceneTracking(Option<crate::scene_tracking::Projection>),
}
/// One read-only control quantum for a directory, avoiding a bus-drive round
/// trip between every definition, availability, activity and history field.
#[derive(Debug, PartialEq, Eq)]
pub struct SceneCatalogEntry {
    pub room: Option<homesense_config::Room>,
    pub execution_kind: homesense_config::rule_document::SceneExecutionKind,
    pub action_gate: crate::gateway_runtime::ActionGate,
    pub tracking: Option<crate::scene_tracking::Projection>,
    pub activity: crate::scene_activity::SceneActivity,
    pub last_run: Option<LastRun>,
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
/// Coalesced current state, not a lossless event log. Execution history remains
/// available through LastRun and the persistent journal.
#[derive(Debug, PartialEq, Eq)]
pub struct SceneObservation {
    pub boot: String,
    pub revision: u64,
    pub activity: crate::scene_activity::SceneActivity,
    pub tracking_valid_until_ms: Option<u64>,
    pub last_run: Option<LastRun>,
}
pub type SceneSubscription = tokio::sync::watch::Receiver<Option<Result<SceneObservation,String>>>;
struct Watcher {
    scene: String,
    sender: tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>,
    next: Instant,
}
struct Request {
    action: Action,
    source:Option<crate::source_auth::Source>,
    watch: Option<tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>>,
    deadline: Instant,
    reply: oneshot::Sender<Result<Reply, String>>,
}
#[derive(Clone)]
pub struct Control {
    sender: mpsc::Sender<Request>,
    instance: Uuid,
    unavailable:Option<std::sync::Arc<Unavailable>>,
    selection_reader: SelectionReaderSlot,
}
struct Unavailable {admission:Admission,generation:TaskGeneration}

type SelectionReaderSlot = std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<SelectionReader>>>>;
struct SelectionReader {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    document: std::sync::Arc<Document>,
    stop: tokio::sync::watch::Receiver<bool>,
}
async fn selection_reply(admission:&Admission,generation:&TaskGeneration,document:&Document,controller:Uuid,revision:u64)->Result<Reply,String> {
    if revision!=document.revision {return Err("scene state revision expired".into());}
    let snapshot=admission.scene_selection_snapshot(controller,generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
    if !admission.diagnostics(controller,generation.clone()).await.map_err(|e|format!("{e:?}"))?.mapping_readable() {return Err("scene selection runtime not ready".into());}
    crate::scene_switches::details(document,&snapshot).map(Reply::SceneSelectionDetails)
}
impl SelectionReader {
    fn active(&self)->bool { !*self.stop.borrow() && self.stop.has_changed().is_ok() }
    async fn read(&self,controller:Uuid,revision:u64)->Result<Reply,String> {
        if !self.active(){return Err("automation runtime stopped".into());}
        // One coherent owner turn preserves both controller authorizations,
        // exact generation, expiry/projection and mapping checks. No values are
        // retained here and no physical/control work can use this capability.
        let snapshot=self.admission.published_scene_selection_snapshot(self.controller,controller,self.generation.clone(),revision,crate::scene_switches::variable_names(&self.document)).await.map_err(|e|format!("{e:?}"))?;
        if !self.admission.source_valid(){return Err("source authority expired after runtime read".into());}
        if !snapshot.mapping_readable {return Err("scene selection runtime not ready".into());}
        crate::scene_switches::details(&self.document,&snapshot.values).map(Reply::SceneSelectionDetails)
    }
}

impl Control {
    /// Non-secret identity of this request channel, retained by every view/clone.
    /// A rebuilt scheduler may reuse numeric run handles within the same boot.
    pub(crate) fn instance_id(&self) -> Uuid { self.instance }
    pub(crate) fn unavailable(&self,admission:Admission,generation:TaskGeneration)->Self {
        Self{sender:self.sender.clone(),instance:self.instance,unavailable:Some(std::sync::Arc::new(Unavailable{admission,generation})),selection_reader:self.selection_reader.clone()}
    }
    /// Bounded admission, no retry. A lost response after execution is ambiguous;
    /// callers must inspect history rather than resubmit an action automatically.
    pub async fn subscribe(&self, scene: String) -> Result<SceneSubscription,String> {
        let (sender,receiver)=tokio::sync::watch::channel(None);
        self.request_inner(Action::SceneActivity(scene),Some(sender)).await?;
        Ok(receiver)
    }
    pub async fn request(&self, action: Action) -> Result<Reply, String> {
        self.request_inner(action,None).await
    }
    async fn request_inner(&self,action:Action,watch:Option<tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>>)->Result<Reply,String> {
        if let Some(unavailable)=&self.unavailable {
            return match action {
                Action::Diagnostics{controller,revision} if revision==unavailable.generation.epochs.configuration=>{
                    let mut state=unavailable.admission.diagnostics(controller,unavailable.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                    // This view is not replaced until the new control runtime is
                    // installed, even if physical synchronization just completed.
                    state.gateway_ready=false;
                    Ok(Reply::Diagnostics(state))
                },
                _=>Err("gateway unavailable; control runtime is gated".into()),
            };
        }
        if watch.is_none() && let Action::SceneSelectionDetails {controller,revision}=&action {
            let reader=self.selection_reader.lock().map_err(|_|"scene read capability poisoned")?.clone();
            if let Some(reader)=reader {
                if self.sender.is_closed(){return Err("automation runtime stopped".into());}
                let result=tokio::time::timeout(Duration::from_secs(5),reader.read(*controller,*revision)).await
                    .map_err(|_|"automation control response timed out; outcome may be unknown".to_owned())?;
                // Closing/replacing a published owner cannot lend this capability
                // to a late read, even if the old actor has not shut down yet.
                if self.sender.is_closed() || !reader.active() || !self.selection_reader.lock().map_err(|_|"scene read capability poisoned")?.as_ref().is_some_and(|current|std::sync::Arc::ptr_eq(current,&reader)) {
                    return Err("automation runtime stopped".into());
                }
                return result;
            }
        }
        // Keep the channel wait state behind the bounded request boundary.
        // Callers may already retain a sizeable HTTP / joined-driver future.
        Box::pin(self.request_scoped(action,watch,None)).await
    }
    pub(crate) async fn request_authorized(&self,action:Action,source:crate::source_auth::Source)->Result<Reply,String> {
        if !source.matches(&action) {return Err("source authority expired or operation mismatch".into());}
        Box::pin(self.request_scoped(action,None,Some(source))).await
    }
    async fn request_scoped(&self, action: Action, watch: Option<tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>>,source:Option<crate::source_auth::Source>) -> Result<Reply,String> {
        if self.unavailable.is_some(){return Err("gateway unavailable; control runtime is gated".into());}
        if matches!(&action, Action::Start(id) | Action::LastRun(id) | Action::SceneActivity(id) if id.is_empty() || id.len()>256 || id.chars().any(char::is_control)) {
            return Err("invalid scene name".into());
        }
        if matches!(&action, Action::SceneAction { scene, .. } if scene.trim().is_empty() || scene.len()>250 || scene.chars().any(char::is_control)) {
            return Err("invalid scene name".into());
        }
        if let Action::SceneActions{scenes,..}=&action
            && (scenes.is_empty() || scenes.len()>64 || scenes.iter().any(|(id,_)|id.trim().is_empty() || id.len()>250 || id.chars().any(char::is_control))
                || scenes.iter().map(|(id,_)|id).collect::<std::collections::BTreeSet<_>>().len()!=scenes.len()) {
                return Err("invalid scene batch".into());
        }
        if let Action::SceneWrites{scenes,..}=&action
            && (scenes.is_empty()||scenes.len()>64||scenes.iter().any(|(id,_)|id.trim().is_empty()||id.len()>250||id.chars().any(char::is_control))
                ||scenes.iter().map(|(id,_)|id).collect::<std::collections::BTreeSet<_>>().len()!=scenes.len()) {return Err("invalid scene write batch".into());}
        if let Action::SceneRelative{scene,request_id,..}=&action
            && (scene.trim().is_empty() || scene.len()>250 || scene.chars().any(char::is_control) || request_id.is_empty() || request_id.len()>128 || !request_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')) {return Err("invalid relative request".into());}
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Request {
                action,
                source,
                watch,
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
// Timing labels contain no scene/device identities, request data or credentials.
fn managed_request_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::SceneSelectionDetails {..} => Some("scene_selection"),
        Action::SceneCatalog {..} => Some("scene_catalog"),
        Action::DeviceEvents {..} => Some("device_events"),
        Action::Devices {..} => Some("devices"),
        Action::SceneAction {..} | Action::SceneActions {..} | Action::SceneWrites {..} => Some("scene_write"),
        Action::DeviceCommand {..} | Action::PanelDeviceCommand {..} | Action::ModuleCommand {..} => Some("device_write"),
        Action::ModuleQuery {..} | Action::ModuleQueryStatus {..} | Action::Modules {..} => Some("module_read"),
        _ => None,
    }
}
pub(crate) struct Receiver {
    receiver: mpsc::Receiver<Request>,
    controller: Uuid,
    generation: TaskGeneration,
    watchers: std::collections::VecDeque<Watcher>,
    relative: crate::native_scene::Manager,
    budget:homesense_core::rule::Budget,
    selection_reader: SelectionReaderSlot,
}
pub(crate) fn channel(controller: Uuid, generation: TaskGeneration) -> (Control, Receiver) {
    let (sender, receiver) = mpsc::channel(32);
    let selection_reader=SelectionReaderSlot::default();
    (
        Control { sender,instance:Uuid::new_v4(),unavailable:None,selection_reader:selection_reader.clone() },
        Receiver {
            receiver,
            controller,
            generation,
            watchers: Default::default(),
            relative: Default::default(),
            budget:Default::default(),
            selection_reader,
        },
    )
}
impl Receiver {
    /// Production read-only capability. It uses the existing Admission owner,
    /// never the physical work queue; manual poll-only channels keep old behavior.
    pub(crate) async fn bind_selection_reader(&mut self,admission:&Admission,document:&Document,stop:tokio::sync::watch::Receiver<bool>)->Result<(),String> {
        if *stop.borrow() || stop.has_changed().is_err() || self.receiver.is_closed() || document.revision!=self.generation.epochs.configuration
            || admission.task_generation().await.map_err(|e|format!("{e:?}"))?!=self.generation {
            return Err("scene read capability generation invalid".into());
        }
        *self.selection_reader.lock().map_err(|_|"scene read capability poisoned")?=Some(std::sync::Arc::new(SelectionReader {
            admission:admission.clone(),controller:self.controller,generation:self.generation.clone(),document:std::sync::Arc::new(document.clone()),stop,
        }));
        Ok(())
    }

    pub(crate) fn controller(&self)->Uuid {self.controller}
    pub(crate) fn set_budget(&mut self,budget:homesense_core::rule::Budget){self.budget=budget;}

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn poll(
        &mut self,
        admission: &Admission,
        document: &Document,
        scenes: &mut SceneScheduler,
        feedback: &crate::feedback::Feedback,
        max_age_ms: u64,
        calendar: Option<&crate::event_rules::ScheduledRules>,
    ) {
        self.poll_causal(admission,document,scenes,feedback,max_age_ms,calendar,None).await;
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn poll_causal(&mut self,admission:&Admission,document:&Document,scenes:&mut SceneScheduler,feedback:&crate::feedback::Feedback,max_age_ms:u64,calendar:Option<&crate::event_rules::ScheduledRules>,mut causal:Option<&mut crate::deferred_router::Router>) {
        self.relative.tick(admission,document,feedback).await;
        // Bound per-cycle work so incoming control cannot starve bus processing.
        for _ in 0..8 {
            let Ok(request) = self.receiver.try_recv() else {
                break;
            };
            let read_kind=managed_request_kind(&request.action);
            let handling_started=Instant::now();
            let queued_ms=Duration::from_secs(5).saturating_sub(request.deadline.saturating_duration_since(handling_started)).as_millis();
            if request.reply.is_closed() {
                if let Some(kind)=read_kind && queued_ms>=100 {
                    log::warn!("automation_request_lag kind={kind} phase=abandoned queue_ms={queued_ms} handler_ms=0");
                }
                continue;
            }
            self.watchers.retain(|watcher|!watcher.sender.is_closed());
            let watch_scene=if request.watch.is_some() {
                match &request.action {Action::SceneActivity(id)=>Some(id.clone()),_=>None}
            } else {None};
            // One bounded request at a time: do not embed every action's
            // derived scene/fade/session state in the outer polling future.
            let result = Box::pin(async {
                if request.source.as_ref().is_some_and(|source|!source.matches(&request.action)) {return Err("source authority expired before runtime handling".into());}
                if let Some(source)=&request.source {
                    let requested=match &request.action {
                        Action::SceneAction{scene,execute:true,..}=>vec![scene.clone()],
                        Action::SceneActions{scenes,..}=>scenes.iter().filter(|(_,on)|*on).map(|(id,_)|id.clone()).collect(),
                        Action::SceneWrites{scenes,..}|Action::SceneActivityWrites{scenes,..}=>scenes.iter().filter(|(_,on)|*on==Some(true)).map(|(id,_)|id.clone()).collect(),
                        _=>vec![],
                    };
                    if !requested.is_empty() {let bundle=admission.automation_bundle(self.controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        for scene in requested {if bundle.configuration.keypads().values().flat_map(|p|p.keys.values()).any(|k|matches!(&k.action,Some(homesense_config::keypad::KeyAction::SceneExecute{scene:id,..}) if *id==scene)) {
                            let ids=homesense_config::keypad::scene_execute_devices(document,&bundle.configuration,&scene).map_err(|e|e.to_string())?;
                            if ids.iter().any(|id|!source.permits_device(id.as_str())) {return Err("scene execute full-branch device scope denied".into());}
                        }}
                    }
                }
                let scoped_admission=admission.with_source(request.source.clone());
                let admission=&scoped_admission;
                let mut scene_scope=scenes.source_scope(request.source.clone());
                let scenes=&mut *scene_scope;
                let mut causal_scope=causal.as_deref_mut().map(|router|router.source_scope(request.source.clone()));
                let mut causal=causal_scope.as_deref_mut();
                if request.watch.is_some() && self.watchers.len()>=16 {return Err("scene subscription capacity reached".into());}
                if Instant::now() >= request.deadline {
                    return Err("automation request expired".into());
                }
                // A diagnostic read must remain possible after admission closes.
                if let Action::Diagnostics {controller,revision}=&request.action {
                    if *revision!=document.revision {return Err("diagnostic revision expired".into());}
                    return admission.diagnostics(*controller,self.generation.clone()).await.map(Reply::Diagnostics).map_err(|e|format!("{e:?}"));
                }
                admission
                    .variable_snapshot(self.controller, self.generation.clone(), vec![])
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                // Recheck after authorization yields; no deferred stale request.
                if request.reply.is_closed() || Instant::now() >= request.deadline {
                    return Err("automation request expired or abandoned".into());
                }
                if !admission.source_valid() {return Err("source authority expired after runtime read".into());}
                match request.action {
                    Action::ConfigurationHistory{controller,revision,before,limit}=>{
                        if revision!=document.revision {return Err("configuration history revision expired".into());}
                        admission.configuration_history(controller,self.generation.clone(),before,limit).await.map(Reply::ConfigurationHistory).map_err(|e|format!("{e:?}"))
                    },
                    Action::ConfigurationExport{controller,revision,selected}=>{
                        if revision!=document.revision {return Err("configuration export revision expired".into());}
                        admission.configuration_export(controller,self.generation.clone(),selected).await.map(Reply::ConfigurationExport).map_err(|e|format!("{e:?}"))
                    },
                    Action::DeviceEvents {controller,revision,ids,cursor,scope} => {
                        if revision!=document.revision{return Err("event configuration changed".into());}
                        crate::management_events::read_scoped(admission,controller,self.generation.clone(),ids,feedback,cursor,&scope).await.map(Reply::DeviceEvents)
                    },

                    Action::ConfigurationPreview {controller,revision,candidate} => {
                        if revision!=document.revision || candidate.revision()<=revision {return Err("configuration preview revision mismatch".into());}
                        let current=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::ConfigurationPreview(crate::configuration_preview::Preview {
                            boot:admission.boot_id().to_owned(),active_revision:revision,candidate_revision:candidate.revision(),
                            connection_rebuild_required:crate::configuration_preview::connection_rebuild_required(&current.configuration,&candidate),
                            interrupted_runs:scenes.publication_impact(),invalidates_old_rules_and_timers:true,
                            device_changes:crate::configuration_preview::device_changes(&current.configuration,&candidate),
                        }))
                    },

                    Action::ModuleQuery{controller,revision,module_id,scope,submit}=>{
                        if revision!=document.revision || request.reply.is_closed() || Instant::now()>=request.deadline || !crate::module_queries::matches_feedback(admission,&self.generation,feedback,revision) {return Err("module query expired or feedback generation changed".into());}
                        admission.module_query_until(controller,self.generation.clone(),module_id,scope,submit,request.deadline).await.map(Reply::ModuleQuery).map_err(|e|format!("module query rejected: {e:?}"))
                    },
                    Action::ModuleQueryStatus{controller,revision,module_id,scope,commands}=>{
                        if revision!=document.revision || !crate::module_queries::matches_feedback(admission,&self.generation,feedback,revision) {return Err("query status generation changed".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        let records=admission.module_query_records(controller,self.generation.clone(),commands.clone()).await.map_err(|e|format!("{e:?}"))?;
                        let diagnostics=admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        let captured_ms=admission.monotonic_ms();
                        let modules=crate::management_modules::project(&bundle.configuration,&feedback.snapshot(),Some(feedback.module_truth()),captured_ms,max_age_ms,diagnostics.ready()&&feedback.is_active(),0,64)?;
                        // Locate independently of pagination; the module registry can exceed 64.
                        let (location,_)=bundle.configuration.module(&module_id).ok_or("module missing")?;
                        let index=bundle.configuration.gateways().iter().flat_map(|(gateway,g)|g.modules().keys().map(move|address|(gateway,address))).position(|(gateway,address)|gateway==&location.gateway&&*address==location.address).ok_or("module missing")?;
                        let module=if index<64 {modules.into_iter().find(|m|m.module_id.as_deref()==Some(&module_id)).ok_or("module missing")?} else {crate::management_modules::project(&bundle.configuration,&feedback.snapshot(),Some(feedback.module_truth()),captured_ms,max_age_ms,diagnostics.ready()&&feedback.is_active(),index,1)?.pop().ok_or("module missing")?};
                        crate::module_queries::project_status(records,module,&scope,self.generation.epochs.transport,&commands,captured_ms).map(Reply::ModuleQueryStatus)
                    },
                    Action::AwayOff{controller,revision,lease,submit}=>{
                        if revision!=document.revision || controller!=lease.controller() || request.reply.is_closed() || Instant::now()>=request.deadline {return Err("Away operation expired".into());}
                        if submit {
                            admission.submit_away_off(revision,lease.clone(),false).await.map_err(|e|format!("{e:?}"))?;
                            // Preserve settled physical brightness before the module goes
                            // dark, including runtimes without HAP or indicator samplers.
                            if crate::module_queries::matches_feedback(admission,&self.generation,feedback,revision) {
                                admission.remember_levels(revision,feedback.snapshot(),max_age_ms).await.map_err(|e|format!("restore levels: {e:?}"))?;
                            }
                            if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("Away operation expired".into());}
                        }
                        let ids=admission.submit_away_off(revision,lease,submit).await.map_err(|e|format!("{e:?}"))?;
                        if submit && matches!(document.away_off.as_ref().map(|p|p.active_execution_policy),Some(homesense_config::rule_document::AwayExecutionPolicy::CancelAffectedLighting)) {
                            for device in document.away_off.as_ref().unwrap().modules.iter().flat_map(|m|&m.devices) {
                                scenes.manual_override(&homesense_core::DeviceId::new(device.clone()).map_err(|e|e.to_string())?).await?;
                            }
                        }
                        Ok(Reply::CommandsAccepted(ids))
                    },
                    Action::Modules {controller,revision,offset,limit} => {
                        if revision!=document.revision {return Err("module inventory revision expired".into());}
                        crate::management_modules::snapshot(admission,controller,self.generation.clone(),feedback,max_age_ms,offset,limit).await.map(Reply::Modules)
                    },
                    Action::Devices {controller,revision,ids} => {
                        if revision!=document.revision {return Err("device query revision expired".into());}
                        crate::management_devices::snapshot_with_max_age(admission,controller,self.generation.clone(),ids,feedback,max_age_ms).await.map(Reply::Devices)
                    },
                    Action::MotorBinding {controller,revision,device}=>{
                        if revision!=document.revision {return Err("motor binding revision expired".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        bundle.configuration.motor_command(&device,homesense_core::MotorAction::Stop).map_err(|e|e.to_string())?;
                        Ok(Reply::ModuleBinding(device))
                    },
                    Action::ModuleBinding{controller,revision,module_id,channel}=>{
                        if revision!=document.revision{return Err("module binding revision expired".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        crate::management_modules::control_device(&bundle.configuration,&module_id,channel).map(Reply::ModuleBinding)
                    },
                    Action::ModuleCommand{controller,revision,module_id,channel,allowed_devices,action} => {
                        if revision!=document.revision{return Err("module command revision expired".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        let device=crate::management_modules::control_device(&bundle.configuration,&module_id,channel)?;
                        if !allowed_devices.contains(device.as_str()){return Err("module channel not permitted".into());}
                        if request.reply.is_closed() || Instant::now()>=request.deadline{return Err("module command expired or abandoned".into());}
                        match action {
                            crate::management_devices::DeviceAction::Open|crate::management_devices::DeviceAction::Close|crate::management_devices::DeviceAction::Stop=>{
                                let action=match action{crate::management_devices::DeviceAction::Open=>homesense_core::MotorAction::Open,crate::management_devices::DeviceAction::Close=>homesense_core::MotorAction::Close,_=>homesense_core::MotorAction::Stop};
                                admission.submit_motor_for_revision(revision,controller,device,action).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"))
                            },
                            action=>{
                                let batch=crate::management_devices::prepare(admission,controller,self.generation.clone(),device,action,feedback,max_age_ms).await?;
                                if request.reply.is_closed() || Instant::now()>=request.deadline{return Err("module command expired or abandoned".into());}
                                admission.submit_for_revision(revision,batch).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"))
                            },
                        }
                    },
                    Action::KeypadSetting{controller,revision,keypad,lease,setting,submit} => {
                        if revision!=document.revision {return Err("keypad settings revision expired".into());}
                        let (batch,lease)=crate::management_keypads::prepare(admission,controller,self.generation.clone(),&keypad,lease,setting).await?;
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("keypad setting expired or abandoned".into());}
                        if !submit {return Ok(Reply::CommandsAccepted(vec![]));}
                        let intent=batch.intents.into_iter().next().ok_or("empty panel setting")?;
                        admission.submit_panel_command_for_revision(revision,controller,intent.device,intent.command,lease).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"))
                    },
                    Action::PanelDeviceCommand{controller,revision,device,action,lease}=>{
                        if revision!=document.revision || lease.controller()!=controller || !lease.valid() {return Err("panel credential or revision expired".into());}
                        if !matches!(action,crate::management_devices::DeviceAction::PanelLed{..}|crate::management_devices::DeviceAction::PanelLedOff) {return Err("typed LED action required".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                        let batch=crate::management_devices::prepare(admission,controller,self.generation.clone(),device,action,feedback,max_age_ms).await?;
                        let intent=batch.intents.into_iter().next().ok_or("empty panel command")?;
                        let lease=crate::panel_auth::Lease::led(lease,&bundle.configuration,intent.device.clone(),intent.command).ok_or("panel authorization expired")?;
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("panel command expired or abandoned".into());}
                        admission.submit_panel_command_for_revision(revision,controller,intent.device,intent.command,lease).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"))
                    },
                    Action::DeviceCommand {controller,revision,device,action} => {
                        if revision!=document.revision {return Err("device command revision expired".into());}
                        if let crate::management_devices::DeviceAction::KeypadPress{keypad,key}=action {
                            if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("keypad press expired or abandoned".into());}
                            let remaining=request.deadline.saturating_duration_since(Instant::now());
                            let limit=admission.monotonic_ms().saturating_add(u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
                            return admission.submit_keypad_press_for_revision(revision,controller,device,keypad,key,limit).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"));
                        }
                        if let crate::management_devices::DeviceAction::MotorPress {direction}=action {
                            if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("motor press expired or abandoned".into());}
                            let remaining=request.deadline.saturating_duration_since(Instant::now());
                            let limit=admission.monotonic_ms().saturating_add(u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
                            return admission.submit_motor_press_for_revision(revision,controller,device,direction,limit).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"));
                        }
                        let motor=match action {
                            crate::management_devices::DeviceAction::Open=>Some(homesense_core::MotorAction::Open),
                            crate::management_devices::DeviceAction::Close=>Some(homesense_core::MotorAction::Close),
                            crate::management_devices::DeviceAction::Stop=>Some(homesense_core::MotorAction::Stop),
                            _=>None,
                        };
                        if let Some(motor)=motor {
                            admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?;
                            if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("device command expired or abandoned".into());}
                            return admission.submit_motor_for_revision(revision,controller,device,motor).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"));
                        }
                        if matches!(action,crate::management_devices::DeviceAction::PanelLed{..}|crate::management_devices::DeviceAction::PanelLedOff) {return Err("native LED control requires a source credential lease".into());}
                        let batch=crate::management_devices::prepare(admission,controller,self.generation.clone(),device,action,feedback,max_age_ms).await?;
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("device command expired or abandoned".into());}
                        admission.submit_for_revision(revision,batch).await.map(Reply::CommandsAccepted).map_err(|e|format!("{e:?}"))
                    },
                    Action::Diagnostics {..} => unreachable!("handled before admission guard"),
                    Action::ManagedCalendarStatus { controller, revision } => {
                        if revision != document.revision { return Err("calendar query revision expired".into()); }
                        admission.variable_snapshot(controller,self.generation.clone(),vec![]).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::CalendarStatus { boot: admission.boot_id().to_owned(), revision: document.revision, status: calendar.map(|calendar|calendar.status()) })
                    },
                    Action::CalendarStatus => Ok(Reply::CalendarStatus {
                        boot: admission.boot_id().to_owned(),
                        revision: document.revision,
                        status: calendar.map(|calendar| calendar.status()),
                    }),
                    Action::SceneCancel {controller,revision,run} => {
                        if revision!=document.revision || !scenes.owned_by(run,controller) {return Err("scene cancellation ownership or revision mismatch".into());}
                        admission.variable_snapshot(controller,self.generation.clone(),vec![]).await.map_err(|e|format!("{e:?}"))?;
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("cancellation expired or abandoned".into());}
                        scenes.cancel(run).await.map(Reply::Cancelled)
                    },
                    Action::SceneSelectionDetails {controller,revision}=>selection_reply(admission,&self.generation,document,controller,revision).await,
                    Action::SceneSwitchStates {controller,revision}=>{
                        if revision!=document.revision {return Err("scene state revision expired".into());}
                        let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        if !admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?.mapping_readable() {return Err("scene selection runtime not ready".into());}
                        crate::scene_switches::states(document,&snapshot).map(Reply::SceneSwitchStates)
                    },
                    Action::SceneRelative{controller,revision,scene,request_id}=>{
                        if revision!=document.revision {return Err("relative revision expired".into());}
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("relative request expired".into());}
                        self.relative.start(admission,controller,self.generation.clone(),document,scene,request_id,feedback).await.map(Reply::SceneRelative)
                    },
                    Action::SceneRelativeStatus{controller,revision,request_id}=>{
                        if revision!=document.revision {return Err("relative status revision expired".into());}
                        let row=admission.native_scene_status(controller,self.generation.clone(),request_id).await.map_err(|e|format!("{e:?}"))?.ok_or("relative request not found")?;
                        Ok(Reply::SceneRelative(crate::native_scene::receipt(&row,document)))
                    },
                    Action::SceneWrites {controller,revision,scenes:writes}=>{
                        if revision!=document.revision {return Err("scene write revision expired".into());}
                        if writes.iter().any(|(id,_)|document.immediate_scenes.contains_key(id)) {
                            return Box::pin(crate::immediate_scene::writes(admission,document,&self.generation,controller,&writes,causal.as_deref_mut(),scenes,feedback,max_age_ms,self.budget,request.deadline)).await;
                        }
                        let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        let plan=crate::scene_switches::plan(document,&writes,&snapshot)?;
                        if request.reply.is_closed()||Instant::now()>=request.deadline {return Err("scene writes expired or abandoned".into());}
                        if plan.definitions.is_empty(){Ok(Reply::ScenesAccepted(vec![]))}
                        else{scenes.start_documents_with_variables_as(revision,plan.definitions,admission.monotonic_ms(),controller,plan.updates).await.map(Reply::ScenesAccepted)}
                    },
                    Action::SceneActions {controller,revision,scenes:actions} => {
                        if revision!=document.revision {return Err("scene batch revision expired".into());}
                        if actions.iter().any(|(id,_)|document.immediate_scenes.contains_key(id)) {
                            let writes=actions.iter().map(|(id,on)|(id.clone(),on.then_some(true))).collect::<Vec<_>>();
                            return Box::pin(crate::immediate_scene::writes(admission,document,&self.generation,controller,&writes,causal.as_deref_mut(),scenes,feedback,max_age_ms,self.budget,request.deadline)).await;
                        }
                        for (id,_) in &actions {
                            let definition=document.timed_scenes.get(id).ok_or("unknown timed scene")?;
                            if !definition.enabled || definition.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {
                                return Err("timed scene disabled or unreviewed".into());
                            }
                        }
                        admission.variable_snapshot(controller,self.generation.clone(),vec![]).await.map_err(|e|format!("{e:?}"))?;
                        if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("scene batch expired or abandoned".into());}
                        let writes=actions.into_iter().map(|(id,execute)|(id,execute.then_some(true))).collect::<Vec<_>>();
                        let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        let plan=crate::scene_switches::plan(document,&writes,&snapshot)?;
                        if request.reply.is_closed()||Instant::now()>=request.deadline {return Err("scene batch expired or abandoned".into());}
                        if plan.definitions.is_empty(){Ok(Reply::ScenesAccepted(vec![]))}
                        else{scenes.start_documents_with_variables_as(revision,plan.definitions,admission.monotonic_ms(),controller,plan.updates).await.map(Reply::ScenesAccepted)}
                    },
                    Action::SceneAction { controller, revision, scene, execute } => {
                        if revision != document.revision { return Err("scene action revision expired".into()); }
                        if document.immediate_scenes.contains_key(&scene) {
                            let reply=Box::pin(crate::immediate_scene::writes(admission,document,&self.generation,controller,&[(scene,Some(execute))],causal,scenes,feedback,max_age_ms,self.budget,request.deadline)).await?;
                            return Ok(match reply {Reply::ScenesAccepted(_)=>Reply::SceneAccepted(None),other=>other});
                        }
                        let definition = document.timed_scenes.get(&scene).ok_or("unknown timed scene")?;
                        if !definition.enabled || definition.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {
                            return Err("timed scene disabled or unreviewed".into());
                        }
                        admission.variable_snapshot(controller, self.generation.clone(), vec![]).await.map_err(|e|format!("{e:?}"))?;
                        if request.reply.is_closed() || Instant::now() >= request.deadline { return Err("scene action expired or abandoned".into()); }
                        if crate::scene_switches::related(document,&scene) {
                            crate::scene_reconciliation::tick(admission,controller,document,scenes,feedback).await?;
                            let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                            let plan=crate::scene_switches::plan(document,&[(scene,Some(execute))],&snapshot)?;
                            if request.reply.is_closed()||Instant::now()>=request.deadline {return Err("scene action expired or abandoned".into());}
                            if plan.definitions.is_empty(){Ok(Reply::SceneAccepted(None))}
                            else{scenes.start_documents_with_variables_as(revision,plan.definitions,admission.monotonic_ms(),controller,plan.updates).await.map(|runs|Reply::SceneAccepted(runs.into_iter().next()))}
                        }else if execute { scenes.start_document_as(document, &scene, admission.monotonic_ms(), controller).await.map(|run|Reply::SceneAccepted(Some(run))) }
                        else { Ok(Reply::SceneAccepted(None)) }
                    },
                    Action::Start(id) => {
                        if crate::scene_switches::related(document,&id){
                            let snapshot=admission.scene_selection_values(self.controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                            let plan=crate::scene_switches::forced_plan(document,&id,&snapshot)?;
                            if request.reply.is_closed()||Instant::now()>=request.deadline {return Err("scene start expired or abandoned".into());}
                            let runs=scenes.start_documents_with_variables_as(document.revision,plan.definitions,admission.monotonic_ms(),self.controller,plan.updates).await?;
                            runs.into_iter().next().map(Reply::Started).ok_or("manual activation receipt missing".into())
                        }
                        else{scenes.start_document(document,&id,admission.monotonic_ms()).await.map(Reply::Started)}
                    },
                    Action::Cancel(run) => scenes.cancel(run).await.map(Reply::Cancelled),
                    Action::Status(run) => Ok(Reply::Status(scenes.status(run))),
                    Action::Activity(run) => scenes.activity(run,feedback,max_age_ms).await.map(Reply::Activity),
                    Action::SceneActivity(id) => crate::scene_tracking::activity(document,&id,admission,self.controller,&self.generation,feedback,max_age_ms).await.map(|v|Reply::Activity(Some(v))),
                    Action::SceneActivityWrites{controller,revision,scenes:writes}=>{
                        if revision!=document.revision || writes.is_empty() || writes.len()>64{return Err("activity role request invalid".into());}
                        if !admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?.ready(){return Err("activity role runtime unavailable".into());}
                        if writes.iter().any(|(id,_)|!document.scene_tracking.get(id).is_some_and(|p|p.enabled)){return Err("unknown activity role".into());}
                        if writes.iter().any(|(_,value)|value.is_some()){return Err("activity role is read-only".into());}
                        Ok(Reply::ScenesAccepted(vec![]))
                    },
                    Action::SceneCatalog {controller,revision,scenes:ids}=>{
                        if revision!=document.revision || ids.len()>2048 || ids.iter().collect::<std::collections::BTreeSet<_>>().len()!=ids.len() {return Err("scene catalog revision or scope invalid".into());}
                        let diagnostics=admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("catalog authorization: {e:?}"))?;
                        if !diagnostics.mapping_readable(){return Err("scene catalog runtime not ready".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("catalog mapping: {e:?}"))?;
                        let mut entries=std::collections::BTreeMap::new();
                        for id in ids {
                            if request.reply.is_closed() || Instant::now()>=request.deadline {return Err("scene catalog expired or abandoned".into());}
                            let execution_kind=document.scene_execution_kind(&id).ok_or("unknown enabled scene")?;
                            let gateways=crate::gateway_runtime::scene_devices(document,&id)?.into_iter().map(|device|bundle.configuration.binding(&device).map(|(d,_)|d.gateway.clone()).ok_or("scene target unavailable")).collect::<Result<std::collections::BTreeSet<_>,_>>()?;
                            let views=diagnostics.gateways.iter().filter(|view|gateways.contains(&view.gateway)).cloned().collect::<Vec<_>>();
                            let available=if diagnostics.gateways.is_empty(){diagnostics.gateway_ready}else{views.len()==gateways.len()&&views.iter().all(|view|view.state=="ready")};
                            let tracking=crate::scene_tracking::read(document,&id,admission,controller,&self.generation,feedback).await?;
                            let activity=if let Some(value)=&tracking {value.activity}else{crate::scene_tracking::activity(document,&id,admission,controller,&self.generation,feedback,max_age_ms).await.unwrap_or(crate::scene_activity::SceneActivity::Unknown)};
                            let last_run=if execution_kind==homesense_config::rule_document::SceneExecutionKind::Immediate {None}else{admission.scene_history(controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:id.clone()}).await.map_err(|e|format!("catalog history: {e:?}"))?.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})};
                            let room=document.scene_rooms.get(&id).cloned();
                            entries.insert(id,SceneCatalogEntry{room,execution_kind,action_gate:crate::gateway_runtime::ActionGate{available,required_gateways:views},tracking,activity,last_run});
                        }
                        Ok(Reply::SceneCatalog(entries))
                    },
                    Action::SceneTracking {controller,revision,scene}=>{
                        if revision!=document.revision{return Err("tracking revision expired".into());}
                        if !admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("tracking authorization: {e:?}"))?.mapping_readable(){return Err("tracking runtime not ready".into());}
                        crate::scene_tracking::read(document,&scene,admission,controller,&self.generation,feedback).await.map(Reply::SceneTracking)
                    },
                    Action::Retire(run) => scenes.retire(run).map(Reply::Retired),
                    Action::SceneHistory {controller,revision,scene,before,limit} => {
                        if revision!=document.revision || !document.timed_scenes.contains_key(&scene) {return Err("scene history revision or definition mismatch".into());}
                        admission.scene_history_page(controller,self.generation.clone(),scene,before,limit).await.map(Reply::SceneHistory).map_err(|e|format!("{e:?}"))
                    },
                    Action::RunDetails {controller,revision,query} => {
                        if revision!=document.revision || matches!(query,homesense_storage::scene_run::Query::Latest{..}) {return Err("run detail query invalid".into());}
                        let page=admission.scene_history(controller,self.generation.clone(),query).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::RunDetails(page.and_then(|p|p.record.definition.map(|scene|{
                            let commands=p.commands.into_iter().map(|r|serde_json::json!({"device":r.device,"command":r.command_id.to_string(),"step":r.step,"sample":r.sample,"status":format!("{:?}",r.status)})).collect::<Vec<_>>();
                            let cancellations=p.cancellations.into_iter().map(|r|serde_json::json!({"device":r.cancellation.device.as_str(),"version":r.version.to_string(),"reason":format!("{:?}",r.cancellation.reason)})).collect::<Vec<_>>();
                            let expiries=p.expiries.into_iter().map(|r|serde_json::json!({"device":r.expiry.device.as_str(),"version":r.version.to_string(),"step":r.expiry.index})).collect::<Vec<_>>();
                            (scene,serde_json::json!({"boot":p.boot,"run":p.run,"revision":p.record.revision.to_string(),"record_version":p.record.version.to_string(),"status":format!("{:?}",p.record.status),"commands":commands,"cancellations":cancellations,"expiries":expiries,"next_after":p.next_after.map(|v|v.to_string())}))
                        }))))
                    },
                    Action::RunSummary {controller,revision,boot,run} => {
                        if revision!=document.revision {return Err("run query revision expired".into());}
                        let page=admission.scene_history(controller,self.generation.clone(),homesense_storage::scene_run::Query::Run{boot,run,after:0,limit:1}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::RunSummary(page.and_then(|p|p.record.definition.clone().map(|scene|(scene,LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})))))
                    },
                    Action::SceneAvailability{controller,revision,scene}=>{
                        if revision!=document.revision{return Err("scene availability revision expired".into());}
                        let diagnostics=admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("availability authorization: {e:?}"))?;
                        if !diagnostics.mapping_readable(){return Err("scene availability runtime unavailable".into());}
                        let bundle=admission.automation_bundle(controller,self.generation.clone()).await.map_err(|e|format!("availability mapping: {e:?}"))?;
                        let mut gateways=std::collections::BTreeSet::new();
                        for id in crate::gateway_runtime::scene_devices(document,&scene)? {
                            gateways.insert(bundle.configuration.binding(&id).ok_or("scene target unavailable")?.0.gateway.clone());
                        }
                        let local=!diagnostics.gateways.is_empty();
                        let views=diagnostics.gateways.into_iter().filter(|view|gateways.contains(&view.gateway)).collect::<Vec<_>>();
                        let available=if !local {diagnostics.gateway_ready}else{views.len()==gateways.len()&&views.iter().all(|view|view.state=="ready")};
                        Ok(Reply::SceneAvailability(crate::gateway_runtime::ActionGate {available,required_gateways:views}))
                    },
                    Action::SceneDefinition{controller,revision,scene}=>{
                        if revision!=document.revision{return Err("scene definition revision expired".into());}
                        admission.variable_snapshot(controller,self.generation.clone(),vec![]).await.map_err(|e|format!("{e:?}"))?;
                        document.scene_execution_kind(&scene).map(Reply::SceneDefinition).ok_or("unknown enabled scene".into())
                    },
                    Action::LastRun(id) => {
                        if document.scene_execution_kind(&id)==Some(homesense_config::rule_document::SceneExecutionKind::Immediate){return Ok(Reply::LastRun(None));}
                        if !document.timed_scenes.contains_key(&id) { return Err("unknown timed scene".into()); }
                        let page=admission.scene_history(self.controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:id}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::LastRun(page.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})))
                    },
                }
            })
            .await;
            if result.is_ok() && !request.reply.is_closed()
                && let (Some(scene),Some(sender))=(watch_scene,request.watch) {
                self.watchers.push_back(Watcher{scene,sender,next:Instant::now()});
            }
            if let Some(kind)=read_kind && (queued_ms>=100 || handling_started.elapsed()>=Duration::from_millis(100)) {
                log::warn!("automation_request_lag kind={kind} phase=complete queue_ms={queued_ms} handler_ms={} abandoned={}",handling_started.elapsed().as_millis(),request.reply.is_closed());
            }
            let _ = request.reply.send(result);
        }
        // At most one snapshot per cycle, and no more than 10 Hz per watcher.
        if let Some(mut watcher)=self.watchers.pop_front()
            && !watcher.sender.is_closed() {
                if Instant::now()>=watcher.next {
                    let result=async {
                        let tracking=crate::scene_tracking::read(document,&watcher.scene,admission,self.controller,&self.generation,feedback).await?;
                        let activity=if let Some(value)=&tracking {value.activity}else{crate::scene_tracking::activity(document,&watcher.scene,admission,self.controller,&self.generation,feedback,max_age_ms).await?};
                        let tracking_valid_until_ms=tracking.and_then(|p|p.valid_until_ms);
                        let page=admission.scene_history(self.controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:watcher.scene.clone()}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(SceneObservation{boot:admission.boot_id().to_owned(),revision:document.revision,activity,tracking_valid_until_ms,last_run:page.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})})
                    }.await;
                    watcher.sender.send_if_modified(|current| {
                        if current.as_ref()==Some(&result) {false} else {*current=Some(result);true}
                    });
                    watcher.next=Instant::now()+Duration::from_millis(100);
                }
                self.watchers.push_back(watcher);
        }
    }
    pub(crate) fn close(&mut self) {
        if let Ok(mut reader)=self.selection_reader.lock(){*reader=None;}
        for watcher in self.watchers.drain(..) {
            watcher.sender.send_replace(Some(Err("automation runtime stopped".into())));
        }
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

#[cfg(test)]
#[path="ordinary_http_source_auth_tests.rs"]
mod ordinary_http_source_auth_tests;

#[cfg(test)]
#[path="selection_reader_guard_tests.rs"]
mod selection_reader_guard_tests;
