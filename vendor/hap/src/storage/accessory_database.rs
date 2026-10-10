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
        if let Err(status) = handler.write(controller, body) {
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

    /// Takes a pointer to an accessory and removes the accessory from the `AccessoryDatabase`.
    pub async fn remove_accessory(&mut self, accessory: &pointer::Accessory) -> Result<()> {
        let accessory = accessory.lock().await;
        let mut remove = None;

        for (i, a) in self.accessories.iter_mut().enumerate() {
            if a.lock().await.get_id() == accessory.get_id() {
                a.lock().await.set_event_emitter_on_characteristics(None);

                remove = Some(i);
                break;
            }
        }

        if let Some(i) = remove {
            self.accessories.remove(i);

            return Ok(());
        }

        Err(Error::AccessoryNotFound)
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
