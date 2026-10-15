use crate::transport::http::body::Body;
use aead::{AeadInOut, KeyInit};
use chacha20poly1305::ChaCha20Poly1305;
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use log::{debug, info};
use rand::SeedableRng;
use signature::Signer;
use std::str;
use uuid::Uuid;
use x25519_dalek::{EphemeralSecret, PublicKey};

use crate::{
    pointer,
    tlv::{self, Encodable, Type, Value},
    transport::{hkdf_extract_and_expand, http::handler::TlvHandlerExt, tcp},
};

struct Session {
    b_pub: PublicKey,
    a_pub: PublicKey,
    shared_secret: [u8; 32],
    session_key: [u8; 32],
}

pub struct PairVerify {
    session: Option<Session>,
    session_sender: Option<oneshot::Sender<tcp::Session>>,
}

impl PairVerify {
    pub fn new(session_sender: oneshot::Sender<tcp::Session>) -> PairVerify {
        PairVerify {
            session: None,
            session_sender: Some(session_sender),
        }
    }
}

#[derive(Debug, Clone)]
enum StepNumber {
    Unknown = 0,
    StartReq = 1,
    StartRes = 2,
    FinishReq = 3,
    FinishRes = 4,
}

#[derive(Debug, Clone)]
pub enum Step {
    Start { a_pub: Vec<u8> },
    Finish { data: Vec<u8> },
}

impl TlvHandlerExt for PairVerify {
    type ParseResult = Step;
    type Result = tlv::Container;

    fn parse(&self, body: Body) -> BoxFuture<'_, Result<Step, tlv::ErrorContainer>> {
        async {
            let mut decoded = super::read_tlv_body(body).await?;
            match decoded.get(&(Type::State as u8)) {
                Some(method) if method.len() == 1 => match method[0] {
                    x if x == StepNumber::StartReq as u8 => {
                        let a_pub = decoded.remove(&(Type::PublicKey as u8)).ok_or(
                            tlv::ErrorContainer::new(
                                StepNumber::StartRes as u8,
                                tlv::Error::Unknown,
                            ),
                        )?;
                        Ok(Step::Start { a_pub })
                    }
                    x if x == StepNumber::FinishReq as u8 => {
                        let data = decoded.remove(&(Type::EncryptedData as u8)).ok_or(
                            tlv::ErrorContainer::new(
                                StepNumber::FinishRes as u8,
                                tlv::Error::Unknown,
                            ),
                        )?;
                        Ok(Step::Finish { data })
                    }
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
        step: Step,
        _: pointer::ControllerId,
        config: pointer::Config,
        storage: pointer::Storage,
        _: pointer::EventEmitter,
    ) -> BoxFuture<'_, Result<tlv::Container, tlv::ErrorContainer>> {
        async move {
            match step {
                Step::Start { a_pub } => match handle_start(self, config, a_pub).await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(tlv::ErrorContainer::new(StepNumber::StartRes as u8, err)),
                },
                Step::Finish { data } => match handle_finish(self, storage, &data).await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(tlv::ErrorContainer::new(StepNumber::FinishRes as u8, err)),
                },
            }
        }
        .boxed()
    }
}

