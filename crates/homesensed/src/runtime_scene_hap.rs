//! Explicit scene HAP installation over the running automation control channel.
//! No listener or physical connection is opened here. Publish only alongside a
//! running automation owner and keep polling reset events outside HAP locks.
use crate::automation_control::{Action, Control, Reply};
use crate::scene_selection_hap::{Cache, Source};
use hap::{
    characteristic::CharacteristicCallbacks,
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_hap::{
    scene_graph::{SceneBinding, SceneGraph, SceneSpec},
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

pub struct RuntimeSceneHap {
    pub accessories: Vec<(SceneBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    resets: tokio::sync::mpsc::Receiver<String>,
    selection: Cache,
    source: Option<Source>,
    writes: Arc<tokio::sync::Mutex<()>>,
    published: BTreeMap<String, bool>,
    refreshed: Option<Instant>,
}
impl RuntimeSceneHap {
    pub async fn new(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        specs: Vec<SceneSpec>,
        control: Control,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::build(store, document, specs, control, None, authorize).await
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
        let source = Source::new(admission, controller, groups).await?;
        Self::build(store, document, specs, control, Some(source), authorize).await
    }
    async fn build(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        specs: Vec<SceneSpec>,
        control: Control,
        source: Option<Source>,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let selected = specs.iter().map(|s| s.id.clone()).collect::<BTreeSet<_>>();
        let stateful = Arc::new(
            document
                .scene_switch_groups
                .iter()
                .flat_map(|g| g.members.iter())
                .filter(|m| selected.contains(&m.scene))
                .map(|m| m.scene.clone())
                .collect::<BTreeSet<_>>(),
        );
        if !stateful.is_empty() && source.is_none() {
            return Err("stateful HAP scene requires an admission state source".into());
        }
        let source = if stateful.is_empty() { None } else { source };
        let selection = Cache::default();
        if let Some(source) = &source {
            selection.install(source.sample().await.map_err(|reason| format!("{reason:?}"))?)?;
        }
        for spec in &specs {
            let scene = document
                .timed_scenes
                .get(&spec.id)
                .ok_or("unknown published scene")?;
            if !scene.enabled
                || scene
                    .review_evidence
                    .as_ref()
                    .is_none_or(|s| s.trim().is_empty())
            {
                return Err("scene disabled or unreviewed".into());
            }
        }
        let mut graph = SceneGraph::build(store, specs)
            .await
            .map_err(|e| e.to_string())?;
        let requests = SceneRequests::new(graph.bindings.clone()).map_err(|e| e.to_string())?;
        let revision = document.revision;
        let authorize = Arc::new(authorize);
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
                    let accepted = matches!(result, Ok(Reply::ScenesAccepted(_)));
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
        Ok(Self {
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
                .filter_map(|id| self.selection.read(id).selected().map(|value| (id.clone(), value)))
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
