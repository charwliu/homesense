use std::sync::Arc;

use futures::lock::Mutex;
use serde_json::json;

use crate::{
    accessory::HapAccessory,
    characteristic::Perm,
    pointer,
    transport::http::{ReadResponseObject, Status, WriteObject, WriteResponseObject},
    Error, Result,
};

/// `AccessoryDatabase` is a wrapper type holding a list of accessories.
pub struct AccessoryDatabase {
    pub accessories: Vec<pointer::Accessory>,
    event_emitter: pointer::EventEmitter,
    write_batch_handler: Option<Arc<dyn crate::server::WriteBatchHandler>>,
}

impl AccessoryDatabase {
    /// Creates a new `AccessoryDatabase`.
    pub fn new(event_emitter: pointer::EventEmitter) -> AccessoryDatabase {
        AccessoryDatabase {
            accessories: Vec::new(),
            event_emitter,
            write_batch_handler: None,
        }
    }

    pub fn set_write_batch_handler(&mut self, handler: Arc<dyn crate::server::WriteBatchHandler>) {
        self.write_batch_handler = Some(handler);
    }

    /// None selects the upstream individual-write path. Once installed, errors
    /// never fall back to it. Lock mappings and subscriptions before acceptance,
    /// so no await/cancellation point can split acceptance from subscription commit.
    pub(crate) async fn write_batch(
        &self,
        controller: uuid::Uuid,
        body: &[u8],
        writes: &[WriteObject],
        subscriptions: &pointer::EventSubscriptions,
    ) -> Option<std::result::Result<(), Status>> {
        let handler = self.write_batch_handler.as_ref()?;
        let mut guards = Vec::with_capacity(self.accessories.len());
        for accessory in &self.accessories {
            guards.push(accessory.lock().await);
        }
        let mut seen = std::collections::BTreeSet::new();
        for write in writes {
            if !seen.insert((write.aid, write.iid))
                || (write.value.is_none() && write.ev.is_none())
                || write.auth_data.is_some()
            {
                return Some(Err(Status::InvalidValueInRequest));
            }
            let mut found = false;
            for accessory in &mut guards {
                if accessory.get_id() != write.aid {
                    continue;
                }
                for service in accessory.get_mut_services() {
                    for characteristic in service.get_mut_characteristics() {
                        if characteristic.get_id() != write.iid {
                            continue;
                        }
                        found = true;
                        let perms = characteristic.get_perms();
                        if write.value.is_some() && !perms.contains(&Perm::PairedWrite) {
                            return Some(Err(Status::ReadOnlyCharacteristic));
                        }
                        if write.ev.is_some() && !perms.contains(&Perm::Events) {
                            return Some(Err(Status::NotificationNotSupported));
                        }
                    }
                }
            }
            if !found {
                return Some(Err(Status::ResourceDoesNotExist));
            }
        }
        let mut subscriptions = subscriptions.lock().await;
        if let Err(status) = handler.write_async(controller, body).await {
            return Some(Err(status));
        }
        for write in writes {
            if let Some(enabled) = write.ev {
                let key = (write.aid, write.iid);
                if enabled {
                    if !subscriptions.contains(&key) {
                        subscriptions.push(key);
                    }
                    for accessory in &mut guards {
                        if accessory.get_id() != write.aid {
                            continue;
                        }
                        for service in accessory.get_mut_services() {
                            for characteristic in service.get_mut_characteristics() {
                                if characteristic.get_id() == write.iid {
                                    characteristic.set_event_notifications(Some(true));
                                }
                            }
                        }
                    }
                } else {
                    subscriptions.retain(|item| *item != key);
                    // Do not disable the shared emitter: other sessions may subscribe.
                }
            }
        }
        Some(Ok(()))
    }

    /// Adds an accessory to the `AccessoryDatabase` and returns a pointer to the added accessory.
    pub fn add_accessory(
        &mut self,
        accessory: Box<dyn HapAccessory>,
    ) -> Result<pointer::Accessory> {
        let mut accessory = accessory;
        accessory.set_event_emitter_on_characteristics(Some(self.event_emitter.clone()));

        let accessory = Arc::new(Mutex::new(accessory));
        self.accessories.push(accessory.clone());
        // TODO: some error handling here?

        Ok(accessory)
    }

