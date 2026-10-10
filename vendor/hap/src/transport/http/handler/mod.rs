use futures::future::{BoxFuture, FutureExt};
use hyper::{self, Body, Response, StatusCode, Uri};

use crate::{
    pointer,
    tlv::{self, Encodable},
    transport::http::{status_response, tlv_response},
    Error, Result,
};

pub mod accessories;
pub mod characteristics;
pub mod identify;
pub mod pair_setup;
pub mod pair_verify;
pub mod pairings;

pub trait HandlerExt {
    fn handle(
        &mut self,
        uri: Uri,
        body: Body,
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<Response<Body>>>;
}

pub trait TlvHandlerExt {
    type ParseResult: Send;
    type Result: Encodable;

    fn parse(
        &self,
        body: Body,
    ) -> BoxFuture<std::result::Result<Self::ParseResult, tlv::ErrorContainer>>;
    fn handle(
        &mut self,
        step: Self::ParseResult,
        controller_id: pointer::ControllerId,
        config: pointer::Config,
        storage: pointer::Storage,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<std::result::Result<Self::Result, tlv::ErrorContainer>>;
}

#[derive(Debug)]
pub struct TlvHandler<T: TlvHandlerExt + Send + Sync>(T);

impl<T: TlvHandlerExt + Send + Sync> From<T> for TlvHandler<T> {
    fn from(inst: T) -> TlvHandler<T> {
        TlvHandler(inst)
    }
}

impl<T: TlvHandlerExt + Send + Sync> HandlerExt for TlvHandler<T> {
    fn handle(
        &mut self,
        _: Uri,
        body: Body,
        controller_id: pointer::ControllerId,
        _: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        _: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<Response<Body>>> {
        async move {
            let response = match self.0.parse(body).await {
                Err(e) => e.encode(),
                Ok(step) => match self
                    .0
                    .handle(step, controller_id, config, storage, event_emitter)
                    .await
                {
                    Err(e) => e.encode(),
                    Ok(res) => res.encode(),
                },
            };
            tlv_response(response, StatusCode::OK)
        }
        .boxed()
    }
}

pub trait JsonHandlerExt {
    fn handle(
        &mut self,
        uri: Uri,
        body: Body,
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<Response<Body>>>;
}

#[derive(Debug)]
pub struct JsonHandler<T: JsonHandlerExt + Send + Sync>(T);

impl<T: JsonHandlerExt + Send + Sync> From<T> for JsonHandler<T> {
    fn from(inst: T) -> JsonHandler<T> {
        JsonHandler(inst)
    }
}

impl<T: JsonHandlerExt + Send + Sync> HandlerExt for JsonHandler<T> {
    fn handle(
        &mut self,
        uri: Uri,
        body: Body,
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<Response<Body>>> {
        async move {
            match self
                .0
                .handle(
                    uri,
                    body,
                    controller_id,
                    event_subscriptions,
                    config,
                    storage,
                    accessory_database,
                    event_emitter,
                )
                .await
            {
                Ok(res) => Ok(res),
                Err(e) => match e {
                    Error::HttpStatus(status) => status_response(status),
                    _ => status_response(StatusCode::INTERNAL_SERVER_ERROR),
                },
            }
        }
        .boxed()
    }
}

/// Recheck persisted pairing at request handling, not just session establishment.
pub(crate) async fn paired_controller(
    controller: &pointer::ControllerId,
    storage: &pointer::Storage,
) -> Result<uuid::Uuid> {
    let id = (*controller
        .read()
        .map_err(|_| Error::HttpStatus(StatusCode::UNAUTHORIZED))?)
    .ok_or(Error::HttpStatus(StatusCode::UNAUTHORIZED))?;
    let pairing = storage
        .lock()
        .await
        .load_pairing(&id)
        .await
        .map_err(|_| Error::HttpStatus(StatusCode::UNAUTHORIZED))?;
    if pairing.id != id {
        return Err(Error::HttpStatus(StatusCode::UNAUTHORIZED));
    }
    Ok(id)
}

/// Bound authentication bodies before decoding attacker-controlled TLV fields.
pub(crate) async fn read_tlv_body(
    mut body: hyper::Body,
) -> std::result::Result<std::collections::HashMap<u8, Vec<u8>>, crate::tlv::ErrorContainer> {
    use hyper::body::HttpBody;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
        let mut bytes = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk.map_err(|_| crate::tlv::Error::Unknown)?;
            if chunk.len() > 65536 - bytes.len() {
                return Err(crate::tlv::Error::Unknown);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| crate::tlv::ErrorContainer::new(0, crate::tlv::Error::Unknown))?
    .map_err(|e| crate::tlv::ErrorContainer::new(0, e))?;
    crate::tlv::try_decode(&bytes).map_err(|e| crate::tlv::ErrorContainer::new(0, e))
}

#[cfg(test)]
mod authentication_input_tests {
    use super::*;
    use hyper::Body;

    #[tokio::test]
    async fn setup_and_pairings_reject_malformed_fields() {
        let setup = pair_setup::PairSetup::new();
        let pairings = pairings::Pairings::new();
        for bytes in [
            vec![6],
            vec![6, 2, 1],
            vec![6, 0],
            vec![6, 2, 1, 1],
            vec![6, 1, 1, 6, 1, 1],
        ] {
            assert!(setup.parse(Body::from(bytes.clone())).await.is_err());
            assert!(pairings.parse(Body::from(bytes)).await.is_err());
        }
        for method in [vec![], vec![3, 3]] {
            let mut bytes = vec![6, 1, 1, 0, method.len() as u8];
            bytes.extend(method);
            assert!(pairings.parse(Body::from(bytes)).await.is_err());
        }
        for key_len in [0, 31, 33] {
            let mut bytes = vec![6, 1, 1, 0, 1, 3, 1, 1, b'a', 3, key_len];
            bytes.extend(vec![1; key_len as usize]);
            bytes.extend([11, 1, 1]);
            assert!(pairings.parse(Body::from(bytes)).await.is_err());
        }
        for permission in [vec![], vec![1, 1], vec![2]] {
            let mut bytes = vec![6, 1, 1, 0, 1, 3, 1, 1, b'a', 3, 32];
            bytes.extend([1; 32]);
            bytes.extend([11, permission.len() as u8]);
            bytes.extend(permission);
            assert!(pairings.parse(Body::from(bytes)).await.is_err());
        }
    }

    #[tokio::test]
    async fn authentication_body_has_size_and_time_limits() {
        assert!(read_tlv_body(Body::from(vec![0; 65537])).await.is_err());
        let (mut sender, body) = Body::channel();
        let producer = tokio::spawn(async move {
            for _ in 0..65 {
                if sender.send_data(vec![0; 1024].into()).await.is_err() {
                    break;
                }
            }
        });
        assert!(read_tlv_body(body).await.is_err());
        producer.await.unwrap();
        let (_sender, body) = Body::channel();
        assert!(read_tlv_body(body).await.is_err());
    }
}
