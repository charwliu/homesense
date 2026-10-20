//! A management listener sharing its supervisor's runtime and bus owner.
use crate::{
    admission::Admission,
    automation_runtime::AutomationRuntime,
    web_api::{Api, Runtime},
};
use std::{
    path::Path,
    sync::{Arc, Mutex, RwLock},
};
use uuid::Uuid;
pub(crate) struct Endpoint {
    pub api: Arc<Api>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}
impl Endpoint {
    pub(crate) fn set_hap_placement(&self,placement:Option<crate::hap_placement::Publication>)->Result<(),String> {
        *self.api.hap_placement.write().map_err(|_|"HAP placement lock unavailable")?=placement;
        Ok(())
    }
    pub fn set_hap_bridges(&self,statuses:std::collections::BTreeMap<String,crate::hap_bridge_group::Status>)->Result<(),String> {
        let degraded=statuses.values().any(|s|*s==crate::hap_bridge_group::Status::Degraded);
        *self.api.hap_bridges.write().map_err(|_|"HAP bridge status lock unavailable")?=statuses;
        self.set_hap_status(if degraded {crate::web_api::HapStatus::Degraded}else{crate::web_api::HapStatus::Listening})
    }
    pub fn set_hap_status(&self,status:crate::web_api::HapStatus)->Result<(),String> {
        *self.api.hap_status.write().map_err(|_|"HAP status lock unavailable")?=status;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        settings: (crate::service::WebSettings,crate::web_receipt_policy::Policy),
        directory: &Path,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
        publisher: crate::automation_publication::Publisher,
        daily_backup: crate::daily_backup::Monitor,
    ) -> Result<Self, String> {
        Self::start_in_context(settings,directory,admission,controller,runtime,Some(publisher),homesense_core::ExecutionContext::Live,daily_backup).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn start_in_context(
        settings: (crate::service::WebSettings,crate::web_receipt_policy::Policy),
        directory: &Path,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
        publisher: Option<crate::automation_publication::Publisher>,
        context: homesense_core::ExecutionContext,
        daily_backup: crate::daily_backup::Monitor,
    ) -> Result<Self, String> {
        let (settings,receipt_policy)=settings;receipt_policy.validate()?;
        if let Some(tls)=&settings.tls {tls.validate_address(settings.address)?;}
        else if !settings.address.ip().is_loopback() || settings.address.port()==0 {return Err("plaintext management requires loopback".into());}
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
        let view = view(admission, runtime, &bundle)?;
        let tokens = Arc::new(crate::web_auth::Tokens::open_bootstrapped(
            &directory.join("web-credentials.sqlite"),
            &settings.secret,
            crate::web_auth::Grant {
                controller,
                role: if settings.administrator {
                    crate::web_auth::Role::Administrator
                } else {
                    crate::web_auth::Role::Member
                },
                scenes: crate::web_auth::bootstrap_scenes(
                    &bundle.configuration,
                    bundle.automation.as_ref().ok_or("automation missing")?,
                ),
                devices: bundle
                    .configuration
                    .devices()
                    .iter()
                    .filter(|(_, d)| d.enabled)
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect(),
            },
        )?);
        let origin = settings.tls.as_ref().map_or_else(
            || format!("http://{}", settings.address),
            |tls| tls.origin.clone(),
        );
        let api = Arc::new(Api {
            daily_backup,
            hap_placement:Default::default(),hap_bridges:Default::default(),hap_status: Default::default(),
            backup_source: Some(directory.join(match context {homesense_core::ExecutionContext::Simulation=>"automation-simulation.sqlite",homesense_core::ExecutionContext::Live=>"automation-tcp.sqlite"})),
            origin,
            tokens,
            runtime: RwLock::new(Some(view)),
            publisher,
            receipts: Mutex::new(crate::web_receipts::Receipts::open_with_policy(
                &directory.join("web-receipts.sqlite"),receipt_policy,
            )?),
            drafts: Some((
                context,
                Mutex::new(crate::web_drafts::Drafts::open(
                    &directory.join("web-drafts.sqlite"),
                )?),
            )),
        });
        let (stop, receive) = tokio::sync::watch::channel(false);
        let (ready, started) = tokio::sync::oneshot::channel();
        let owned = api.clone();
        let task = tokio::spawn(crate::web_server::serve_ready(
            owned,
            settings.address,
            receive,
            settings.tls,
            Some(ready),
        ));
        let mut endpoint = Self { api, stop, task };
        if started.await.is_err() {
            return Err(endpoint
                .finished()
                .await
                .err()
                .unwrap_or_else(|| "management failed before listening".into()));
        }
        Ok(endpoint)
    }
    /// Keep the listener, sessions and a read-only diagnostic view alive while
    /// transport resynchronizes. Every other runtime request remains refused.
    pub fn unavailable(&self,control:crate::automation_control::Control)->Result<(),String> {
        if let Some(view)=self.api.runtime.write().map_err(|_|"management lock poisoned")?.as_mut(){view.control=control;}
        self.set_hap_status(crate::web_api::HapStatus::Degraded)
    }
    /// Durable publication already committed, but its new physical mapping is
    /// still unavailable. Do not expose the old revision or open a control queue.
    pub async fn unavailable_mapping(&self,admission:&Admission,controller:Uuid,control:crate::automation_control::Control,configuration:&homesense_config::Configuration,rules:&[u8])->Result<(),String> {
        let generation=admission.diagnostic_generation(controller,configuration.revision()).await.map_err(|e|format!("{e:?}"))?;
        if generation.epochs.configuration!=configuration.revision(){return Err("unavailable mapping revision mismatch".into());}
        let document=crate::configuration_publish::validate_automation(configuration,rules,homesense_core::ExecutionContext::Live).map_err(|e|e.to_string())?;
        self.restore(Some(Runtime{control:control.unavailable(admission.clone(),generation),revision:configuration.revision(),boot:admission.boot_id().to_owned(),scenes:document.scene_catalog()}))?;
        self.set_hap_status(crate::web_api::HapStatus::Degraded)
    }
    pub fn pause(&self) -> Result<Option<Runtime>, String> {
        Ok(self
            .api
            .runtime
            .write()
            .map_err(|_| "management lock poisoned")?
            .take())
    }
    pub fn restore(&self, view: Option<Runtime>) -> Result<(), String> {
        *self
            .api
            .runtime
            .write()
            .map_err(|_| "management lock poisoned")? = view;
        Ok(())
    }
    pub async fn install(
        &self,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
    ) -> Result<(), String> {
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
        self.restore(Some(view(admission, runtime, &bundle)?))
    }
    pub async fn finished(&mut self) -> Result<(), String> {
        (&mut self.task).await.map_err(|e| e.to_string())?
    }
}
fn view(
    admission: &Admission,
    runtime: &AutomationRuntime,
    bundle: &homesense_storage::automation::Bundle,
) -> Result<Runtime, String> {
    let document = bundle
        .automation
        .as_ref()
        .ok_or("automation document missing")?;
    let scenes=document.scene_catalog();
    crate::web_api::check_catalog_capacity(scenes.len())?;
    Ok(Runtime {
        control: runtime.control(),
        revision: bundle.configuration.revision(),
        boot: admission.boot_id().to_owned(),
        scenes,
    })
}
#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[tokio::test]
    async fn complete_catalog_accepts130_and256_rejects257_without_executing_scenes(){
        use homesense_core::ExecutionContext;
        for count in [130,256,257]{
            let dir=std::env::temp_dir().join(format!("management-capacity-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();let path=dir.join("runtime.sqlite");
            let devices=br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"lamp","name":"Synthetic light","module_id":"relay","channel":1,"enabled":true}]}"#;
            let scenes=(0..count).map(|n|serde_json::json!({"id":format!("synthetic-{n}"),"enabled":true,"review_evidence":"synthetic catalog boundary","activate":[{"source":format!("synthetic/{n}"),"action":{"kind":"command","device":"lamp","command":{"kind":"switch","on":true}}}]})).collect::<Vec<_>>();
            let document=serde_json::to_vec(&serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":scenes})).unwrap();let mut store=homesense_storage::Store::open(&path).unwrap();let bundle=store.publish_automation(None,devices,&document,ExecutionContext::Simulation,&[]).unwrap();drop(store);
            let controller=Uuid::new_v4();let admission=Admission::open(path.clone(),crate::admission::Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();admission.synchronized(1,1).await.unwrap();let runtime=AutomationRuntime::load(admission.clone(),controller,1_600_000_000_000,1000,16,homesense_core::rule::Budget::default()).await.unwrap();let result=view(&admission,&runtime,&bundle);assert_eq!(result.is_ok(),count<=crate::web_api::MAX_MANAGED_SCENES);if let Ok(value)=result{assert_eq!(value.scenes.len(),count);}assert_eq!(rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);drop(runtime);admission.shutdown().await.unwrap();std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