    /// Register an existing graph handle without replacing callbacks or identity.
    /// Intended for startup before requests are served.
    pub async fn add_accessory_handle(&mut self, accessory: pointer::Accessory) -> Result<()> {
        if self
            .accessories
            .iter()
            .any(|stored| Arc::ptr_eq(stored, &accessory))
        {
            return Err(Error::DuplicateAccessory);
        }
        let mut candidate = accessory.lock().await;
        let aid = candidate.get_id();
        for stored in &self.accessories {
            if stored.lock().await.get_id() == aid {
                return Err(Error::DuplicateAccessory);
            }
        }
        candidate.set_event_emitter_on_characteristics(Some(self.event_emitter.clone()));
        drop(candidate);
        self.accessories.push(accessory);
        Ok(())
    }

    /// Validate the entire startup graph before any database mutation.
    pub(crate) async fn startup_aids(accessories: &[pointer::Accessory]) -> Result<Vec<u64>> {
        if accessories.is_empty() || accessories.len() > 4096 {
            return Err(Error::Storage);
        }
        let mut aids = std::collections::BTreeSet::new();
        for accessory in accessories {
            let accessory = accessory.lock().await;
            if accessory.get_id() == 0 || !aids.insert(accessory.get_id()) {
                return Err(Error::DuplicateAccessory);
            }
            let mut iids = std::collections::BTreeSet::new();
            for service in accessory.get_services() {
                if service.get_id() == 0 || !iids.insert(service.get_id()) {
                    return Err(Error::DuplicateAccessory);
                }
                for characteristic in service.get_characteristics() {
                    if characteristic.get_id() == 0 || !iids.insert(characteristic.get_id()) {
                        return Err(Error::DuplicateAccessory);
                    }
                }
            }
        }
        Ok(aids.into_iter().collect())
    }
    /// Canonical structural schema: volatile values/subscriptions are excluded.
    pub(crate) fn graph_manifest(mut accessories: Vec<serde_json::Value>) -> Result<Vec<u8>> {
        let static_types = [
            crate::HapType::Name,
            crate::HapType::ConfiguredName,
            crate::HapType::Manufacturer,
            crate::HapType::Model,
            crate::HapType::SerialNumber,
            crate::HapType::FirmwareRevision,
            crate::HapType::HardwareRevision,
            crate::HapType::SoftwareRevision,
        ]
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
        for accessory in &mut accessories {
            let services = accessory
                .get_mut("services")
                .and_then(|v| v.as_array_mut())
                .ok_or(Error::Storage)?;
            for service in services.iter_mut() {
                let characteristics = service
                    .get_mut("characteristics")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(Error::Storage)?;
                for characteristic in characteristics.iter_mut() {
                    let object = characteristic.as_object_mut().ok_or(Error::Storage)?;
                    if !object
                        .get("type")
                        .is_some_and(|value| static_types.contains(value))
                    {
                        object.remove("value");
                    }
                    object.remove("ev");
                    if let Some(perms) = object.get_mut("perms").and_then(|v| v.as_array_mut()) {
                        perms.sort_by_key(|v| v.to_string());
                    }
                }
                characteristics.sort_by_key(|v| v.get("iid").and_then(|v| v.as_u64()));
            }
            services.sort_by_key(|v| v.get("iid").and_then(|v| v.as_u64()));
        }
        accessories.sort_by_key(|v| v.get("aid").and_then(|v| v.as_u64()));
        let bytes = serde_json::to_vec(&json!({"version":2,"accessories":accessories}))?;
        // Keep existing small manifests byte-compatible. Large installations
        // persist a domain/version-tagged digest, not an oversized SQLite entry.
        // The digest covers exactly the same canonical structural document.
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(Error::Storage);
        }
        if bytes.len() > 65536 {
            use sha2::{Digest, Sha256};
            return Ok(serde_json::to_vec(&json!({
                "version":3,"canonical_version":2,"algorithm":"sha256",
                "digest":Sha256::digest(&bytes).to_vec()
            }))?);
        }
        Ok(bytes)
    }
    pub(crate) fn event_emitter(&self) -> pointer::EventEmitter {
        self.event_emitter.clone()
    }

    /// Takes a pointer to an accessory and removes the accessory from the `AccessoryDatabase`.
    pub async fn remove_accessory(&mut self, accessory: &pointer::Accessory) -> Result<()> {
        // Match the registered handle, not merely its AID. Locking the supplied
        // handle and then the database entry self-deadlocks when they are equal.
        let index = self
            .accessories
            .iter()
            .position(|stored| Arc::ptr_eq(stored, accessory))
            .ok_or(Error::AccessoryNotFound)?;
        self.accessories[index]
            .lock()
            .await
            .set_event_emitter_on_characteristics(None);
        self.accessories.remove(index);
        Ok(())
    }

    /// Reads the value of a characteristic.
    pub(crate) async fn read_characteristic(
        &self,
        aid: u64,
        iid: u64,
        meta: bool,
        perms: bool,
        hap_type: bool,
        ev: bool,
    ) -> Result<ReadResponseObject> {
        let mut result_object = ReadResponseObject {
            iid,
            aid,
            hap_type: None,
            format: None,
            perms: None,
            ev: None,
            value: None,
            unit: None,
            max_value: None,
            min_value: None,
            step_value: None,
            max_len: None,
            status: Some(Status::ResourceDoesNotExist as i32),
        };

        'l: for accessory in self.accessories.iter() {
            let mut a = accessory.lock().await;
            if a.get_id() == aid {
                for service in a.get_mut_services() {
                    for characteristic in service.get_mut_characteristics() {
                        if characteristic.get_id() == iid {
                            let characteristic_perms = characteristic.get_perms();
                            if characteristic_perms.contains(&Perm::PairedRead) {
                                result_object.status = Some(0);
                                result_object.value = Some(characteristic.get_value().await?);
                                if meta {
                                    result_object.format = Some(characteristic.get_format());
                                    result_object.unit = characteristic.get_unit();
                                    result_object.max_value = characteristic.get_max_value();
                                    result_object.min_value = characteristic.get_min_value();
                                    result_object.step_value = characteristic.get_step_value();
                                    result_object.max_len = characteristic.get_max_len();
                                }
                                if perms {
                                    result_object.perms = Some(characteristic_perms);
                                }
                                if hap_type {
                                    result_object.hap_type = Some(characteristic.get_type());
                                }
                                if ev {
                                    result_object.ev = characteristic.get_event_notifications();
                                }
                            } else {
                                result_object.status = Some(Status::WriteOnlyCharacteristic as i32);
                            }
                            break 'l;
                        }
                    }
                }
            }
        }

        Ok(result_object)
    }

    /// Writes the value of a characteristic.
    pub(crate) async fn write_characteristic(
        &mut self,
        write_object: WriteObject,
        event_subscriptions: &pointer::EventSubscriptions,
    ) -> Result<WriteResponseObject> {
        let mut result_object = WriteResponseObject {
            aid: write_object.aid,
            iid: write_object.iid,
            status: 0,
        };

        'l: for accessory in self.accessories.iter_mut() {
            let mut a = accessory.lock().await;
            if a.get_id() == write_object.aid {
                for service in a.get_mut_services() {
                    for characteristic in service.get_mut_characteristics() {
                        if characteristic.get_id() == write_object.iid {
                            let characteristic_perms = characteristic.get_perms();
                            if let Some(ev) = write_object.ev {
                                if characteristic_perms.contains(&Perm::Events) {
                                    characteristic.set_event_notifications(Some(ev));
                                    let subscription = (write_object.aid, write_object.iid);
                                    let mut es = event_subscriptions.lock().await;
                                    let pos = es.iter().position(|&s| s == subscription);
                                    match (ev, pos) {
                                        (true, None) => {
                                            es.push(subscription);
                                        }
                                        (false, Some(p)) => {
                                            es.remove(p);
                                        }
                                        _ => {}
                                    }
                                } else {
                                    result_object.status = Status::NotificationNotSupported as i32;
                                }
                            }
                            if let Some(value) = write_object.value {
                                if characteristic_perms.contains(&Perm::PairedWrite) {
                                    characteristic.set_value(value).await?;
                                } else {
                                    result_object.status = Status::ReadOnlyCharacteristic as i32;
                                }
                            }
                            break 'l;
                        }
                    }
                }
            }
        }

        Ok(result_object)
    }

    pub(crate) async fn as_serialized_json(&self) -> Result<Vec<u8>> {
        let mut accessory_values = Vec::new();
        for accessory in &self.accessories {
            let mut a = accessory.lock().await;
            let mut value = serde_json::to_value(&*a)?;
            let services = value
                .get_mut("services")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or(Error::Storage)?;
            for service in a.get_mut_services() {
                let serialized = services
                    .iter_mut()
                    .find(|v| v["iid"].as_u64() == Some(service.get_id()))
                    .ok_or(Error::Storage)?;
                let characteristics = serialized
                    .get_mut("characteristics")
                    .and_then(serde_json::Value::as_array_mut)
                    .ok_or(Error::Storage)?;
                for characteristic in service.get_mut_characteristics() {
                    let object = characteristics
                        .iter_mut()
                        .find(|v| v["iid"].as_u64() == Some(characteristic.get_id()))
                        .and_then(serde_json::Value::as_object_mut)
                        .ok_or(Error::Storage)?;
                    // Metadata serialization contains a cached value. Replace it
                    // with a snapshot read without invoking writes or notifications.
                    object.remove("value");
                    if characteristic.get_perms().contains(&Perm::PairedRead) {
                        object.insert("value".into(), characteristic.get_value().await?);
                    }
                }
            }
            accessory_values.push(value);
        }

        let json = json!({ "accessories": accessory_values });

        Ok(serde_json::to_vec(&json)?)
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_json_serialization() {} // TODO: test it
// }

#[cfg(test)]
mod async_acceptance_tests {
    use super::*;
    use crate::{
        accessory::{lightbulb::LightbulbAccessory, AccessoryInformation},
        characteristic::HapCharacteristic,
        event::EventEmitter,
        server::{WriteBatchHandler, WriteStatus},
    };
    use futures::{channel::oneshot, future::BoxFuture, poll};
    use std::task::Poll;
    #[tokio::test]
    async fn removal_releases_exact_registered_handle_without_self_deadlock() {
        let mut db = AccessoryDatabase::new(Arc::new(Mutex::new(EventEmitter::new())));
        let handle = db
            .add_accessory(Box::new(
                LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap(),
            ))
            .unwrap();
        let impostor: pointer::Accessory = Arc::new(Mutex::new(Box::new(
            LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap(),
        )));
        assert!(db.remove_accessory(&impostor).await.is_err());
        assert_eq!(db.accessories.len(), 1);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            db.remove_accessory(&handle),
        )
        .await
        .expect("remove deadlocked")
        .unwrap();
        assert!(db.accessories.is_empty());
        assert!(db.remove_accessory(&handle).await.is_err());
    }
    #[tokio::test]
    async fn shared_handle_registration_preserves_identity_and_rejects_duplicate_aid() {
        let mut db = AccessoryDatabase::new(Arc::new(Mutex::new(EventEmitter::new())));
        let handle: pointer::Accessory = Arc::new(Mutex::new(Box::new(
            LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap(),
        )));
        db.add_accessory_handle(handle.clone()).await.unwrap();
        assert!(Arc::ptr_eq(&handle, &db.accessories[0]));
        assert!(db.add_accessory_handle(handle.clone()).await.is_err());
        let other: pointer::Accessory = Arc::new(Mutex::new(Box::new(
            LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap(),
        )));
        assert!(db.add_accessory_handle(other).await.is_err());
        assert_eq!(db.accessories.len(), 1);
        db.remove_accessory(&handle).await.unwrap();
    }
    #[tokio::test]
    async fn startup_graph_checks_all_aids_and_iids_before_registration() {
        let a: pointer::Accessory = Arc::new(Mutex::new(Box::new(
            LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap(),
        )));
        let b: pointer::Accessory = Arc::new(Mutex::new(Box::new(
            LightbulbAccessory::new(1, AccessoryInformation::default()).unwrap(),
        )));
        assert_eq!(
            AccessoryDatabase::startup_aids(&[a.clone(), b.clone()])
                .await
                .unwrap(),
            vec![1, 2]
        );
        assert!(AccessoryDatabase::startup_aids(&[a.clone(), a.clone()])
            .await
            .is_err());
        b.lock().await.set_id(2);
        assert!(AccessoryDatabase::startup_aids(&[a.clone(), b])
            .await
            .is_err());
        a.lock().await.get_mut_services()[0].set_id(0);
        assert!(AccessoryDatabase::startup_aids(&[a]).await.is_err());
    }
    #[test]
    fn graph_manifest_ignores_values_but_tracks_characteristic_structure() {
        let graph = json!({"aid":1,"services":[{"iid":1,"type":"43","characteristics":[{"iid":2,"type":"25","perms":["pr","pw"],"value":false,"ev":false}]}]});
        let before = AccessoryDatabase::graph_manifest(vec![graph.clone()]).unwrap();
        let mut changed = graph;
        changed["services"][0]["characteristics"][0]["value"] = json!(true);
        changed["services"][0]["characteristics"][0]["ev"] = json!(true);
        assert_eq!(
            before,
            AccessoryDatabase::graph_manifest(vec![changed.clone()]).unwrap()
        );
        changed["services"][0]["characteristics"][0]["type"] = json!("8");
        assert_ne!(
            before,
            AccessoryDatabase::graph_manifest(vec![changed]).unwrap()
        );
    }
    #[test]
    fn large_manifest_digest_preserves_structure_and_size_boundary() {
        let mut graph=json!({"aid":1,"services":[{"iid":1,"type":"43","characteristics":[
            {"iid":2,"type":"23","value":"n".repeat(70_000),"perms":["pr"]},
            {"iid":3,"type":"25","value":false,"perms":["pr","pw"],"ev":false}
        ]}]});
        let before=AccessoryDatabase::graph_manifest(vec![graph.clone()]).unwrap();
        assert!(before.len()<65536);
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&before).unwrap()["version"],3);
        graph["services"][0]["characteristics"][1]["value"]=json!(true);
        graph["services"][0]["characteristics"][1]["ev"]=json!(true);
        graph["services"][0]["characteristics"][1]["perms"]=json!(["pw","pr"]);
        assert_eq!(before,AccessoryDatabase::graph_manifest(vec![graph.clone()]).unwrap());
        graph["services"][0]["characteristics"][0]["value"]=json!("m".repeat(70_000));
        assert_ne!(before,AccessoryDatabase::graph_manifest(vec![graph.clone()]).unwrap());
        graph["services"][0]["characteristics"][0]["value"]=json!("n".repeat(4*1024*1024));
        assert!(AccessoryDatabase::graph_manifest(vec![graph]).is_err());
    }
    #[tokio::test]
    async fn real_accessory_manifest_tracks_name_but_not_power() {
        let mut light = LightbulbAccessory::new(
            2,
            AccessoryInformation {
                name: "Study".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let before =
            AccessoryDatabase::graph_manifest(vec![serde_json::to_value(&light).unwrap()]).unwrap();
        light
            .lightbulb
            .power_state
            .set_observed_value(json!(true))
            .await
            .unwrap();
        let powered =
            AccessoryDatabase::graph_manifest(vec![serde_json::to_value(&light).unwrap()]).unwrap();
        assert_eq!(before, powered);
        let renamed = LightbulbAccessory::new(
            2,
            AccessoryInformation {
                name: "Study light".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_ne!(
            before,
            AccessoryDatabase::graph_manifest(vec![serde_json::to_value(&renamed).unwrap()])
                .unwrap()
        );
    }
    struct Gate(std::sync::Mutex<Option<oneshot::Receiver<std::result::Result<(), WriteStatus>>>>);
    impl WriteBatchHandler for Gate {
        fn write(&self, _: uuid::Uuid, _: &[u8]) -> std::result::Result<(), WriteStatus> {
            panic!("synchronous fallback invoked")
        }
        fn write_async<'a>(
            &'a self,
            _: uuid::Uuid,
            _: &'a [u8],
        ) -> BoxFuture<'a, std::result::Result<(), WriteStatus>> {
            let receiver = self.0.lock().unwrap().take().unwrap();
            Box::pin(async move { receiver.await.unwrap() })
        }
    }
    #[tokio::test]
    async fn subscriptions_commit_only_after_async_acceptance() {
        for result in [Ok(()), Err(Status::ResourceBusy)] {
            let emitter = Arc::new(Mutex::new(EventEmitter::new()));
            let mut db = AccessoryDatabase::new(emitter);
            let light = LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap();
            let iid = light.lightbulb.power_state.get_id();
            db.accessories.push(Arc::new(Mutex::new(Box::new(light))));
            let (sender, receiver) = oneshot::channel();
            db.set_write_batch_handler(Arc::new(Gate(std::sync::Mutex::new(Some(receiver)))));
            let subscriptions = Arc::new(Mutex::new(Vec::new()));
            let writes = vec![WriteObject {
                aid: 2,
                iid,
                ev: Some(true),
                value: Some(json!(true)),
                auth_data: None,
                _remote: None,
            }];
            let mut pending =
                Box::pin(db.write_batch(uuid::Uuid::new_v4(), b"request", &writes, &subscriptions));
            assert!(matches!(poll!(pending.as_mut()), Poll::Pending));
            sender.send(result).unwrap();
            assert_eq!(pending.await, Some(result));
            assert_eq!(
                subscriptions.lock().await.len(),
                usize::from(result.is_ok())
            );
            let mut light = db.accessories[0].lock().await;
            assert_eq!(
                light
                    .get_mut_service(crate::HapType::Lightbulb)
                    .unwrap()
                    .get_mut_characteristic(crate::HapType::PowerState)
                    .unwrap()
                    .get_value()
                    .await
                    .unwrap(),
                json!(false)
            );
        }
    }
}
