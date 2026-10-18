//! Authenticated HAP Away action with bridge-local scope and revocation.
//! Construction allocates identifiers only; it sends no control command.
use crate::{
    admission::Admission,
    automation_control::{Action, Control, Reply},
    away_auth::{Authorization, HapLease},
    module_all_off::{self, LightingReview},
};
use hap::{
    characteristic::CharacteristicCallbacks,
    server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::DeviceId;
use homesense_hap::{
    scene_graph::SceneBinding, scene_requests::SceneRequests, storage::IdentityStore,
};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
struct Handler {
    requests: SceneRequests,
    identity: IdentityStore,
    devices: BTreeSet<String>,
    active: Arc<AtomicBool>,
    revision: u64,
    control: Control,
    resets: tokio::sync::mpsc::Sender<()>,
    writes: tokio::sync::Mutex<()>,
}

impl WriteBatchHandler for Handler {
    fn write(&self, _: uuid::Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        Err(WriteStatus::ServiceCommunicationFailure)
    }
    fn write_async<'a>(
        &'a self,
        controller: uuid::Uuid,
        body: &'a [u8],
    ) -> hap::futures::future::BoxFuture<'a, Result<(), WriteStatus>> {
        Box::pin(async move {
            let request = self
                .requests
                .normalize(controller, body)
                .map_err(|_| WriteStatus::InvalidValueInRequest)?;
            let _guard = self.writes.lock().await;
            let lease = Authorization::Hap(
                HapLease::new(
                    self.identity.clone(),
                    controller,
                    self.devices.clone(),
                    self.active.clone(),
                )
                .ok_or(WriteStatus::InsufficientPrivileges)?,
            );
            if !lease.valid() {
                return Err(WriteStatus::InsufficientPrivileges);
            }
            if !matches!(
                self.control
                    .request(Action::AwayOff {
                        controller,
                        revision: self.revision,
                        lease: lease.clone(),
                        submit: false
                    })
                    .await,
                Ok(Reply::CommandsAccepted(_))
            ) {
                return Err(WriteStatus::InsufficientPrivileges);
            }
            if !request.starts.is_empty() {
                let reset = self
                    .resets
                    .try_reserve()
                    .map_err(|_| WriteStatus::ResourceBusy)?;
                match self
                    .control
                    .request(Action::AwayOff {
                        controller,
                        revision: self.revision,
                        lease,
                        submit: true,
                    })
                    .await
                {
                    Ok(Reply::CommandsAccepted(_)|Reply::AwayAccepted(_)) => reset.send(()),
                    _ => return Err(WriteStatus::ServiceCommunicationFailure),
                }
            }
            Ok(())
        })
    }
}
pub struct RuntimeAwayHap {
    pub binding: SceneBinding,
    pub accessory: AccessoryHandle,
    pub write_handler: Arc<dyn WriteBatchHandler>,
    resets: tokio::sync::mpsc::Receiver<()>,
    active: Arc<AtomicBool>,
}
impl Drop for RuntimeAwayHap {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}
impl RuntimeAwayHap {
    /// Only a bridge covering every effect can expose this operation. Hidden
    /// supplies are included only when all their logical owners are published.
    pub async fn new(
        config: &Configuration,
        document: &Document,
        identity: &IdentityStore,
        admission: &Admission,
        control: Control,
        published: &BTreeSet<DeviceId>,
    ) -> Result<Option<Self>, String> {
        let Some(policy) = document
            .away_off
            .as_ref()
            .filter(|p| p.enabled && p.hap_expose)
        else {
            return Ok(None);
        };
        let logical_scope = published.iter().map(|id| id.as_str().to_owned()).collect();
        let allowed = crate::away_off::authorized_devices(config, &logical_scope)?;
        let reviews = policy
            .modules
            .iter()
            .map(|m| {
                LightingReview::new(
                    m.module_id.clone(),
                    m.devices
                        .iter()
                        .map(|id| DeviceId::new(id.clone()))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?,
                    m.review_evidence.clone(),
                )
                .map_err(|e| format!("HAP Away review: {e:?}"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        match module_all_off::plan(config, &reviews, &allowed, admission.context()) {
            Ok(_) => {}
            Err(module_all_off::Error::Unauthorized) => return Ok(None),
            Err(error) => return Err(format!("HAP Away scope: {error:?}")),
        }
        let (binding, mut accessory) = homesense_hap::away_graph::build(identity)
            .await
            .map_err(|e| e.to_string())?;
        let requests = SceneRequests::new(vec![binding.clone()]).map_err(|e| e.to_string())?;
        accessory
            .switch
            .power_state
            .on_read(Some(|| Ok(Some(false))));
        accessory.switch.power_state.on_update(Some(
            |_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                Err(Box::new(std::io::Error::other(
                    "grouped Away action required",
                )))
            },
        ));
        let accessory: AccessoryHandle =
            Arc::new(hap::futures::lock::Mutex::new(Box::new(accessory)));
        let (resets, receive) = tokio::sync::mpsc::channel(32);
        let active = Arc::new(AtomicBool::new(true));
        let handler = Handler {
            requests,
            identity: identity.clone(),
            devices: logical_scope,
            active: active.clone(),
            revision: config.revision(),
            control,
            resets,
            writes: tokio::sync::Mutex::new(()),
        };
        Ok(Some(Self {
            binding,
            accessory,
            write_handler: Arc::new(handler),
            resets: receive,
            active,
        }))
    }
    /// An action acceptance pulse; no lamp state is manufactured here.
    pub async fn flush_resets(&mut self) -> Result<usize, String> {
        let mut count = 0;
        while count < 32 && self.resets.try_recv().is_ok() {
            let mut accessory = self.accessory.lock().await;
            let characteristic = accessory
                .get_mut_services()
                .into_iter()
                .flat_map(|s| s.get_mut_characteristics())
                .find(|c| c.get_id() == self.binding.on_iid)
                .ok_or("Away reset binding missing")?;
            characteristic
                .set_observed_value(serde_json::json!(false))
                .await
                .map_err(|e| e.to_string())?;
            count += 1;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hap::{
        pairing::{Pairing, Permissions},
        storage::Storage,
    };
    use homesense_core::ExecutionContext;
    #[tokio::test]
    async fn partial_bridge_is_ineligible_and_retirement_cancels_accepted_hap_work() {
        let root = std::env::temp_dir().join(format!("away-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let devices=(1..=8).map(|channel|serde_json::json!({"id":format!("lamp:{channel}"),"name":"Synthetic light","module_id":"relay","channel":channel,"enabled":true})).collect::<Vec<_>>();
        let config = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"27","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":devices});
        let ids = (1..=8).map(|n| format!("lamp:{n}")).collect::<Vec<_>>();
        let mut rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"hap_expose":true,"review_evidence":"Synthetic operation","active_execution_policy":"cancel_affected_lighting","modules":[{"module_id":"relay","devices":ids,"review_evidence":"All eight synthetic ports are lighting"}]}});
        let bytes = serde_json::to_vec(&config).unwrap();
        let config = Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
        let document = Document::parse(&serde_json::to_vec(&rules).unwrap(), &config).unwrap();
        let db = root.join("runtime.sqlite");
        homesense_storage::Store::open(&db)
            .unwrap()
            .publish_automation(
                None,
                &bytes,
                &serde_json::to_vec(&rules).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let admission = Admission::open(
            db.clone(),
            crate::admission::Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let (control, _requests) = crate::automation_control::channel(
            uuid::Uuid::new_v4(),
            admission.task_generation().await.unwrap(),
        );
        let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
        identity.save_config(&hap::Config::default()).await.unwrap();
        let full = config.devices().keys().cloned().collect::<BTreeSet<_>>();
        let mut partial = full.clone();
        partial.remove(&DeviceId::new("lamp:8").unwrap());
        assert!(
            RuntimeAwayHap::new(
                &config,
                &document,
                &identity,
                &admission,
                control.clone(),
                &partial
            )
            .await
            .unwrap()
            .is_none()
        );
        rules["away_off"]["hap_expose"] = false.into();
        let disabled = Document::parse(&serde_json::to_vec(&rules).unwrap(), &config).unwrap();
        assert!(
            RuntimeAwayHap::new(
                &config,
                &disabled,
                &identity,
                &admission,
                control.clone(),
                &full
            )
            .await
            .unwrap()
            .is_none()
        );
        let graph = RuntimeAwayHap::new(&config, &document, &identity, &admission, control, &full)
            .await
            .unwrap()
            .unwrap();
        let controller = uuid::Uuid::new_v4();
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [11; 32]))
            .await
            .unwrap();
        let lease = Authorization::Hap(
            HapLease::new(
                identity.clone(),
                controller,
                full.iter().map(|id| id.as_str().into()).collect(),
                graph.active.clone(),
            )
            .unwrap(),
        );
        let commands = admission.submit_away_off(1, lease, true).await.unwrap();
        assert_eq!(commands.len(), 1);
        identity.delete_pairing(&controller).await.unwrap();
        assert!(admission.dispatch().await.unwrap().is_none());
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [12; 32]))
            .await
            .unwrap();
        let lease = Authorization::Hap(
            HapLease::new(
                identity.clone(),
                controller,
                full.iter().map(|id| id.as_str().into()).collect(),
                graph.active.clone(),
            )
            .unwrap(),
        );
        assert_eq!(
            admission
                .submit_away_off(1, lease, true)
                .await
                .unwrap()
                .len(),
            1
        );
        let handler = graph.write_handler.clone();
        let body=serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":graph.binding.aid,"iid":graph.binding.on_iid,"value":true}]})).unwrap();
        drop(graph);
        assert_eq!(
            handler.write_async(controller, &body).await,
            Err(WriteStatus::InsufficientPrivileges)
        );
        assert!(admission.dispatch().await.unwrap().is_none());
        let sql = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            sql.query_row(
                "SELECT count(*) FROM command_journal WHERE status='cancelled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            16
        );
        admission.shutdown().await.unwrap();
        drop((sql, identity, handler));
        std::fs::remove_dir_all(root).unwrap();
    }
}
