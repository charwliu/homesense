use crate::transport::http::body::Body;
use futures::future::{BoxFuture, FutureExt};
use hyper::{Response, StatusCode, Uri};
use log::error;
use std::collections::HashMap;
use url::form_urlencoded;

use crate::{
    pointer,
    transport::http::{
        handler::JsonHandlerExt, json_response, status_response, CharacteristicResponseBody,
        ReadResponseObject, Status, WriteObject, WriteResponseObject,
    },
    Error, Result,
};

pub struct GetCharacteristics;

impl GetCharacteristics {
    pub fn new() -> Self {
        GetCharacteristics
    }
}

impl JsonHandlerExt for GetCharacteristics {
    fn handle(
        &mut self,
        uri: Uri,
        _: Body,
        controller: pointer::ControllerId,
        _: pointer::EventSubscriptions,
        _: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        _: pointer::EventEmitter,
    ) -> BoxFuture<'_, Result<Response<Body>>> {
        async move {
            super::paired_controller(&controller, &storage).await?;
            if let Some(query) = uri.query() {
                let mut resp_body = CharacteristicResponseBody::<ReadResponseObject> {
                    characteristics: Vec::new(),
                };
                let mut some_err = false;

                let mut queries: HashMap<String, String> = HashMap::new();
                for (key, val) in form_urlencoded::parse(query.as_bytes()) {
                    queries.insert(key.into(), val.into());
                }
                let (f_meta, f_perms, f_type, f_ev) = check_flags(&queries);
                let q_id = queries
                    .get("id")
                    .ok_or(Error::HttpStatus(StatusCode::BAD_REQUEST))?;
                let ids = q_id.split(',').collect::<Vec<&str>>();
                for id in ids {
                    let id_pair = id.split('.').collect::<Vec<&str>>();
                    if id_pair.len() != 2 {
                        return Err(Error::HttpStatus(StatusCode::BAD_REQUEST));
                    }
                    let aid = id_pair[0].parse::<u64>()?;
                    let iid = id_pair[1].parse::<u64>()?;

                    let res_object = match accessory_database
                        .lock()
                        .await
                        .read_characteristic(aid, iid, f_meta, f_perms, f_type, f_ev)
                        .await
                    {
                        Ok(mut res_object) => {
                            if res_object.status != Some(0) {
                                some_err = true;
                                res_object.value = None;
                            }
                            res_object
                        }
                        Err(e) => {
                            error!("error reading characteristic: {:?}", e);
                            some_err = true;
                            ReadResponseObject {
                                iid,
                                aid,
                                status: Some(Status::ServiceCommunicationFailure as i32),
                                ..Default::default()
                            }
                        }
                    };

                    resp_body.characteristics.push(res_object);
                }

                if some_err {
                    let res = serde_json::to_vec(&resp_body)?;
                    return json_response(res, StatusCode::MULTI_STATUS);
                }
                for ref mut r in &mut resp_body.characteristics {
                    r.status = None;
                }
                let res = serde_json::to_vec(&resp_body)?;

                json_response(res, StatusCode::OK)
            } else {
                status_response(StatusCode::BAD_REQUEST)
            }
        }
        .boxed()
    }
}

fn check_flags(flags: &HashMap<String, String>) -> (bool, bool, bool, bool) {
    let true_val = "1".to_string();
    (
        flags.get("meta") == Some(&true_val),
        flags.get("perms") == Some(&true_val),
        flags.get("type") == Some(&true_val),
        flags.get("ev") == Some(&true_val),
    )
}

pub struct UpdateCharacteristics;

impl UpdateCharacteristics {
    pub fn new() -> Self {
        UpdateCharacteristics {}
    }
}