async fn handle_start(
    handler: &mut PairVerify,
    config: pointer::Config,
    a_pub_bytes: Vec<u8>,
) -> Result<tlv::Container, tlv::Error> {
    info!("pair verify M1: received verify start request");

    handler.session = None;
    if a_pub_bytes.len() != 32 {
        return Err(tlv::Error::Unknown);
    }
    let mut a_pub = [0; 32];
    a_pub.copy_from_slice(&a_pub_bytes);
    let a_pub = PublicKey::from(a_pub);

    let mut csprng = rand::rngs::StdRng::from_rng(&mut rand::rng());
    let b = EphemeralSecret::random_from_rng(&mut csprng);
    let b_pub = PublicKey::from(&b);
    let shared_secret = b.diffie_hellman(&a_pub);
    if !shared_secret.was_contributory() {
        return Err(tlv::Error::Authentication);
    }

    let config = config.lock().await;
    let device_id = config.device_id.to_string();

    let mut accessory_info: Vec<u8> = Vec::new();
    accessory_info.extend(b_pub.as_bytes());
    accessory_info.extend(device_id.as_bytes());
    accessory_info.extend(a_pub.as_bytes());
    let accessory_signature = config.device_ed25519_keypair.sign(&accessory_info);

    drop(config);

    let encoded_sub_tlv = vec![
        Value::Identifier(device_id),
        Value::Signature(accessory_signature.to_bytes().to_vec()),
    ]
    .encode();

    let session_key = hkdf_extract_and_expand(
        b"Pair-Verify-Encrypt-Salt",
        shared_secret.as_bytes(),
        b"Pair-Verify-Encrypt-Info",
    )?;

    handler.session = Some(Session {
        b_pub,
        a_pub,
        shared_secret: *shared_secret.as_bytes(),
        session_key,
    });

    let mut nonce = vec![0; 4];
    nonce.extend(b"PV-Msg02");

    let aead = ChaCha20Poly1305::new((&session_key).into());

    let mut encrypted_data = Vec::new();
    encrypted_data.extend_from_slice(&encoded_sub_tlv);
    let auth_tag =
        aead.encrypt_inout_detached(nonce.as_slice().try_into().expect("12-byte HAP nonce"), &[], encrypted_data.as_mut_slice().into())?;
    encrypted_data.extend(&auth_tag);

    info!("pair verify M2: sending verify start response");

    Ok(vec![
        Value::State(StepNumber::StartRes as u8),
        Value::PublicKey(b_pub.as_bytes().to_vec()),
        Value::EncryptedData(encrypted_data),
    ])
}

