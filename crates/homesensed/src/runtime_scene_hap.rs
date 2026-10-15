//! Explicit scene HAP installation over the running automation control channel.
//! No listener or physical connection is opened here. Publish only alongside a
//! running automation owner and keep polling reset events outside HAP locks.
use crate::automation_control::{Action, Control, Reply};
use crate::scene_selection_hap::{Cache, Source};
use hap::{
    characteristic::{AsyncCharacteristicCallbacks, CharacteristicCallbacks, HapCharacteristic},
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_hap::{
    scene_graph::{SceneActivityBinding, SceneActivityGraph, SceneBinding, SceneGraph, SceneSpec},
    scene_requests::{SceneBatch, SceneRequests},
    scene_write::SceneWriteHandler,
    storage::IdentityStore,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct TrackingGuard {
    admission: crate::admission::Admission,
    controller: Uuid,
    generation: crate::admission::TaskGeneration,
}
pub struct RuntimeSceneHap {
    pub accessories: Vec<(SceneBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    pub activities: Vec<(SceneActivityBinding, AccessoryHandle)>,
    pub activity_write_handler: Option<Arc<dyn WriteBatchHandler>>,
    resets: tokio::sync::mpsc::Receiver<String>,
    selection: Cache,
    source: Option<Source>,
    writes: Arc<tokio::sync::Mutex<()>>,
    published: BTreeMap<String, bool>,
    refreshed: Option<Instant>,
    tracking_ids: BTreeSet<String>,
    tracking_guard: Option<TrackingGuard>,
    tracking_install:
        tokio::sync::mpsc::Receiver<(String, crate::automation_control::SceneSubscription)>,
    tracking_subscriptions: BTreeMap<String, crate::automation_control::SceneSubscription>,
    tracking_published: BTreeMap<String, Option<bool>>,
    tracking_tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl RuntimeSceneHap {
    pub async fn new(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        specs: Vec<SceneSpec>,
        control: Control,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::build(store, document, specs, control, None, None, authorize).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_admission(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        specs: Vec<SceneSpec>,
        control: Control,
        admission: crate::admission::Admission,
        controller: Uuid,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let groups = document
            .scene_switch_groups
            .iter()
            .filter(|g| {
                g.members
                    .iter()
                    .any(|m| specs.iter().any(|s| s.id == m.scene))
            })
            .cloned()
            .collect();
        let guard = TrackingGuard {
            generation: admission
                .task_generation()
                .await
                .map_err(|e| format!("{e:?}"))?,
            admission: admission.clone(),
            controller,
        };
        let source = Source::new(admission, controller, groups).await?;
        Self::build(
            store,
            document,
            specs,
            control,
            Some(source),
            Some(guard),
            authorize,
        )
        .await
    }
    async fn build(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        mut specs: Vec<SceneSpec>,
        control: Control,
        source: Option<Source>,
        tracking_controller: Option<TrackingGuard>,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let selected = specs.iter().map(|s| s.id.clone()).collect::<BTreeSet<_>>();
        let selection_members = document
            .scene_switch_groups
            .iter()
            .flat_map(|g| g.members.iter())
            .filter(|m| selected.contains(&m.scene))
            .map(|m| m.scene.clone())
            .collect::<BTreeSet<_>>();
        let tracking = document
            .scene_tracking
            .iter()
            .filter(|(id, p)| p.enabled && selected.contains(*id))
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        if !selection_members.is_empty() && source.is_none() {
            return Err("stateful HAP scene requires an admission state source".into());
        }
        if !tracking.is_empty() && tracking_controller.is_none() {
            return Err("tracking HAP scene requires an admission controller".into());
        }
        if !selection_members.is_disjoint(&tracking) {
            return Err("conflicting scene selection and tracking providers".into());
        }
        let stateful = Arc::new(selection_members.clone());
        let source = if selection_members.is_empty() {
            None
        } else {
            source
        };
        for spec in &mut specs {
            if let Some(name) = document.scene_metadata.get(&spec.id) {
                spec.name = name.clone();
            }
        }
        if tracking.len() > 16 {
            return Err("tracking HAP subscription capacity exceeds 16".into());
        }
        let tracking_control = control.clone();
        let selection = Cache::default();
        if let Some(source) = &source {
            selection.install(
                source
                    .sample()
                    .await
                    .map_err(|reason| format!("{reason:?}"))?,
            )?;
        }
        for spec in &specs {
            if document.scene_execution_kind(&spec.id).is_none(){return Err("scene missing, disabled or unreviewed".into());}
            if document.immediate_scenes.contains_key(&spec.id){document.linked_manual_scenes(std::slice::from_ref(&spec.id)).map_err(|e|e.to_string())?;}
        }
        let mut graph = SceneGraph::build(store, specs)
            .await
            .map_err(|e| e.to_string())?;
        let activity_specs = tracking
            .iter()
            .map(|id| SceneSpec {
                id: id.clone(),
                name: document.scene_tracking[id].activity_name.clone(),
            })
            .collect();
        let mut activity_graph = SceneActivityGraph::build(store, activity_specs)
            .await
            .map_err(|e| e.to_string())?;
        let requests = SceneRequests::new(graph.bindings.clone()).map_err(|e| e.to_string())?;
        let revision = document.revision;
        let authorize = Arc::new(authorize);
        let activity_authorize = authorize.clone();
        let (send, resets) = tokio::sync::mpsc::channel::<String>(32);
        let writes = Arc::new(tokio::sync::Mutex::new(()));
        // The permit is reserved before runtime acceptance. The reset callback
        // is part of the submit closure so no accepted action loses its queue slot.
        let cache = selection.clone();
        let state_source = source.clone();
        let serial = writes.clone();
        let handler = SceneWriteHandler::new(
            requests,
            move |batch: SceneBatch| {
                let control = control.clone();
                let authorize = authorize.clone();
                let send = send.clone();
                let writes = serial.clone();
                let stateful = stateful.clone();
                let cache = cache.clone();
                let state_source = state_source.clone();
                async move {
                    let _guard = writes.lock().await;
                    if batch
                        .addressed
                        .iter()
                        .any(|scene| !authorize(batch.controller, scene))
                    {
                        return Err(WriteStatus::InsufficientPrivileges);
                    }
                    let momentary = batch
                        .starts
                        .iter()
                        .filter(|id| !stateful.contains(*id))
                        .cloned()
                        .collect::<Vec<_>>();
                    let permits = send
                        .try_reserve_many(momentary.len())
                        .map_err(|_| WriteStatus::ResourceBusy)?;
                    let actions = batch
                        .addressed
                        .iter()
                        .map(|scene| (scene.clone(), batch.values.get(scene).copied()))
                        .collect();
                    // Freeze old values while a group write is in flight. This
                    // prevents a delayed projection from publishing the old mode.
                    let changes_selection = state_source.is_some() && !batch.values.is_empty();
                    if changes_selection {
                        cache.invalidate();
                    }
                    let result = control
                        .request(Action::SceneWrites {
                            controller: batch.controller,
                            revision,
                            scenes: actions,
                        })
                        .await;
                    let accepted = matches!(result, Ok(Reply::ScenesAccepted(_) | Reply::ImmediateAccepted(_)));
                    if accepted {
                        for (permit, scene) in permits.zip(momentary) {
                            permit.send(scene);
                        }
                    }
                    if changes_selection {
                        match state_source.as_ref().unwrap().sample().await {
                            Ok(values) => cache
                                .install(values)
                                .map_err(|_| WriteStatus::ServiceCommunicationFailure)?,
                            Err(reason) => {
                                cache.unavailable(reason);
                                return Err(WriteStatus::ServiceCommunicationFailure);
                            }
                        }
                    }
                    if !accepted {
                        return Err(WriteStatus::ServiceCommunicationFailure);
                    }
                    Ok(())
                }
            },
            |_: Vec<String>| async { Ok(()) },
        );
        let mut accessories = Vec::new();
        for binding in graph.bindings {
            let mut accessory = graph
                .scenes
                .remove(&binding.scene)
                .ok_or("missing scene accessory")?;
            if document
                .scene_switch_groups
                .iter()
                .flat_map(|g| g.members.iter())
                .any(|m| m.scene == binding.scene)
            {
                let cache = selection.clone();
                let id = binding.scene.clone();
                accessory.switch.power_state.on_read(Some(move || {
                    cache.read(&id).selected().map(Some).ok_or_else(|| {
                        Box::new(std::io::Error::other("scene selection unknown or stale"))
                            as Box<dyn std::error::Error + Send + Sync>
                    })
                }));
            } else {
                accessory
                    .switch
                    .power_state
                    .on_read(Some(|| Ok(Some(false))));
            }
            accessory.switch.power_state.on_update(Some(
                |_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    Err(Box::new(std::io::Error::other(
                        "grouped scene write required",
                    )))
                },
            ));
            let handle: AccessoryHandle =
                Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
            accessories.push((binding, handle));
        }
        let mut activities = vec![];
        for binding in activity_graph.bindings {
            let mut accessory = activity_graph
                .activities
                .remove(&binding.scene)
                .ok_or("activity accessory missing")?;
            accessory.switch.power_state.set_perms(vec![
                hap::characteristic::Perm::PairedRead,
                hap::characteristic::Perm::Events,
            ]);
            let control = tracking_control.clone();
            let id = binding.scene.clone();
            let controller = tracking_controller
                .as_ref()
                .ok_or("tracking controller missing")?
                .controller;
            accessory.switch.power_state.on_read_async(Some(move || -> hap::futures::future::BoxFuture<'static, Result<Option<bool>,Box<dyn std::error::Error+Send+Sync>>> {
                    let control=control.clone();let id=id.clone();
                    Box::pin(async move {
                        let result=tokio::time::timeout(Duration::from_millis(500),control.request(Action::SceneTracking{controller,revision,scene:id})).await;
                        if let Ok(Ok(Reply::SceneTracking(Some(value))))=result && let Some(active)=value.active(){return Ok(Some(active));}
                        Err(Box::new(std::io::Error::other("actual scene tracking unknown, pending, stale or unavailable")) as Box<dyn std::error::Error+Send+Sync>)
                    })
                }));

            accessory.switch.power_state.on_update(Some(
                |_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    Err(Box::new(std::io::Error::other(
                        "activity role is read-only",
                    )))
                },
            ));
            let handle: AccessoryHandle =
                Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
            activities.push((binding, handle));
        }
        let activity_write_handler: Option<Arc<dyn WriteBatchHandler>> = if activities.is_empty() {
            None
        } else {
            let bindings = activities
                .iter()
                .map(|(b, _)| SceneBinding {
                    scene: b.scene.clone(),
                    aid: b.aid,
                    on_iid: b.on_iid,
                })
                .collect();
            let requests = SceneRequests::new(bindings).map_err(|e| e.to_string())?;
            let control = tracking_control.clone();
            Some(Arc::new(SceneWriteHandler::new(
                requests,
                move |batch: SceneBatch| {
                    let authorize = activity_authorize.clone();
                    let control = control.clone();
                    async move {
                        if batch
                            .addressed
                            .iter()
                            .any(|id| !authorize(batch.controller, id))
                        {
                            return Err(WriteStatus::InsufficientPrivileges);
                        }
                        if !batch.values.is_empty() {
                            return Err(WriteStatus::ReadOnlyCharacteristic);
                        }
                        let scenes = batch.addressed.into_iter().map(|id| (id, None)).collect();
                        match control
                            .request(Action::SceneActivityWrites {
                                controller: batch.controller,
                                revision,
                                scenes,
                            })
                            .await
                        {
                            Ok(Reply::ScenesAccepted(v)) if v.is_empty() => Ok(()),
                            _ => Err(WriteStatus::ServiceCommunicationFailure),
                        }
                    }
                },
                |_: Vec<String>| async { Ok(()) },
            )))
        };
        let (install, tracking_install) = tokio::sync::mpsc::channel(16);
        let mut tracking_tasks = vec![];
        for id in &tracking {
            let id = id.clone();
            let control = tracking_control.clone();
            let install = install.clone();
            tracking_tasks.push(tokio::spawn(async move {
                loop {
                    if install.is_closed() {
                        return;
                    }
                    if let Ok(subscription) = control.subscribe(id.clone()).await {
                        let _ = install.send((id, subscription)).await;
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }));
        }
        Ok(Self {
            tracking_ids: tracking,
            tracking_guard: tracking_controller,
            tracking_install,
            tracking_subscriptions: Default::default(),
            tracking_published: Default::default(),
            tracking_tasks,
            activities,
            activity_write_handler,
            accessories,
            write_handler: Arc::new(handler),
            resets,
            selection,
            source,
            writes,
            published: BTreeMap::new(),
            refreshed: None,
        })
    }
    /// Call outside the HTTP batch handler, which holds accessory locks. Reset
    /// is a virtual trigger acknowledgment, never evidence of scene completion.
    pub async fn flush_resets(&mut self) -> Result<usize, String> {
        self.publish_tracking().await?;
        let handles: BTreeMap<_, _> = self
            .accessories
            .iter()
            .map(|(b, h)| (b.scene.clone(), (b.on_iid, h.clone())))
            .collect();
        let mut count = 0;
        while count < 32 {
            let Ok(scene) = self.resets.try_recv() else {
                break;
            };
            let (iid, handle) = handles.get(&scene).ok_or("missing reset accessory")?;
            let mut accessory = handle.lock().await;
            let characteristic = accessory
                .get_mut_services()
                .into_iter()
                .flat_map(|s| s.get_mut_characteristics())
                .find(|c| c.get_id() == *iid)
                .ok_or("missing reset characteristic")?;
            characteristic
                .set_observed_value(hap::serde_json::json!(false))
                .await
                .map_err(|e| e.to_string())?;
            count += 1;
        }
        if let Some(source) = &self.source {
            if self
                .refreshed
                .is_none_or(|time| time.elapsed() >= Duration::from_millis(100))
            {
                let _guard = self.writes.lock().await;
                match source.sample().await {
                    Ok(values) => self.selection.install(values)?,
                    Err(reason) => self.selection.unavailable(reason),
                }
                self.refreshed = Some(Instant::now());
            }
            // Off notifications precede On. Reads use one coherent cached map,
            // and the value is rechecked after each accessory lock is acquired.
            let mut values = handles
                .keys()
                .filter_map(|id| {
                    self.selection
                        .read(id)
                        .selected()
                        .map(|value| (id.clone(), value))
                })
                .collect::<Vec<_>>();
            values.sort_by_key(|(_, on)| *on);
            for (scene, on) in values {
                if self.published.get(&scene) == Some(&on) {
                    continue;
                }
                let (iid, handle) = &handles[&scene];
                let mut accessory = handle.lock().await;
                if self.selection.read(&scene).selected() != Some(on) {
                    continue;
                }
                let characteristic = accessory
                    .get_mut_services()
                    .into_iter()
                    .flat_map(|s| s.get_mut_characteristics())
                    .find(|c| c.get_id() == *iid)
                    .ok_or("missing selection characteristic")?;
                characteristic
                    .set_observed_value(hap::serde_json::json!(on))
                    .await
                    .map_err(|e| e.to_string())?;
                self.published.insert(scene, on);
            }
        }
        Ok(count)
    }
}

impl RuntimeSceneHap {
    pub(crate) fn has_tracking(&self) -> bool {
        !self.tracking_ids.is_empty()
    }
    async fn tracking_ready(&self) -> bool {
        if let Some(guard) = &self.tracking_guard {
            guard
                .admission
                .diagnostics(guard.controller, guard.generation.clone())
                .await
                .is_ok_and(|s| s.ready())
        } else {
            false
        }
    }
    fn tracking_value(&self, id: &str, ready: bool) -> Option<bool> {
        if !ready {
            return None;
        }
        let guard = self.tracking_guard.as_ref()?;
        let now = guard.admission.monotonic_ms();
        let stream = self.tracking_subscriptions.get(id)?;
        if stream.has_changed().is_err() {
            return None;
        }
        let current = stream.borrow();
        let observation = current.as_ref()?.as_ref().ok()?;
        if observation.boot != guard.admission.boot_id()
            || observation.revision != guard.generation.epochs.configuration
            || observation
                .tracking_valid_until_ms
                .is_none_or(|until| now >= until)
        {
            return None;
        }
        match observation.activity {
            crate::scene_activity::SceneActivity::Active => Some(true),
            crate::scene_activity::SceneActivity::Inactive => Some(false),
            _ => None,
        }
    }
    async fn publish_tracking(&mut self) -> Result<(), String> {
        if self.tracking_ids.is_empty() {
            return Ok(());
        }
        while let Ok((id, subscription)) = self.tracking_install.try_recv() {
            self.tracking_subscriptions.insert(id, subscription);
        }
        let ready = self.tracking_ready().await;
        for id in self.tracking_ids.clone() {
            let active = self.tracking_value(&id, ready);
            if self.tracking_published.get(&id) == Some(&active) {
                continue;
            }
            let (binding, handle) = self
                .activities
                .iter()
                .find(|(b, _)| b.scene == id)
                .ok_or("tracking accessory missing")?;
            let mut accessory = handle.lock().await;
            // Waiting on a HAP lock must not extend a sample or authority lease.
            let active = self.tracking_value(&id, self.tracking_ready().await);
            if self.tracking_published.get(&id) == Some(&active) {
                continue;
            }
            accessory
                .get_mut_services()
                .into_iter()
                .flat_map(|s| s.get_mut_characteristics())
                .find(|c| c.get_id() == binding.on_iid)
                .ok_or("tracking characteristic missing")?
                .set_observed_value(active.map_or(serde_json::Value::Null, serde_json::Value::Bool))
                .await
                .map_err(|e| e.to_string())?;
            self.tracking_published.insert(id, active);
        }
        Ok(())
    }
    pub(crate) async fn invalidate_tracking(&mut self) -> Result<(), String> {
        for id in self.tracking_ids.clone() {
            if self.tracking_published.get(&id) == Some(&None) {
                continue;
            }
            let (binding, handle) = self
                .activities
                .iter()
                .find(|(b, _)| b.scene == id)
                .ok_or("tracking accessory missing")?;
            let mut accessory = handle.lock().await;
            accessory
                .get_mut_services()
                .into_iter()
                .flat_map(|s| s.get_mut_characteristics())
                .find(|c| c.get_id() == binding.on_iid)
                .ok_or("tracking characteristic missing")?
                .set_observed_value(serde_json::Value::Null)
                .await
                .map_err(|e| e.to_string())?;
            self.tracking_published.insert(id, None);
        }
        Ok(())
    }
}
impl Drop for RuntimeSceneHap {
    fn drop(&mut self) {
        for task in &self.tracking_tasks {
            task.abort();
        }
    }
}