impl JsonHandlerExt for UpdateCharacteristics {
    fn handle(
        &mut self,
        _: Uri,
        body: Body,
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        _: pointer::Config,
        storage: pointer::Storage,
        accessories: pointer::AccessoryDatabase,
        _: pointer::EventEmitter,
    ) -> BoxFuture<'_, Result<Response<Body>>> {
        async move {
            let controller = *controller_id
                .read()
                .map_err(|_| Error::HttpStatus(StatusCode::UNAUTHORIZED))?;
            let controller = controller.ok_or(Error::HttpStatus(StatusCode::UNAUTHORIZED))?;
            let aggregated_body = read_write_body(body, std::time::Duration::from_secs(5)).await?;
            // Check persisted pairing after body receipt; a stale session alone
            // must not authorize a newly received write. Dispatch must recheck too.
            let paired = storage
                .lock()
                .await
                .load_pairing(&controller)
                .await
                .map_err(|_| Error::HttpStatus(StatusCode::UNAUTHORIZED))?;
            if paired.id != controller {
                return Err(Error::HttpStatus(StatusCode::UNAUTHORIZED));
            }

            let write_body: CharacteristicResponseBody<WriteObject> =
                serde_json::from_slice(&aggregated_body)
                    .map_err(|_| Error::HttpStatus(StatusCode::BAD_REQUEST))?;
            if write_body.characteristics.is_empty() || write_body.characteristics.len() > 128 {
                return Err(Error::HttpStatus(StatusCode::BAD_REQUEST));
            }
            if let Some(result) = accessories
                .lock()
                .await
                .write_batch(
                    controller,
                    &aggregated_body,
                    &write_body.characteristics,
                    &event_subscriptions,
                )
                .await
            {
                return match result {
                    Ok(()) => status_response(StatusCode::NO_CONTENT),
                    Err(status) => {
                        let response = CharacteristicResponseBody {
                            characteristics: write_body
                                .characteristics
                                .iter()
                                .map(|write| WriteResponseObject {
                                    aid: write.aid,
                                    iid: write.iid,
                                    status: status as i32,
                                })
                                .collect::<Vec<_>>(),
                        };
                        json_response(serde_json::to_vec(&response)?, StatusCode::MULTI_STATUS)
                    }
                };
            }
            let mut resp_body = CharacteristicResponseBody::<WriteResponseObject> {
                characteristics: Vec::new(),
            };
            let mut some_err = false;
            let mut all_err = true;

            for c in write_body.characteristics {
                let iid = c.iid;
                let aid = c.aid;
                let res_object = match accessories
                    .lock()
                    .await
                    .write_characteristic(c, &event_subscriptions)
                    .await
                {
                    Ok(res_object) => {
                        if res_object.status != 0 {
                            some_err = true;
                        } else {
                            all_err = false;
                        }
                        res_object
                    }
                    Err(e) => {
                        error!("error updating characteristic: {:?}", e);
                        some_err = true;
                        WriteResponseObject {
                            iid,
                            aid,
                            status: Status::ServiceCommunicationFailure as i32,
                        }
                    }
                };

                resp_body.characteristics.push(res_object);
            }

            if all_err {
                let res = serde_json::to_vec(&resp_body)?;
                json_response(res, StatusCode::BAD_REQUEST)
            } else if some_err {
                let res = serde_json::to_vec(&resp_body)?;
                json_response(res, StatusCode::MULTI_STATUS)
            } else {
                status_response(StatusCode::NO_CONTENT)
            }
        }
        .boxed()
    }
}

