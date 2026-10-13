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
    ModuleQuery{controller:Uuid,revision:u64,module_id:String,scope:crate::module_queries::Scope,submit:bool},
    ModuleQueryStatus{controller:Uuid,revision:u64,module_id:String,scope:crate::module_queries::Scope,commands:Vec<u64>},
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
    DeviceCommand {controller:Uuid,revision:u64,device:homesense_core::DeviceId,action:crate::management_devices::DeviceAction},
    SceneCancel { controller: Uuid, revision: u64, run: u64 },
    SceneSwitchStates {controller:Uuid,revision:u64},
    SceneSelectionDetails {controller:Uuid,revision:u64},
    SceneWrites {controller:Uuid,revision:u64,scenes:Vec<(String,Option<bool>)>},
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
    watch: Option<tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>>,
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
    pub async fn subscribe(&self, scene: String) -> Result<SceneSubscription,String> {
        let (sender,receiver)=tokio::sync::watch::channel(None);
        self.request_inner(Action::SceneActivity(scene),Some(sender)).await?;
        Ok(receiver)
    }
    pub async fn request(&self, action: Action) -> Result<Reply, String> {
        self.request_inner(action,None).await
    }
    async fn request_inner(&self, action: Action, watch: Option<tokio::sync::watch::Sender<Option<Result<SceneObservation,String>>>>) -> Result<Reply,String> {
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
pub(crate) struct Receiver {
    receiver: mpsc::Receiver<Request>,
    controller: Uuid,
    generation: TaskGeneration,
    watchers: std::collections::VecDeque<Watcher>,
    relative: crate::native_scene::Manager,
}
pub(crate) fn channel(controller: Uuid, generation: TaskGeneration) -> (Control, Receiver) {
    let (sender, receiver) = mpsc::channel(32);
    (
        Control { sender },
        Receiver {
            receiver,
            controller,
            generation,
            watchers: Default::default(),
            relative: Default::default(),
        },
    )
}
impl Receiver {
    pub(crate) fn controller(&self)->Uuid {self.controller}

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
        self.relative.tick(admission,document,feedback).await;
        // Bound per-cycle work so incoming control cannot starve bus processing.
        for _ in 0..8 {
            let Ok(request) = self.receiver.try_recv() else {
                break;
            };
            if request.reply.is_closed() {
                continue;
            }
            self.watchers.retain(|watcher|!watcher.sender.is_closed());
            let watch_scene=if request.watch.is_some() {
                match &request.action {Action::SceneActivity(id)=>Some(id.clone()),_=>None}
            } else {None};
            let result = async {
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
                        let modules=crate::management_modules::project(&bundle.configuration,&feedback.snapshot(),Some(feedback.module_truth()),admission.monotonic_ms(),max_age_ms,diagnostics.ready()&&feedback.is_active(),0,64)?;
                        // Locate independently of pagination; the module registry can exceed 64.
                        let (location,_)=bundle.configuration.module(&module_id).ok_or("module missing")?;
                        let index=bundle.configuration.gateways().iter().flat_map(|(gateway,g)|g.modules().keys().map(move|address|(gateway,address))).position(|(gateway,address)|gateway==&location.gateway&&*address==location.address).ok_or("module missing")?;
                        let module=if index<64 {modules.into_iter().find(|m|m.module_id.as_deref()==Some(&module_id)).ok_or("module missing")?} else {crate::management_modules::project(&bundle.configuration,&feedback.snapshot(),Some(feedback.module_truth()),admission.monotonic_ms(),max_age_ms,diagnostics.ready()&&feedback.is_active(),index,1)?.pop().ok_or("module missing")?};
                        crate::module_queries::project_status(records,&module,&scope,self.generation.epochs.transport,&commands).map(Reply::ModuleQueryStatus)
                    },
                    Action::Modules {controller,revision,offset,limit} => {
                        if revision!=document.revision {return Err("module inventory revision expired".into());}
                        crate::management_modules::snapshot(admission,controller,self.generation.clone(),feedback,max_age_ms,offset,limit).await.map(Reply::Modules)
                    },
                    Action::Devices {controller,revision,ids} => {
                        if revision!=document.revision {return Err("device query revision expired".into());}
                        crate::management_devices::snapshot(admission,controller,self.generation.clone(),ids,feedback).await.map(Reply::Devices)
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
                    Action::DeviceCommand {controller,revision,device,action} => {
                        if revision!=document.revision {return Err("device command revision expired".into());}
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
                    Action::SceneSelectionDetails {controller,revision}=>{
                        if revision!=document.revision {return Err("scene state revision expired".into());}
                        let snapshot=admission.scene_selection_snapshot(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        if !admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?.ready() {return Err("scene selection runtime not ready".into());}
                        crate::scene_switches::details(document,&snapshot).map(Reply::SceneSelectionDetails)
                    },
                    Action::SceneSwitchStates {controller,revision}=>{
                        if revision!=document.revision {return Err("scene state revision expired".into());}
                        let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        if !admission.diagnostics(controller,self.generation.clone()).await.map_err(|e|format!("{e:?}"))?.ready() {return Err("scene selection runtime not ready".into());}
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
                        let snapshot=admission.scene_selection_values(controller,self.generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|e|format!("{e:?}"))?;
                        let plan=crate::scene_switches::plan(document,&writes,&snapshot)?;
                        if request.reply.is_closed()||Instant::now()>=request.deadline {return Err("scene writes expired or abandoned".into());}
                        if plan.definitions.is_empty(){Ok(Reply::ScenesAccepted(vec![]))}
                        else{scenes.start_documents_with_variables_as(revision,plan.definitions,admission.monotonic_ms(),controller,plan.updates).await.map(Reply::ScenesAccepted)}
                    },
                    Action::SceneActions {controller,revision,scenes:actions} => {
                        if revision!=document.revision {return Err("scene batch revision expired".into());}
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
                        let definition = document.timed_scenes.get(&scene).ok_or("unknown timed scene")?;
                        if !definition.enabled || definition.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {
                            return Err("timed scene disabled or unreviewed".into());
                        }
                        admission.variable_snapshot(controller, self.generation.clone(), vec![]).await.map_err(|e|format!("{e:?}"))?;
                        if request.reply.is_closed() || Instant::now() >= request.deadline { return Err("scene action expired or abandoned".into()); }
                        if crate::scene_switches::related(document,&scene) {
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
                    Action::SceneActivity(id) => {
                        let scene=document.timed_scenes.get(&id).ok_or("unknown timed scene")?;
                        if !scene.enabled || scene.review_evidence.as_ref().is_none_or(|s|s.trim().is_empty()) {return Err("timed scene disabled or unreviewed".into());}
                        crate::scene_activity::Targets::new(&scene.steps).project(admission,self.controller,&self.generation,feedback,max_age_ms).await.map(|a|Reply::Activity(Some(a)))
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
                    Action::LastRun(id) => {
                        if !document.timed_scenes.contains_key(&id) { return Err("unknown timed scene".into()); }
                        let page=admission.scene_history(self.controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:id}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(Reply::LastRun(page.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})))
                    },
                }
            }
            .await;
            if result.is_ok() && !request.reply.is_closed()
                && let (Some(scene),Some(sender))=(watch_scene,request.watch) {
                self.watchers.push_back(Watcher{scene,sender,next:Instant::now()});
            }
            let _ = request.reply.send(result);
        }
        // At most one snapshot per cycle, and no more than 10 Hz per watcher.
        if let Some(mut watcher)=self.watchers.pop_front()
            && !watcher.sender.is_closed() {
                if Instant::now()>=watcher.next {
                    let result=async {
                        let scene=document.timed_scenes.get(&watcher.scene).ok_or("scene removed")?;
                        let activity=crate::scene_activity::Targets::new(&scene.steps).project(admission,self.controller,&self.generation,feedback,max_age_ms).await?;
                        let page=admission.scene_history(self.controller,self.generation.clone(),homesense_storage::scene_run::Query::Latest{definition:watcher.scene.clone()}).await.map_err(|e|format!("{e:?}"))?;
                        Ok(SceneObservation{boot:admission.boot_id().to_owned(),revision:document.revision,activity,last_run:page.map(|p|LastRun{boot:p.boot,run:p.run,revision:p.record.revision,started_ms:p.record.started_ms,updated_ms:p.record.updated_ms,version:p.record.version,status:p.record.status,cause:p.cause})})
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
