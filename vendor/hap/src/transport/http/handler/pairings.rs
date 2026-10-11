use rand::SeedableRng;
use futures::future::{BoxFuture, FutureExt};
use crate::transport::http::body::Body;
use log::info;
use std::{ops::Deref, str};
use uuid::Uuid;

use crate::{
    event::Event,
    pairing::{Pairing, Permissions},
    pointer,
    tlv::{self, Type, Value},
    transport::http::handler::TlvHandlerExt,
};

pub struct Pairings;

impl Pairings {
    pub fn new() -> Pairings {
        Pairings
    }
}

#[derive(Debug, Clone)]
enum StepNumber {
    Unknown = 0,
    Res = 2,
}

#[derive(Debug, Clone)]
enum HandlerNumber {
    Add = 3,
    Remove = 4,
    List = 5,
}

pub enum HandlerType {
    Add {
        pairing_id: Vec<u8>,
        ltpk: Vec<u8>,
        permissions: Permissions,
    },
    Remove {
        pairing_id: Vec<u8>,
    },
    List,
}

impl TlvHandlerExt for Pairings {
    type ParseResult = HandlerType;
    type Result = tlv::Container;

    fn parse(&self, body: Body) -> BoxFuture<Result<HandlerType, tlv::ErrorContainer>> {
        async {
            let mut decoded = super::read_tlv_body(body).await?;
            if decoded.get(&(Type::State as u8)) != Some(&vec![1]) {
                return Err(tlv::ErrorContainer::new(0, tlv::Error::Unknown));
            }
            match decoded.get(&(Type::Method as u8)) {
                Some(handler) if handler.len() == 1 => match handler[0] {
                    x if x == HandlerNumber::Add as u8 => {
                        let pairing_id = decoded.remove(&(Type::Identifier as u8)).ok_or(
                            tlv::ErrorContainer::new(StepNumber::Res as u8, tlv::Error::Unknown),
                        )?;
                        let ltpk = decoded.remove(&(Type::PublicKey as u8)).ok_or(
                            tlv::ErrorContainer::new(StepNumber::Res as u8, tlv::Error::Unknown),
                        )?;
                        let perms = decoded.remove(&(Type::Permissions as u8)).ok_or(
                            tlv::ErrorContainer::new(StepNumber::Res as u8, tlv::Error::Unknown),
                        )?;
                        if perms.len() != 1 || ltpk.len() != 32 {
                            return Err(tlv::ErrorContainer::new(2, tlv::Error::Unknown));
                        }
                        let permissions = Permissions::from_byte(perms[0]).map_err(|_| {
                            tlv::ErrorContainer::new(StepNumber::Res as u8, tlv::Error::Unknown)
                        })?;
                        Ok(HandlerType::Add {
                            pairing_id,
                            ltpk,
                            permissions,
                        })
                    }
                    x if x == HandlerNumber::Remove as u8 => {
                        let pairing_id = decoded.remove(&(Type::Identifier as u8)).ok_or(
                            tlv::ErrorContainer::new(StepNumber::Res as u8, tlv::Error::Unknown),
                        )?;
                        Ok(HandlerType::Remove { pairing_id })
                    }
                    x if x == HandlerNumber::List as u8 => Ok(HandlerType::List),
                    _ => Err(tlv::ErrorContainer::new(
                        StepNumber::Unknown as u8,
                        tlv::Error::Unknown,
                    )),
                },
                _ => Err(tlv::ErrorContainer::new(
                    StepNumber::Unknown as u8,
                    tlv::Error::Unknown,
                )),
            }
        }
        .boxed()
    }

    fn handle(
        &mut self,
        handler: HandlerType,
        controller_id: pointer::ControllerId,
        config: pointer::Config,
        storage: pointer::Storage,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<tlv::Container, tlv::ErrorContainer>> {
        async move {
            match handler {
                HandlerType::Add {
                    pairing_id,
                    ltpk,
                    permissions,
                } => match handle_add(
                    controller_id,
                    config,
                    storage,
                    event_emitter,
                    pairing_id,
                    ltpk,
                    permissions,
                )
                .await
                {
                    Ok(res) => Ok(res),
                    Err(err) => Err(tlv::ErrorContainer::new(StepNumber::Res as u8, err)),
                },
                HandlerType::Remove { pairing_id } => {
                    match handle_remove(controller_id, storage, event_emitter, pairing_id).await {
                        Ok(res) => Ok(res),
                        Err(err) => Err(tlv::ErrorContainer::new(StepNumber::Res as u8, err)),
                    }
                }
                HandlerType::List => match handle_list(controller_id, storage).await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(tlv::ErrorContainer::new(StepNumber::Res as u8, err)),
                },
            }
        }
        .boxed()
    }
}

