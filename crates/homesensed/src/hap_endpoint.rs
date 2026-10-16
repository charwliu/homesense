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
    pub motors: Vec<homesense_core::DeviceId>,
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
            if !document.scene_action_available(id) {
                return Err("published HAP scene missing, disabled or unreviewed".into());
            }
        }
        let unique: std::collections::BTreeSet<_> = self.motors.iter().collect();
        if unique.len()!=self.motors.len() || self.motors.len()>64 {
            return Err("invalid HAP motor selection".into());
        }
        for id in &self.motors {
            config.motor_command(id,homesense_core::MotorAction::Stop).map_err(|e|e.to_string())?;
            let name=&config.devices()[id].name;
            if name.trim().is_empty() || name.len()>58 || name.chars().any(char::is_control) {
                return Err("invalid HAP motor name".into());
            }
        }
        // Non-light devices remain in automation but are not implicitly exposed
        // as light characteristics. Their own publication policy is separate.
        if !config.devices().keys().any(|id|config.binding(id).is_some_and(|(_,module)|matches!(module.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer))) {
            return Err("HAP requires an enabled light".into());
        }
        Ok(())
    }
    pub(crate) async fn identity(&self, directory: &Path) -> Result<IdentityStore, String> {
        self.identity_with_name(directory,"HomeSense",false).await
    }
    pub(crate) async fn identity_named(&self,directory:&Path,name:&str)->Result<IdentityStore,String> {
        self.identity_with_name(directory,name,true).await
    }
    async fn identity_with_name(&self,directory:&Path,name:&str,rename:bool)->Result<IdentityStore,String> {
        if name.trim().is_empty() || name.len()>64 || name.chars().any(char::is_control) {return Err("invalid HAP bridge name".into());}
        let mut store = IdentityStore::open(directory.join("identity"))
            .await
            .map_err(|e| e.to_string())?;
        if store.has_config().await.map_err(|e| e.to_string())? {
            let mut saved = store.load_config().await.map_err(|e| e.to_string())?;
            if saved.host != self.address.ip() || saved.port != self.address.port() {
                return Err("stored HAP endpoint differs".into());
            }
            if rename && saved.name!=name {saved.name=name.to_owned();store.save_config(&saved).await.map_err(|e|e.to_string())?;}
        } else {
            let config = hap::Config {
                host: self.address.ip(),
                port: self.address.port(),
                name: name.into(),
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
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub panel_feed: Option<crate::runtime_panel_hap::Feed>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(task)=&self.task {task.abort();}
    }
}
impl Endpoint {
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        settings: &Settings,
        identity: &IdentityStore,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
        initial: homesense_state::Snapshot,
        allow_listener_failure: bool,
    ) -> Result<Option<Self>, String> {
        Self::start_selected(settings,identity,admission,controller,runtime,initial,allow_listener_failure,None).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn start_selected(
        settings:&Settings,identity:&IdentityStore,admission:&Admission,
        controller:Uuid,runtime:&AutomationRuntime,initial:homesense_state::Snapshot,
        allow_listener_failure:bool,
        lights:Option<std::collections::BTreeSet<homesense_core::DeviceId>>,
    )->Result<Option<Self>,String> {
        Self::start_selected_panels(settings,identity,admission,controller,runtime,initial,allow_listener_failure,lights,&[]).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn start_selected_panels(
        settings:&Settings,identity:&IdentityStore,admission:&Admission,
        controller:Uuid,runtime:&AutomationRuntime,initial:homesense_state::Snapshot,
        allow_listener_failure:bool,
        lights:Option<std::collections::BTreeSet<homesense_core::DeviceId>>,
        selected_panels:&[homesense_core::DeviceId],
    )->Result<Option<Self>,String> {
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
        let lights = crate::runtime_hap::RuntimeHap::new_selected(
            &bundle.configuration,
            initial.clone(),
            identity,
            admission.clone(),
            bundle.configuration.refresh_policy().observation_max_age_ms(),
            lights,
        )
        .await?;
        let authorization = identity.clone();
        let scenes = if settings.scenes.is_empty() {
            None
        } else {
            Some(
                crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(
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
                    admission.clone(),controller,
                    move |id, _| authorization.controller_is_paired(id).unwrap_or(false),
                )
                .await?,
            )
        };
        let motors = if settings.motors.is_empty() {None} else {
            let authorization=identity.clone();
            Some(crate::runtime_motor_hap::RuntimeMotorHap::new(
                &bundle.configuration,identity,admission.clone(),
                settings.motors.iter().map(|id|homesense_hap::motor_graph::MotorSpec{device:id.clone(),name:bundle.configuration.devices()[id].name.clone()}).collect(),
                move |id,_|authorization.controller_is_paired(id).unwrap_or(false),
            ).await?)
        };
        let published=lights.lights.iter().map(|(binding,_)|binding.device.clone()).collect();
        let away=crate::runtime_away_hap::RuntimeAwayHap::new(&bundle.configuration,document,identity,admission,runtime.control(),&published).await?;
        let mut graph = crate::unified_hap::UnifiedHap { lights, scenes, motors, away };
        let config = identity.load_config().await.map_err(|e| e.to_string())?;
        let server = if settings.address.ip().is_loopback() {
            IpServer::new_loopback(config, identity.clone()).await
        } else {
            IpServer::new(config, identity.clone()).await
        }
        .map_err(|e| e.to_string())?;
        let panels = if selected_panels.is_empty() { None } else {
            Some(crate::runtime_panel_hap::RuntimePanelHap::new(&bundle.configuration,document,identity,admission.clone(),selected_panels,initial.cursor).await?)
        };
        graph.install_with_panels(&server,panels.as_ref()).await?;
        let panel_feed = panels.as_ref().map(|panels|panels.feed.clone());
        let (projection, updates) = tokio::sync::watch::channel(initial);
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (_stop, receive) = tokio::sync::watch::channel(false);
            tokio::select! {
                result=server.run_handle_ready(ready)=>result.map_err(|e|e.to_string()),
                result=graph.project(updates,receive)=>result.and(Err("HAP projection ended".into())),
                result=async { match panels {Some(panels)=>panels.run().await,None=>std::future::pending().await} }=>result.and(Err("HAP panel delivery ended".into())),
            }
        });
        let mut endpoint = Self { projection, task:Some(task), panel_feed };
        let failure=match tokio::time::timeout(std::time::Duration::from_secs(5), started).await {
            Ok(Ok(())) => return Ok(Some(endpoint)),
            Ok(Err(_)) => endpoint.finished().await.err().unwrap_or_else(||"HAP failed before readiness".into()),
            Err(_) => "HAP startup deadline exceeded".into(),
        };
        endpoint.shutdown().await;
        if allow_listener_failure {
            log::error!("HAP channel degraded during listener startup: {failure}");
            Ok(None)
        } else {Err(failure)}
    }
    #[cfg(test)]
    pub(crate) fn abort_handle(&self)->tokio::task::AbortHandle {
        self.task.as_ref().expect("active endpoint").abort_handle()
    }
    #[cfg(test)]
    pub(crate) fn synthetic(initial: homesense_state::Snapshot) -> Self {
        let (projection, _) = tokio::sync::watch::channel(initial);
        Self { projection, task: Some(tokio::spawn(std::future::pending())), panel_feed:None }
    }
    pub async fn finished(&mut self) -> Result<(), String> {
        let task=self.task.as_mut().ok_or("HAP endpoint result already consumed")?;
        let result=task.await.map_err(|e| e.to_string());
        self.task=None;
        result?
    }
    pub async fn shutdown(mut self) {
        if let Some(task)=self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn consumed_task_result_can_be_cleaned_up_without_polling_again() {
        for failure in [false,true] {
            let (projection,_)=tokio::sync::watch::channel(homesense_state::Snapshot{cursor:homesense_state::Cursor{boot_id:1,sequence:0},state_version:0,devices:Default::default()});
            let mut endpoint=Endpoint{projection,panel_feed:None,task:Some(tokio::spawn(async move {
                if failure {Err("synthetic endpoint failure".into())}else{Ok(())}
            }))};
            assert_eq!(endpoint.finished().await.is_err(),failure);
            assert!(endpoint.finished().await.unwrap_err().contains("already consumed"));
            endpoint.shutdown().await;
        }
    }
    #[tokio::test]
    async fn cancelled_wait_keeps_task_owned_until_shutdown() {
        let (projection,_)=tokio::sync::watch::channel(homesense_state::Snapshot{cursor:homesense_state::Cursor{boot_id:1,sequence:0},state_version:0,devices:Default::default()});
        let mut endpoint=Endpoint{projection,panel_feed:None,task:Some(tokio::spawn(std::future::pending()))};
        let handle=endpoint.task.as_ref().unwrap().abort_handle();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(1),endpoint.finished()).await.is_err());
        assert!(endpoint.task.is_some());
        assert!(!handle.is_finished());
        endpoint.shutdown().await;
        assert!(handle.is_finished());
    }
    #[test]
    fn motor_selection_requires_unique_enabled_calibrated_devices() {
        let mut raw:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        raw["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
        raw["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"motor","name":"Curtain","gateway":"simulation","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}));
        let parse=|raw:&serde_json::Value|homesense_config::Configuration::parse(&serde_json::to_vec(raw).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let config=parse(&raw);
        let document=homesense_config::rule_document::Document::parse(br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&config).unwrap();
        let id=homesense_core::DeviceId::new("motor").unwrap();
        let mut settings=Settings{address:"127.0.0.1:51826".parse().unwrap(),scenes:vec![],motors:vec![id.clone()]};
        assert!(settings.validate(&config,&document).is_ok());
        settings.motors.push(id.clone());assert!(settings.validate(&config,&document).is_err());
        settings.motors=vec![homesense_core::DeviceId::new("missing").unwrap()];assert!(settings.validate(&config,&document).is_err());
        settings.motors=vec![homesense_core::DeviceId::new("simulation:light").unwrap()];assert!(settings.validate(&config,&document).is_err());
        settings.motors=vec![id];
        let mut invalid=raw.clone();invalid["devices"][1]["enabled"]=false.into();assert!(settings.validate(&parse(&invalid),&document).is_err());
        let mut invalid=raw.clone();invalid["devices"][1].as_object_mut().unwrap().remove("motor_calibration");assert!(settings.validate(&parse(&invalid),&document).is_err());
        raw["devices"][1]["name"]="x".repeat(59).into();assert!(settings.validate(&parse(&raw),&document).is_err());
    }
}

#[cfg(test)]
#[path = "hap_subset_endpoint_tests.rs"]
mod subset_tests;
