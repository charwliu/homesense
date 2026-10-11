//! Supervised HAP publication sharing the automation owner's admission and snapshots.
use crate::{admission::Admission, automation_runtime::AutomationRuntime};
use hap::{server::IpServer, storage::Storage};
use homesense_hap::storage::IdentityStore;
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
};
use uuid::Uuid;
#[derive(Clone)]
pub struct Settings {
    pub address: SocketAddr,
    pub scenes: Vec<String>,
}
impl Settings {
    pub(crate) fn validate(
        &self,
        config: &homesense_config::Configuration,
        document: &homesense_config::rule_document::Document,
    ) -> Result<(), String> {
        let allowed = self.address.ip().is_loopback()
            || match self.address.ip() {
                IpAddr::V4(ip) => ip.is_private(),
                IpAddr::V6(ip) => ip.is_unique_local(),
            };
        if !allowed || self.address.port() == 0 {
            return Err("HAP requires an explicit private or loopback endpoint".into());
        }
        let unique: std::collections::BTreeSet<_> = self.scenes.iter().collect();
        if unique.len() != self.scenes.len() || self.scenes.len() > 64 {
            return Err("invalid HAP scene selection".into());
        }
        for id in &self.scenes {
            if !document.timed_scenes.get(id).is_some_and(|s| {
                s.enabled
                    && s.review_evidence
                        .as_ref()
                        .is_some_and(|v| !v.trim().is_empty())
            }) {
                return Err("published HAP scene missing, disabled or unreviewed".into());
            }
        }
        // Motor and keypad graphs require their own explicit publication policy.
        // Reject unsupported candidates before retiring the current graph.
        for (id, device) in config.devices() {
            if device.enabled
                && !config.binding(id).is_some_and(|(_, module)| {
                    matches!(
                        module.kind(),
                        homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer
                    )
                })
            {
                return Err("combined HAP currently requires light-only enabled devices".into());
            }
        }
        if !config.devices().values().any(|d| d.enabled) {
            return Err("HAP requires an enabled light".into());
        }
        Ok(())
    }
    pub(crate) async fn identity(&self, directory: &Path) -> Result<IdentityStore, String> {
        let mut store = IdentityStore::open(directory.join("identity"))
            .await
            .map_err(|e| e.to_string())?;
        if store.has_config().await.map_err(|e| e.to_string())? {
            let saved = store.load_config().await.map_err(|e| e.to_string())?;
            if saved.host != self.address.ip() || saved.port != self.address.port() {
                return Err("stored HAP endpoint differs".into());
            }
        } else {
            let config = hap::Config {
                host: self.address.ip(),
                port: self.address.port(),
                name: "HomeSense".into(),
                category: hap::accessory::AccessoryCategory::Bridge,
                pin: crate::service::random_pin(),
                ..Default::default()
            };
            store
                .save_config(&config)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(store)
    }
}
pub(crate) struct Endpoint {
    pub projection: tokio::sync::watch::Sender<homesense_state::Snapshot>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Endpoint {
    pub async fn start(
        settings: &Settings,
        identity: &IdentityStore,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
        initial: homesense_state::Snapshot,
    ) -> Result<Self, String> {
        let bundle = admission
            .automation_bundle(
                controller,
                admission
                    .task_generation()
                    .await
                    .map_err(|e| format!("{e:?}"))?,
            )
            .await
            .map_err(|e| format!("{e:?}"))?;
        let document = bundle.automation.as_ref().ok_or("automation missing")?;
        settings.validate(&bundle.configuration, document)?;
        let lights = crate::runtime_hap::RuntimeHap::new(
            &bundle.configuration,
            initial.clone(),
            identity,
            admission.clone(),
            5000,
        )
        .await?;
        let authorization = identity.clone();
        let scenes = if settings.scenes.is_empty() {
            None
        } else {
            Some(
                crate::runtime_scene_hap::RuntimeSceneHap::new(
                    identity,
                    document,
                    settings
                        .scenes
                        .iter()
                        .map(|id| homesense_hap::scene_graph::SceneSpec {
                            id: id.clone(),
                            name: id.clone(),
                        })
                        .collect(),
                    runtime.control(),
                    move |id, _| authorization.controller_is_paired(id).unwrap_or(false),
                )
                .await?,
            )
        };
        let mut graph = crate::unified_hap::UnifiedHap { lights, scenes };
        let config = identity.load_config().await.map_err(|e| e.to_string())?;
        let server = if settings.address.ip().is_loopback() {
            IpServer::new_loopback(config, identity.clone()).await
        } else {
            IpServer::new(config, identity.clone()).await
        }
        .map_err(|e| e.to_string())?;
        graph.install(&server).await?;
        let (projection, updates) = tokio::sync::watch::channel(initial);
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (_stop, receive) = tokio::sync::watch::channel(false);
            tokio::select! {
                result=server.run_handle_ready(ready)=>result.map_err(|e|e.to_string()),
                result=graph.project(updates,receive)=>result.and(Err("HAP projection ended".into())),
            }
        });
        let mut endpoint = Self { projection, task };
        match tokio::time::timeout(std::time::Duration::from_secs(5), started).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                return Err(endpoint
                    .finished()
                    .await
                    .err()
                    .unwrap_or_else(|| "HAP failed before readiness".into()));
            }
            Err(_) => return Err("HAP startup deadline exceeded".into()),
        }
        Ok(endpoint)
    }
    pub async fn finished(&mut self) -> Result<(), String> {
        (&mut self.task).await.map_err(|e| e.to_string())?
    }
    pub async fn shutdown(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