async fn handle_add(
    controller_id: pointer::ControllerId,
    config: pointer::Config,
    storage: pointer::Storage,
    event_emitter: pointer::EventEmitter,
    pairing_id: Vec<u8>,
    ltpk: Vec<u8>,
    permissions: Permissions,
) -> Result<tlv::Container, tlv::Error> {
    info!("pairings M1: received add pairing request");

    let pairing_uuid = Uuid::parse_str(str::from_utf8(&pairing_id)?)?;
    let public_key = ed25519_dalek::VerifyingKey::try_from(ltpk.as_slice())?.to_bytes();
    let max_peers = config.lock().await.max_peers;
    let mut s = storage.lock().await;
    check_admin(&controller_id, s.as_ref()).await?;
    // Listing must succeed before absence can be inferred. A read failure must
    // never authorize replacing a controller's stored key.
    let pairings = s.list_pairings().await?;
    if let Some(existing) = pairings.iter().find(|pairing| pairing.id == pairing_uuid) {
        if existing.public_key != public_key {
            return Err(tlv::Error::Unknown);
        }
    } else if max_peers.is_some_and(|limit| pairings.len() >= limit) {
        return Err(tlv::Error::MaxPeers);
    }
    let pairing = Pairing::from_identifier(str::from_utf8(&pairing_id)?, permissions, public_key)?;
    let actor = (*controller_id
        .read()
        .map_err(|_| tlv::Error::Authentication)?)
    .ok_or(tlv::Error::Authentication)?;
    let revoked = s
        .save_pairing_authorized(&actor, &pairing, max_peers)
        .await?;
    info!("pairing mutation actor={} target={} revoked_count={} remaining={:?}", actor, pairing_uuid, revoked.len(), s.count_pairings().await.ok());
    drop(s);
    let mut emitter = event_emitter.lock().await;
    if revoked.is_empty() {
        emitter
            .emit(&Event::ControllerPaired { id: pairing.id })
            .await;
    } else {
        for id in revoked {
            emitter.emit(&Event::ControllerUnpaired { id }).await;
        }
    }

    info!("pairings M2: sending add pairing response");

    Ok(vec![Value::State(StepNumber::Res as u8)])
}

async fn handle_remove(
    controller_id: pointer::ControllerId,
    storage: pointer::Storage,
    event_emitter: pointer::EventEmitter,
    pairing_id: Vec<u8>,
) -> Result<tlv::Container, tlv::Error> {
    info!("pairings M1: received remove pairing request");

    let mut s = storage.lock().await;
    check_admin(&controller_id, s.as_ref()).await?;

    let uuid_str = str::from_utf8(&pairing_id)?;
    let pairing_uuid = Uuid::parse_str(uuid_str)?;
    let actor = (*controller_id
        .read()
        .map_err(|_| tlv::Error::Authentication)?)
    .ok_or(tlv::Error::Authentication)?;
    let revoked = s.remove_pairing_authorized(&actor, &pairing_uuid).await?;
    info!("pairing mutation actor={} target={} revoked_count={} remaining={:?}", actor, pairing_uuid, revoked.len(), s.count_pairings().await.ok());
    drop(s);
    let mut emitter = event_emitter.lock().await;
    for id in revoked {
        emitter.emit(&Event::ControllerUnpaired { id }).await;
    }

    info!("pairings M2: sending remove pairing response");

    Ok(vec![Value::State(StepNumber::Res as u8)])
}

async fn handle_list(
    controller_id: pointer::ControllerId,
    storage: pointer::Storage,
) -> Result<tlv::Container, tlv::Error> {
    info!("pairings M1: received list pairings request");

    let s = storage.lock().await;
    check_admin(&controller_id, s.as_ref()).await?;
    let pairings = s.list_pairings().await?;
    drop(s);
    let mut list = vec![Value::State(StepNumber::Res as u8)];
    for (i, pairing) in pairings.iter().enumerate() {
        list.push(Value::Identifier(pairing.wire_identifier()));
        list.push(Value::PublicKey(pairing.public_key.to_vec()));
        list.push(Value::Permissions(pairing.permissions.clone()));
        if i + 1 < pairings.len() {
            list.push(Value::Separator);
        }
    }

    info!("pairings M2: sending list pairings response");

    Ok(list)
}

