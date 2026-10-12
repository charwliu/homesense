//! Explicit scene HAP installation over the running automation control channel.
//! No listener or physical connection is opened here. Publish only alongside a
//! running automation owner and keep polling reset events outside HAP locks.
use crate::automation_control::{Action, Control, Reply};
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
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

pub struct RuntimeSceneHap {
    pub accessories: Vec<(SceneBinding, AccessoryHandle)>,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    resets: tokio::sync::mpsc::Receiver<String>,
}
impl RuntimeSceneHap {
    pub async fn new(
        store: &IdentityStore,
        document: &homesense_config::rule_document::Document,
        specs: Vec<SceneSpec>,
        control: Control,
        authorize: impl Fn(Uuid, &str) -> bool + Send + Sync + 'static,
    ) -> Result<Self, String> {
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
        let handler = SceneWriteHandler::new(
            requests,
            move |batch: SceneBatch| {
                let control = control.clone();
                let authorize = authorize.clone();
                let send = send.clone();
                let writes = writes.clone();
                async move {
                    let _guard = writes.lock().await;
                    if batch.addressed.iter().any(|scene|!authorize(batch.controller,scene)) {
                        return Err(WriteStatus::InsufficientPrivileges);
                    }
                    let permits=send.try_reserve_many(batch.starts.len())
                        .map_err(|_|WriteStatus::ResourceBusy)?;
                    let actions=batch.addressed.iter().map(|scene|
                        (scene.clone(),batch.starts.contains(scene))).collect();
                    let reply=control.request(Action::SceneActions {
                        controller:batch.controller,revision,scenes:actions,
                    }).await.map_err(|_|WriteStatus::ServiceCommunicationFailure)?;
                    if !matches!(reply,Reply::ScenesAccepted(ref runs) if runs.len()==batch.starts.len()) {
                        return Err(WriteStatus::ServiceCommunicationFailure);
                    }
                    for (permit,scene) in permits.zip(batch.starts) {permit.send(scene);}
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
            accessory
                .switch
                .power_state
                .on_read(Some(|| Ok(Some(false))));
            accessory
                .switch
                .power_state
                .on_update(Some(|_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    Err(Box::new(std::io::Error::other(
                        "grouped scene write required",
                    )))
                }));
            let handle: AccessoryHandle =
                Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
            accessories.push((binding, handle));
        }
        Ok(Self {
            accessories,
            write_handler: Arc::new(handler),
            resets,
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
        Ok(count)
    }
}
