use crate::transport::http::body::Body;
use futures::future::{BoxFuture, FutureExt};
use hyper::{Response, StatusCode, Uri};
use log::info;

use crate::{
    pointer,
    transport::http::{
        handler::{paired_controller, JsonHandlerExt},
        json_response, Status,
    },
    Result,
};

pub struct Accessories;

impl Accessories {
    pub fn new() -> Accessories {
        Accessories
    }
}

impl JsonHandlerExt for Accessories {
    fn handle(
        &mut self,
        _: Uri,
        _: Body,
        controller: pointer::ControllerId,
        _: pointer::EventSubscriptions,
        _: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        _: pointer::EventEmitter,
    ) -> BoxFuture<'_, Result<Response<Body>>> {
        info!("received list accessories request");
        async move {
            paired_controller(&controller, &storage).await?;
            let resp_body = match accessory_database.lock().await.as_serialized_json().await {
                Ok(body) => body,
                Err(_) => return json_response(serde_json::to_vec(&serde_json::json!({"status": Status::ServiceCommunicationFailure as i32}))?, StatusCode::SERVICE_UNAVAILABLE),
            };
            // let resp_body = serde_json::to_vec(&accessory_database)?;
            json_response(resp_body, StatusCode::OK)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accessory::{lightbulb::LightbulbAccessory, AccessoryInformation},
        characteristic::{CharacteristicCallbacks, HapCharacteristic},
        event::EventEmitter,
        pairing::{Pairing, Permissions},
        storage::{accessory_database::AccessoryDatabase, FileStorage, Storage},
        Error,
    };
    use futures::lock::Mutex;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, RwLock,
    };
    #[tokio::test]
    async fn discovery_keeps_graph_without_cached_fallback_or_control_side_effects() {
        let id = uuid::Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("hap-discovery-{id}"));
        let mut store = FileStorage::new(path.as_os_str()).await.unwrap();
        store
            .save_pairing(&Pairing::new(id, Permissions::User, [4; 32]))
            .await
            .unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        let mut db = AccessoryDatabase::new(emitter.clone());
        let unavailable = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let mut light = LightbulbAccessory::new(2, AccessoryInformation::default()).unwrap();
        let iid = light.lightbulb.brightness.as_ref().unwrap().get_id();
        let flag = unavailable.clone();
        let count = reads.clone();
        light
            .lightbulb
            .brightness
            .as_mut()
            .unwrap()
            .on_read(Some(move || {
                count.fetch_add(1, Ordering::SeqCst);
                if flag.load(Ordering::SeqCst) {
                    Err(Box::new(Error::Storage) as Box<dyn std::error::Error + Send + Sync>)
                } else {
                    Ok(Some(73))
                }
            }));
        let count = writes.clone();
        light
            .lightbulb
            .brightness
            .as_mut()
            .unwrap()
            .on_update(Some(move |_: &i32, _: &i32| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }));
        db.add_accessory(Box::new(light)).unwrap();
        let db = Arc::new(Mutex::new(db));
        let config = Arc::new(Mutex::new(crate::Config::default()));
        let controller = Arc::new(RwLock::new(None));
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let mut handler = Accessories::new();
        for (authenticated, fail, status) in [
            (false, false, StatusCode::UNAUTHORIZED),
            (true, false, StatusCode::OK),
            (true, true, StatusCode::OK),
        ] {
            *controller.write().unwrap() = authenticated.then_some(id);
            unavailable.store(fail, Ordering::SeqCst);
            let response = handler
                .handle(
                    Uri::from_static("/accessories"),
                    Body::empty(),
                    controller.clone(),
                    subscriptions.clone(),
                    config.clone(),
                    storage.clone(),
                    db.clone(),
                    emitter.clone(),
                )
                .await;
            if !authenticated {
                assert!(matches!(
                    response,
                    Err(Error::HttpStatus(StatusCode::UNAUTHORIZED))
                ));
                assert_eq!(reads.load(Ordering::SeqCst), 0);
                continue;
            }
            let response = response.unwrap();
            assert_eq!(response.status(), status);
            let json: serde_json::Value = serde_json::from_slice(
                &crate::transport::http::body::to_bytes(response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            let characteristic = json["accessories"][0]["services"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|s| s["characteristics"].as_array().unwrap())
                    .find(|c| c["iid"] == iid)
                    .unwrap();
            assert_eq!(characteristic["value"], if fail { serde_json::Value::Null } else { serde_json::json!(73) });
        }
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let mut reader = super::super::characteristics::GetCharacteristics::new();
        let response = reader.handle(
            format!("/characteristics?id=2.{iid}").parse().unwrap(), Body::empty(),
            controller.clone(), subscriptions.clone(), config.clone(), storage.clone(), db.clone(), emitter.clone(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let value: serde_json::Value = serde_json::from_slice(&crate::transport::http::body::to_bytes(response.into_body()).await.unwrap()).unwrap();
        assert_eq!(value["characteristics"][0]["status"], -70402);
        assert!(value["characteristics"][0].get("value").is_none());
        let response = reader
            .handle(
                "/characteristics?id=999.1".parse().unwrap(),
                Body::empty(),
                controller.clone(),
                subscriptions.clone(),
                config.clone(),
                storage.clone(),
                db.clone(),
                emitter.clone(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let json: serde_json::Value = serde_json::from_slice(
            &crate::transport::http::body::to_bytes(response.into_body())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["characteristics"][0]["status"], -70409);
        storage.lock().await.delete_pairing(&id).await.unwrap();
        assert!(matches!(
            handler
                .handle(
                    Uri::from_static("/accessories"),
                    Body::empty(),
                    controller.clone(),
                    subscriptions.clone(),
                    config.clone(),
                    storage.clone(),
                    db.clone(),
                    emitter.clone()
                )
                .await,
            Err(Error::HttpStatus(StatusCode::UNAUTHORIZED))
        ));
        assert!(matches!(
            reader
                .handle(
                    "/characteristics?id=2.1".parse().unwrap(),
                    Body::empty(),
                    controller,
                    subscriptions,
                    config,
                    storage,
                    db,
                    emitter
                )
                .await,
            Err(Error::HttpStatus(StatusCode::UNAUTHORIZED))
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(path).unwrap();
    }
}