async fn check_admin(
    controller_id: &pointer::ControllerId,
    storage: &dyn crate::storage::Storage,
) -> Result<(), tlv::Error> {
    let controller_id: Uuid = controller_id
        .read()
        .map_err(|_| tlv::Error::Authentication)?
        .deref()
        .ok_or(tlv::Error::Authentication)?;
    match storage.load_pairing(&controller_id).await {
        Err(_) => Err(tlv::Error::Authentication),
        Ok(controller) if controller.id == controller_id => match controller.permissions {
            Permissions::Admin => Ok(()),
            _ => Err(tlv::Error::Authentication),
        },
        Ok(_) => Err(tlv::Error::Authentication),
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;
    use crate::{
        event::EventEmitter,
        storage::{FileStorage, Storage},
    };
    use futures::lock::Mutex;
    use std::sync::{Arc, RwLock};

    #[tokio::test]
    async fn list_preserves_uppercase_identifiers_after_reopen() {
        let path = std::env::temp_dir().join(format!("hap-case-{}", Uuid::new_v4()));
        let mut store = FileStorage::new(path.as_os_str()).await.unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[19; 32]).verifying_key().to_bytes();
        let admin_id = "BC158B86-CABF-432D-AEE4-422EF0E3F1D5";
        let second_id = "AC158B86-CABF-432D-AEE4-422EF0E3F1D5";
        let admin = Pairing::from_identifier(admin_id, Permissions::Admin, key).unwrap();
        store.save_pairing(&admin).await.unwrap();
        store.save_pairing(&Pairing::from_identifier(second_id, Permissions::Admin, key).unwrap()).await.unwrap();
        drop(store);
        let reopened = FileStorage::new(path.as_os_str()).await.unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(reopened)));
        let controller = Arc::new(RwLock::new(Some(admin.id)));
        let listed = handle_list(controller, storage.clone()).await.unwrap();
        let identifiers: Vec<_> = listed.iter().filter_map(|v| match v {
            Value::Identifier(id) => Some(id.as_str()), _ => None,
        }).collect();
        assert_eq!(identifiers.len(), 2);
        assert!(identifiers.contains(&admin_id));
        assert!(identifiers.contains(&second_id));
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn permissions_key_replacement_and_corrupt_storage_fail_closed() {
        let path = std::env::temp_dir().join(format!("hap-management-{}", Uuid::new_v4()));
        let mut store = FileStorage::new(path.as_os_str()).await.unwrap();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()))
            .verifying_key()
            .to_bytes();
        let admin = Pairing::new(Uuid::new_v4(), Permissions::Admin, key);
        let user = Pairing::new(Uuid::new_v4(), Permissions::User, key);
        store.save_pairing(&admin).await.unwrap();
        store.save_pairing(&user).await.unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let controller = Arc::new(RwLock::new(Some(user.id)));
        let config = Arc::new(Mutex::new(crate::Config::default()));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        assert!(handle_list(controller.clone(), storage.clone())
            .await
            .is_err());
        assert!(handle_remove(
            controller.clone(),
            storage.clone(),
            emitter.clone(),
            admin.id.to_string().into_bytes()
        )
        .await
        .is_err());
        assert!(handle_add(
            controller.clone(),
            config.clone(),
            storage.clone(),
            emitter.clone(),
            user.id.to_string().into_bytes(),
            key.to_vec(),
            Permissions::Admin
        )
        .await
        .is_err());
        *controller.write().unwrap() = Some(admin.id);
        let listing = handle_list(controller.clone(), storage.clone())
            .await
            .unwrap();
        assert_eq!(
            listing
                .iter()
                .filter(|v| matches!(v, Value::Separator))
                .count(),
            1
        );
        assert!(!matches!(listing.last(), Some(Value::Separator)));
        let other_key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()))
            .verifying_key()
            .to_bytes();
        assert!(handle_add(
            controller.clone(),
            config.clone(),
            storage.clone(),
            emitter.clone(),
            user.id.to_string().into_bytes(),
            other_key.to_vec(),
            Permissions::Admin
        )
        .await
        .is_err());
        assert_eq!(
            storage.lock().await.load_pairing(&user.id).await.unwrap(),
            user
        );
        let file = path.join(format!("pairings/{}.json", user.id));
        std::fs::write(&file, b"corrupt").unwrap();
        assert!(handle_add(
            controller.clone(),
            config,
            storage.clone(),
            emitter,
            user.id.to_string().into_bytes(),
            key.to_vec(),
            Permissions::Admin
        )
        .await
        .is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"corrupt");
        storage
            .lock()
            .await
            .delete_pairing(&admin.id)
            .await
            .unwrap();
        assert!(handle_list(controller, storage.clone()).await.is_err());
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
}