async fn handle_finish(
    handler: &mut PairVerify,
    storage: pointer::Storage,
    data: &[u8],
) -> Result<tlv::Container, tlv::Error> {
    info!("pair verify M3: received verify finish request");

    if data.len() < 16 {
        handler.session = None;
        return Err(tlv::Error::Unknown);
    }
    match handler.session.take() {
        None => Err(tlv::Error::Unknown),
        Some(session) => {
            let encrypted_data = Vec::from(&data[..data.len() - 16]);
            let auth_tag = Vec::from(&data[data.len() - 16..]);

            let mut nonce = vec![0; 4];
            nonce.extend(b"PV-Msg03");

            let aead = ChaCha20Poly1305::new((&session.session_key).into());

            let mut decrypted_data = Vec::new();
            decrypted_data.extend_from_slice(&encrypted_data);
            aead.decrypt_inout_detached(
                nonce.as_slice().try_into().expect("12-byte HAP nonce"),
                &[],
                decrypted_data.as_mut_slice().into(),
                auth_tag.as_slice().try_into().expect("16-byte HAP tag"),
            )?;

            let sub_tlv = tlv::try_decode(&decrypted_data)?;
            debug!("received sub-TLV: {:?}", &sub_tlv);
            let device_pairing_id = sub_tlv
                .get(&(Type::Identifier as u8))
                .ok_or(tlv::Error::Unknown)?;
            debug!("raw device pairing ID: {:?}", &device_pairing_id);
            let device_signature = ed25519_dalek::Signature::from_slice(
                sub_tlv
                    .get(&(Type::Signature as u8))
                    .ok_or(tlv::Error::Unknown)?,
            )?;
            debug!("device signature: {:?}", &device_signature);

            let uuid_str = str::from_utf8(device_pairing_id)?;
            let pairing_uuid = Uuid::parse_str(uuid_str)?;
            debug!("device pairing UUID: {:?}", &pairing_uuid);
            let pairing = storage.lock().await.load_pairing(&pairing_uuid).await?;
            debug!("loaded pairing: {:?}", &pairing);

            let mut device_info: Vec<u8> = Vec::new();
            device_info.extend(session.a_pub.as_bytes());
            device_info.extend(device_pairing_id);
            device_info.extend(session.b_pub.as_bytes());

            if ed25519_dalek::VerifyingKey::from_bytes(&pairing.public_key)?
                .verify_strict(&device_info, &device_signature)
                .is_err()
            {
                return Err(tlv::Error::Authentication);
            }

            if let Some(sender) = handler.session_sender.take() {
                let encrypted_session = tcp::Session {
                    controller_id: pairing_uuid,
                    shared_secret: session.shared_secret,
                };
                sender
                    .send(encrypted_session)
                    .map_err(|_| tlv::Error::Unknown)?;
            } else {
                return Err(tlv::Error::Unknown);
            }

            info!("pair verify M4: sending verify finish response");

            Ok(vec![Value::State(StepNumber::FinishRes as u8)])
        }
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use futures::lock::Mutex;
    use std::sync::Arc;
    #[tokio::test]
    async fn malformed_tlv_and_state_lengths_are_rejected_without_panicking() {
        let (sender, _receiver) = oneshot::channel();
        let handler = PairVerify::new(sender);
        for bytes in [
            vec![],
            vec![6],
            vec![6, 1],
            vec![6, 0],
            vec![6, 2, 1, 1],
            vec![6, 1, 1, 6, 1, 1],
            vec![6, 1, 1, 3, 32, 0],
        ] {
            assert!(handler.parse(Body::from(bytes)).await.is_err());
        }
        assert!(handler.parse(Body::from(vec![0; 65537])).await.is_err());
    }
    #[tokio::test]
    async fn invalid_public_keys_and_short_finish_never_create_a_session() {
        let (sender, mut receiver) = oneshot::channel();
        let mut handler = PairVerify::new(sender);
        let config = Arc::new(Mutex::new(crate::Config::default()));
        for len in [0, 1, 31, 33, 64] {
            assert!(handle_start(&mut handler, config.clone(), vec![1; len])
                .await
                .is_err());
            assert!(handler.session.is_none());
        }
        assert!(handle_start(&mut handler, config.clone(), vec![0; 32])
            .await
            .is_err());
        assert!(handler.session.is_none());
        let secret =
            EphemeralSecret::random_from_rng(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
        let public = PublicKey::from(&secret);
        assert!(
            handle_start(&mut handler, config, public.as_bytes().to_vec())
                .await
                .is_ok()
        );
        let path = std::env::temp_dir().join(format!("hap-short-verify-{}", Uuid::new_v4()));
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(
            crate::storage::FileStorage::new(path.as_os_str())
                .await
                .unwrap(),
        )));
        assert!(handle_finish(&mut handler, storage, &[0; 15])
            .await
            .is_err());
        assert!(handler.session.is_none());
        assert!(receiver.try_recv().unwrap().is_none());
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod exchange_tests {
    use super::*;
    use crate::{
        event::EventEmitter,
        pairing::{Pairing, Permissions},
        storage::{FileStorage, Storage},
    };
    use futures::lock::Mutex;
    use std::sync::Arc;

    // Uses public wire TLVs and a separately derived client key. No reads of the
    // handler's ephemeral/session internals and no direct handle_start/finish calls.
    async fn exchange(fault: u8) {
        let id = Uuid::new_v4();
        let controller_key = ed25519_dalek::SigningKey::generate(
            &mut rand::rngs::StdRng::from_rng(&mut rand::rng()),
        );
        let path = std::env::temp_dir().join(format!("hap-verify-exchange-{id}"));
        let mut store = FileStorage::new(path.as_os_str())
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"));
        let pairing = Pairing::new(
            id,
            Permissions::Admin,
            controller_key.verifying_key().to_bytes(),
        );
        store
            .save_pairing(&pairing)
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"));
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let config = crate::Config::default();
        let accessory_key = config.device_ed25519_keypair.verifying_key();
        let accessory_id = config.device_id.to_string();
        let config = Arc::new(Mutex::new(config));
        let identity = Arc::new(std::sync::RwLock::new(None));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        let (sender, mut receiver) = oneshot::channel();
        let mut handler = PairVerify::new(sender);
        let secret =
            EphemeralSecret::random_from_rng(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
        let public = PublicKey::from(&secret);
        let m1 = vec![
            Value::State(1),
            Value::PublicKey(public.as_bytes().to_vec()),
        ]
        .encode();
        let step = handler
            .parse(Body::from(m1))
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"));
        let m2 = handler
            .handle(
                step,
                identity.clone(),
                config.clone(),
                storage.clone(),
                emitter.clone(),
            )
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"))
            .encode();
        let m2 = tlv::try_decode(&m2).unwrap();
        assert_eq!(m2[&(Type::State as u8)], vec![2]);
        let server_public =
            PublicKey::from(<[u8; 32]>::try_from(m2[&(Type::PublicKey as u8)].as_slice()).unwrap());
        let shared = secret.diffie_hellman(&server_public);
        assert!(shared.was_contributory());
        let mut key = [0; 32];
        hkdf::Hkdf::<sha2::Sha512>::new(Some(b"Pair-Verify-Encrypt-Salt"), shared.as_bytes())
            .expand(b"Pair-Verify-Encrypt-Info", &mut key)
            .unwrap();
        let cipher = ChaCha20Poly1305::new((&key).into());
        let encrypted = &m2[&(Type::EncryptedData as u8)];
        let mut plaintext = encrypted[..encrypted.len() - 16].to_vec();
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(b"PV-Msg02");
        cipher
            .decrypt_inout_detached(
                (&nonce).into(),
                &[],
                plaintext.as_mut_slice().into(),
                encrypted[encrypted.len() - 16..].try_into().unwrap(),
            )
            .unwrap();
        let sub = tlv::try_decode(&plaintext).unwrap();
        assert_eq!(sub[&(Type::Identifier as u8)], accessory_id.as_bytes());
        let signed = [
            server_public.as_bytes().as_slice(),
            accessory_id.as_bytes(),
            public.as_bytes().as_slice(),
        ]
        .concat();
        let signature =
            ed25519_dalek::Signature::from_slice(&sub[&(Type::Signature as u8)]).unwrap();
        accessory_key.verify_strict(&signed, &signature).unwrap();
        let claimed = if fault == 2 { Uuid::new_v4() } else { id };
        let claimed = claimed.to_string();
        let signed = [
            public.as_bytes().as_slice(),
            claimed.as_bytes(),
            server_public.as_bytes().as_slice(),
        ]
        .concat();
        let signing_key = if fault == 1 {
            ed25519_dalek::SigningKey::generate(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()))
        } else {
            controller_key
        };
        let signature = signing_key.sign(&signed);
        let mut encrypted = vec![
            Value::Identifier(claimed),
            Value::Signature(signature.to_bytes().to_vec()),
        ]
        .encode();
        nonce[4..].copy_from_slice(b"PV-Msg03");
        let tag = cipher
            .encrypt_inout_detached((&nonce).into(), &[], encrypted.as_mut_slice().into())
            .unwrap();
        encrypted.extend(tag);
        if fault == 3 {
            *encrypted.last_mut().unwrap() ^= 1;
        }
        let m3 = vec![Value::State(3), Value::EncryptedData(encrypted)].encode();
        let step = handler
            .parse(Body::from(m3.clone()))
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"));
        let result = handler
            .handle(
                step,
                identity.clone(),
                config.clone(),
                storage.clone(),
                emitter.clone(),
            )
            .await;
        if fault == 0 {
            let m4 = tlv::try_decode(&result.unwrap_or_else(|_| panic!("M3 rejected")).encode())
                .unwrap();
            assert_eq!(m4[&(Type::State as u8)], vec![4]);
            let session = receiver.try_recv().unwrap().unwrap();
            assert_eq!(session.controller_id, id);
            assert_eq!(session.shared_secret, *shared.as_bytes());
        } else {
            assert!(result.is_err());
            assert!(receiver.try_recv().unwrap().is_none());
        }
        // Success and all failure variants consume M3 state; replay cannot grant a session.
        let replay = handler
            .parse(Body::from(m3))
            .await
            .unwrap_or_else(|_| panic!("exchange step failed"));
        assert!(handler
            .handle(replay, identity.clone(), config, storage.clone(), emitter)
            .await
            .is_err());
        assert!(identity.read().unwrap().is_none());
        assert_eq!(
            storage
                .lock()
                .await
                .load_pairing(&id)
                .await
                .unwrap_or_else(|_| panic!("exchange step failed")),
            pairing
        );
        std::fs::remove_dir_all(path).unwrap();
    }
    #[tokio::test]
    async fn m1_through_m4_verifies_both_signatures_and_delivers_matching_secret_once() {
        exchange(0).await;
    }
    #[tokio::test]
    async fn wrong_signer_unknown_pairing_and_bad_tag_never_deliver_a_session() {
        for fault in 1..=3 {
            exchange(fault).await;
        }
    }
}