// Bound bytes while reading, including chunked bodies without Content-Length.
async fn read_write_body(mut body: Body, timeout: std::time::Duration) -> Result<Vec<u8>> {
    tokio::time::timeout(timeout, async move {
        let mut bytes = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk?;
            if chunk.len() > 64 * 1024 - bytes.len() {
                return Err(Error::HttpStatus(StatusCode::PAYLOAD_TOO_LARGE));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| Error::HttpStatus(StatusCode::REQUEST_TIMEOUT))?
}

#[cfg(test)]
mod write_body_tests {
    use super::*;
    use crate::characteristic::{CharacteristicCallbacks, HapCharacteristic};
    use std::time::Duration;
    #[tokio::test]
    async fn write_handler_rechecks_pairing_before_accessory_processing() {
        use crate::{
            event::EventEmitter,
            pairing::{Pairing, Permissions},
            storage::{accessory_database::AccessoryDatabase, FileStorage, Storage},
        };
        use futures::lock::Mutex;
        use std::sync::{Arc, RwLock};
        let id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("hap-write-auth-{}", id));
        let mut store = FileStorage::new(dir.as_os_str()).await.unwrap();
        store
            .save_pairing(&Pairing::new(id, Permissions::User, [1; 32]))
            .await
            .unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        let accessories = Arc::new(Mutex::new(AccessoryDatabase::new(emitter.clone())));
        let config = Arc::new(Mutex::new(crate::Config::default()));
        let controller = Arc::new(RwLock::new(None));
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let mut handler = UpdateCharacteristics::new();
        // Unauthenticated and removed-pairing sessions fail before parsing or
        // touching characteristics. A paired session reaches JSON validation.
        for (session, remove, expected) in [
            (None, false, StatusCode::UNAUTHORIZED),
            (Some(id), false, StatusCode::BAD_REQUEST),
            (Some(id), true, StatusCode::UNAUTHORIZED),
        ] {
            *controller.write().unwrap() = session;
            if remove {
                storage.lock().await.delete_pairing(&id).await.unwrap();
            }
            let result = handler
                .handle(
                    Uri::from_static("/characteristics"),
                    Body::from("invalid json"),
                    controller.clone(),
                    subscriptions.clone(),
                    config.clone(),
                    storage.clone(),
                    accessories.clone(),
                    emitter.clone(),
                )
                .await;
            assert!(matches!(result, Err(Error::HttpStatus(code)) if code == expected));
            assert!(subscriptions.lock().await.is_empty());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn grouped_http_write_is_once_and_rejection_preserves_subscriptions_and_cache() {
        use crate::{
            accessory::{lightbulb::LightbulbAccessory, AccessoryInformation},
            event::EventEmitter,
            pairing::{Pairing, Permissions},
            server::WriteBatchHandler,
            storage::{accessory_database::AccessoryDatabase, FileStorage, Storage},
        };
        use futures::lock::Mutex;
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, RwLock,
        };
        struct Adapter {
            id: uuid::Uuid,
            calls: AtomicUsize,
            reject: AtomicBool,
        }
        impl WriteBatchHandler for Adapter {
            fn write(
                &self,
                controller: uuid::Uuid,
                body: &[u8],
            ) -> std::result::Result<(), Status> {
                assert_eq!(controller, self.id);
                let parsed: serde_json::Value = serde_json::from_slice(body).unwrap();
                assert_eq!(parsed["characteristics"].as_array().unwrap().len(), 2);
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.reject.load(Ordering::SeqCst) {
                    Err(Status::ResourceBusy)
                } else {
                    Ok(())
                }
            }
        }
        let id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("hap-group-{}", id));
        let mut store = FileStorage::new(dir.as_os_str()).await.unwrap();
        store
            .save_pairing(&Pairing::new(id, Permissions::User, [1; 32]))
            .await
            .unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        let mut db = AccessoryDatabase::new(emitter.clone());
        let mut light = LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap();
        let on = light.lightbulb.power_state.get_id();
        let brightness = light.lightbulb.brightness.as_ref().unwrap().get_id();
        light
            .lightbulb
            .power_state
            .on_update(Some(|_: &bool, _: &bool| {
                panic!("individual callback must not run")
            }));
        db.add_accessory(Box::new(light)).unwrap();
        let adapter = Arc::new(Adapter {
            id,
            calls: AtomicUsize::new(0),
            reject: AtomicBool::new(true),
        });
        db.set_write_batch_handler(adapter.clone());
        let accessories = Arc::new(Mutex::new(db));
        let config = Arc::new(Mutex::new(crate::Config::default()));
        let controller = Arc::new(RwLock::new(Some(id)));
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let mut handler = UpdateCharacteristics::new();
        for (reject, status) in [
            (true, StatusCode::MULTI_STATUS),
            (false, StatusCode::NO_CONTENT),
        ] {
            adapter.reject.store(reject, Ordering::SeqCst);
            let body = serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":2,"iid":on,"value":true,"ev":true},{"aid":2,"iid":brightness,"value":73}]})).unwrap();
            let response = handler
                .handle(
                    Uri::from_static("/characteristics"),
                    Body::from(body),
                    controller.clone(),
                    subscriptions.clone(),
                    config.clone(),
                    storage.clone(),
                    accessories.clone(),
                    emitter.clone(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            if reject {
                let value: serde_json::Value = serde_json::from_slice(
                    &crate::transport::http::body::to_bytes(response.into_body()).await.unwrap(),
                )
                .unwrap();
                assert!(value["characteristics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|v| v["status"] == -70403));
                assert!(subscriptions.lock().await.is_empty());
            } else {
                assert_eq!(*subscriptions.lock().await, vec![(2, on)]);
            }
            let observed = accessories
                .lock()
                .await
                .read_characteristic(2, on, false, false, false, false)
                .await
                .unwrap();
            assert_eq!(observed.value, Some(serde_json::json!(false)));
        }
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
        // Another session unsubscribing must not turn off the shared emitter.
        let other_subscriptions = Arc::new(Mutex::new(vec![(2, on)]));
        let body = serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":2,"iid":on,"ev":false},{"aid":2,"iid":brightness,"ev":false}]})).unwrap();
        let response = handler
            .handle(
                Uri::from_static("/characteristics"),
                Body::from(body),
                controller.clone(),
                other_subscriptions.clone(),
                config.clone(),
                storage.clone(),
                accessories.clone(),
                emitter.clone(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(other_subscriptions.lock().await.is_empty());
        assert_eq!(*subscriptions.lock().await, vec![(2, on)]);
        let state = accessories
            .lock()
            .await
            .read_characteristic(2, on, false, false, false, true)
            .await
            .unwrap();
        assert_eq!(state.ev, Some(true));
        // Unknown members reject the whole request before the adapter is called.
        let body = serde_json::to_vec(&serde_json::json!({"characteristics":[{"aid":2,"iid":on,"ev":false},{"aid":999,"iid":1,"value":true}]})).unwrap();
        let response = handler
            .handle(
                Uri::from_static("/characteristics"),
                Body::from(body),
                controller,
                subscriptions.clone(),
                config,
                storage,
                accessories,
                emitter,
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        assert_eq!(*subscriptions.lock().await, vec![(2, on)]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn accepts_limit_and_rejects_one_extra_byte() {
        assert_eq!(
            read_write_body(Body::from(vec![0; 65536]), Duration::from_secs(1))
                .await
                .unwrap()
                .len(),
            65536
        );
        assert!(matches!(
            read_write_body(Body::from(vec![0; 65537]), Duration::from_secs(1)).await,
            Err(Error::HttpStatus(StatusCode::PAYLOAD_TOO_LARGE))
        ));
    }
    #[tokio::test]
    async fn chunked_body_is_counted_across_chunks() {
        let (mut sender, body) = Body::channel();
        let task = tokio::spawn(async move {
            sender.send_data(vec![0; 40000].into()).await.unwrap();
            sender.send_data(vec![0; 30000].into()).await.unwrap();
        });
        assert!(matches!(
            read_write_body(body, Duration::from_secs(1)).await,
            Err(Error::HttpStatus(StatusCode::PAYLOAD_TOO_LARGE))
        ));
        task.await.unwrap();
    }
    #[tokio::test]
    async fn stalled_body_times_out() {
        let (_sender, body) = Body::channel();
        assert!(matches!(
            read_write_body(body, Duration::from_millis(10)).await,
            Err(Error::HttpStatus(StatusCode::REQUEST_TIMEOUT))
        ));
    }
}
