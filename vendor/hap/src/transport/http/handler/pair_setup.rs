use aead::{array::Array, AeadInPlace, KeyInit};
use chacha20poly1305::ChaCha20Poly1305;
use futures::future::{BoxFuture, FutureExt};
use crate::transport::http::body::Body;
use log::{debug, info};
use num::BigUint;
use rand::{Rng, SeedableRng};
use sha2::{digest::Digest, Sha512};
use signature::Signer;
use srp::{
    client::SrpClient,
    groups::G_3072,
    server::SrpServer,
    types::SrpGroup,
};
use std::{ops::BitXor, str};
use uuid::Uuid;

use crate::{
    event::Event,
    pairing::{Pairing, Permissions},
    pointer,
    tlv::{self, Encodable, Type, Value},
    transport::{hkdf_extract_and_expand, http::handler::TlvHandlerExt},
};

struct Session {
    salt: [u8; 16],
    verifier: Vec<u8>,
    b: [u8; 64],
    b_pub: Vec<u8>,
    shared_secret: Option<Vec<u8>>,
}

pub struct PairSetup {
    session: Option<Session>,
    unsuccessful_tries: u8,
}

impl PairSetup {
    pub fn new() -> PairSetup {
        PairSetup {
            session: None,
            unsuccessful_tries: 0,
        }
    }
}

#[derive(Debug, Clone)]
enum StepNumber {
    Unknown = 0,
    SrpStartRequest = 1,
    SrpStartResponse = 2,
    SrpVerifyRequest = 3,
    SrpVerifyResponse = 4,
    ExchangeRequest = 5,
    ExchangeResponse = 6,
}

#[derive(Debug, Clone)]
pub enum Step {
    Start,
    Verify { a_pub: Vec<u8>, a_proof: Vec<u8> },
    Exchange { data: Vec<u8> },
}

impl TlvHandlerExt for PairSetup {
    type ParseResult = Step;
    type Result = tlv::Container;

    fn parse(&self, body: Body) -> BoxFuture<Result<Step, tlv::ErrorContainer>> {
        async {
            let mut decoded = super::read_tlv_body(body).await?;
            match decoded.get(&(Type::State as u8)) {
                Some(method) if method.len() == 1 => match method[0] {
                    x if x == StepNumber::SrpStartRequest as u8 => Ok(Step::Start),
                    x if x == StepNumber::SrpVerifyRequest as u8 => {
                        let a_pub = decoded.remove(&(Type::PublicKey as u8)).ok_or(
                            tlv::ErrorContainer::new(
                                StepNumber::SrpVerifyResponse as u8,
                                tlv::Error::Unknown,
                            ),
                        )?;
                        let a_proof = decoded.remove(&(Type::Proof as u8)).ok_or(
                            tlv::ErrorContainer::new(
                                StepNumber::SrpVerifyResponse as u8,
                                tlv::Error::Unknown,
                            ),
                        )?;
                        Ok(Step::Verify { a_pub, a_proof })
                    }
                    x if x == StepNumber::ExchangeRequest as u8 => {
                        let data = decoded.remove(&(Type::EncryptedData as u8)).ok_or(
                            tlv::ErrorContainer::new(
                                StepNumber::ExchangeResponse as u8,
                                tlv::Error::Unknown,
                            ),
                        )?;
                        Ok(Step::Exchange { data })
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
        event_emitter: pointer::EventEmitter,
    ) -> BoxFuture<Result<tlv::Container, tlv::ErrorContainer>> {
        async move {
            match step {
                Step::Start => match handle_start(self, config, storage).await {
                    Ok(res) => {
                        self.unsuccessful_tries = 0;
                        Ok(res)
                    }
                    Err(err) => {
                        self.unsuccessful_tries = self.unsuccessful_tries.saturating_add(1);
                        Err(tlv::ErrorContainer::new(
                            StepNumber::SrpStartResponse as u8,
                            err,
                        ))
                    }
                },
                Step::Verify { a_pub, a_proof } => {
                    match handle_verify(self, &a_pub, &a_proof).await {
                        Ok(res) => {
                            self.unsuccessful_tries = 0;
                            Ok(res)
                        }
                        Err(err) => {
                            self.unsuccessful_tries = self.unsuccessful_tries.saturating_add(1);
                            Err(tlv::ErrorContainer::new(
                                StepNumber::SrpVerifyResponse as u8,
                                err,
                            ))
                        }
                    }
                }
                Step::Exchange { data } => {
                    match handle_exchange(self, config, storage, event_emitter, &data).await {
                        Ok(res) => {
                            self.unsuccessful_tries = 0;
                            Ok(res)
                        }
                        Err(err) => {
                            self.unsuccessful_tries = self.unsuccessful_tries.saturating_add(1);
                            Err(tlv::ErrorContainer::new(
                                StepNumber::ExchangeResponse as u8,
                                err,
                            ))
                        }
                    }
                }
            }
        }
        .boxed()
    }
}

async fn handle_start(
    handler: &mut PairSetup,
    config: pointer::Config,
    storage: pointer::Storage,
) -> Result<tlv::Container, tlv::Error> {
    info!("pair setup M1: received SRP start request");

    handler.session = None;
    if storage.lock().await.count_pairings().await? != 0 {
        return Err(tlv::Error::Unavailable);
    }

    if handler.unsuccessful_tries > 100 {
        return Err(tlv::Error::MaxTries);
    }

    // TODO
    // If the accessory is currently performing a PairSetup procedure with a different controller, it must respond with
    // the following TLV items:
    // kTLVType_State <M2>
    // kTLVType_Error <kTLVError_Busy>

    let mut csprng = rand::rngs::StdRng::from_rng(&mut rand::rng());
    let mut salt = [0; 16]; // s
    let mut b = [0; 64];
    csprng.fill_bytes(&mut salt);
    csprng.fill_bytes(&mut b);

    // TODO - respect pairing flags (specification p. 35 - 7.) for split pair setup

    let srp_client = SrpClient::<sha2_srp::Sha512>::new(&G_3072);
    let verifier = srp_client.compute_verifier(b"Pair-Setup", config.lock().await.pin.to_string().as_bytes(), &salt);
    let srp_server = SrpServer::<sha2_srp::Sha512>::new(&G_3072);
    let b_pub = srp_server.compute_public_ephemeral(&b, &verifier);

    handler.session = Some(Session {
        salt,
        verifier,
        b,
        b_pub: b_pub.clone(),
        shared_secret: None,
    });

    info!("pair setup M2: sending SRP start response");

    Ok(vec![
        Value::State(StepNumber::SrpStartResponse as u8),
        Value::PublicKey(b_pub),
        Value::Salt(salt),
    ])
}

async fn handle_verify(
    handler: &mut PairSetup,
    a_pub: &[u8],
    a_proof: &[u8],
) -> Result<tlv::Container, tlv::Error> {
    info!("pair setup M3: received SRP verify request");

    match handler.session.take() {
        None => Err(tlv::Error::Unknown),
        Some(mut session) => {
            if session.shared_secret.is_some()
                || a_pub.is_empty()
                || a_pub.len() > 384
                || a_proof.len() != 64
            {
                return Err(tlv::Error::Authentication);
            }
            let srp_server = SrpServer::<sha2_srp::Sha512>::new(&G_3072);
            let verifier = srp_server.process_reply(&session.b, &session.verifier, a_pub)?;
            // SRP 0.6 exposes the premaster S; HomeKit uses K = SHA512(S).
            let shared_secret = Sha512::digest(verifier.key()).to_vec();

            let b_proof = verify_client_proof::<Sha512>(
                &session.b_pub,
                a_pub,
                a_proof,
                &session.salt,
                &shared_secret,
                &G_3072,
            )?;

            session.shared_secret = Some(shared_secret.to_vec());
            handler.session = Some(session);

            info!("pair setup M4: sending SRP verify response");

            Ok(vec![
                Value::State(StepNumber::SrpVerifyResponse as u8),
                Value::Proof(b_proof),
            ])
        }
    }
}

async fn handle_exchange(
    handler: &mut PairSetup,
    config: pointer::Config,
    storage: pointer::Storage,
    event_emitter: pointer::EventEmitter,
    data: &[u8],
) -> Result<tlv::Container, tlv::Error> {
    info!("pair setup M5: received exchange request");

    if data.len() < 16 {
        return Err(tlv::Error::Unknown);
    }
    match handler.session.take() {
        None => Err(tlv::Error::Unknown),
        Some(session) => match session.shared_secret {
            None => Err(tlv::Error::Unknown),
            Some(ref shared_secret) => {
                let encrypted_data = Vec::from(&data[..data.len() - 16]);
                let auth_tag = Vec::from(&data[data.len() - 16..]);

                let encryption_key = hkdf_extract_and_expand(
                    b"Pair-Setup-Encrypt-Salt",
                    shared_secret,
                    b"Pair-Setup-Encrypt-Info",
                )?;

                let mut nonce = vec![0; 4];
                nonce.extend(b"PS-Msg05");

                let aead = ChaCha20Poly1305::new(Array::from_slice(&encryption_key));

                let mut decrypted_data = Vec::new();
                decrypted_data.extend_from_slice(&encrypted_data);
                aead.decrypt_in_place_detached(
                    Array::from_slice(&nonce),
                    &[],
                    &mut decrypted_data,
                    Array::from_slice(&auth_tag),
                )?;

                let sub_tlv = tlv::try_decode(&decrypted_data)?;
                let device_pairing_id = sub_tlv
                    .get(&(Type::Identifier as u8))
                    .ok_or(tlv::Error::Unknown)?;
                let device_ltpk = ed25519_dalek::VerifyingKey::try_from(
                    sub_tlv
                        .get(&(Type::PublicKey as u8))
                        .ok_or(tlv::Error::Unknown)?
                        .as_slice(),
                )?;
                let device_signature = ed25519_dalek::Signature::from_slice(
                    sub_tlv
                        .get(&(Type::Signature as u8))
                        .ok_or(tlv::Error::Unknown)?,
                )?;

                let device_x = hkdf_extract_and_expand(
                    b"Pair-Setup-Controller-Sign-Salt",
                    shared_secret,
                    b"Pair-Setup-Controller-Sign-Info",
                )?;

                let mut device_info: Vec<u8> = Vec::new();
                device_info.extend(&device_x);
                device_info.extend(device_pairing_id);
                device_info.extend(device_ltpk.as_bytes());

                if device_ltpk
                    .verify_strict(&device_info, &device_signature)
                    .is_err()
                {
                    return Err(tlv::Error::Authentication);
                }

                let uuid_str = str::from_utf8(device_pairing_id)?;
                let mut pairing_ltpk = [0; 32];
                pairing_ltpk[..32].copy_from_slice(&device_ltpk.as_bytes()[..32]);

                let pairing =
                    Pairing::from_identifier(uuid_str, Permissions::Admin, device_ltpk.to_bytes())?;

                debug!("pairing: {:?}", &pairing);

                let accessory_x = hkdf_extract_and_expand(
                    b"Pair-Setup-Accessory-Sign-Salt",
                    shared_secret,
                    b"Pair-Setup-Accessory-Sign-Info",
                )?;

                let configuration = config.lock().await;
                let device_id = configuration.device_id.to_string();

                let mut accessory_info: Vec<u8> = Vec::new();
                accessory_info.extend(&accessory_x);
                accessory_info.extend(device_id.as_bytes());
                accessory_info.extend(
                    configuration
                        .device_ed25519_keypair
                        .verifying_key()
                        .as_bytes(),
                );
                let accessory_signature =
                    configuration.device_ed25519_keypair.sign(&accessory_info);

                let encoded_sub_tlv = vec![
                    Value::Identifier(device_id),
                    Value::PublicKey(
                        configuration
                            .device_ed25519_keypair
                            .verifying_key()
                            .as_bytes()
                            .to_vec(),
                    ),
                    Value::Signature(accessory_signature.to_bytes().to_vec()),
                ]
                .encode();

                drop(configuration);

                let mut nonce = vec![0; 4];
                nonce.extend(b"PS-Msg06");

                let mut encrypted_data = Vec::new();
                encrypted_data.extend_from_slice(&encoded_sub_tlv);
                let auth_tag = aead.encrypt_in_place_detached(
                    Array::from_slice(&nonce),
                    &[],
                    &mut encrypted_data,
                )?;
                encrypted_data.extend(&auth_tag);

                // Build M6 before persisting; serialize the final empty-store check
                // and save so two concurrent setup sessions cannot both enroll.
                let max_peers = config.lock().await.max_peers;
                save_initial_pairing(&storage, &pairing, max_peers).await?;

                event_emitter
                    .lock()
                    .await
                    .emit(&Event::ControllerPaired { id: pairing.id })
                    .await;

                info!("pair setup M6: sending exchange response");

                Ok(vec![
                    Value::State(StepNumber::ExchangeResponse as u8),
                    Value::EncryptedData(encrypted_data),
                ])
            }
        },
    }
}

fn verify_client_proof<D: Digest>(
    b_pub: &[u8],
    a_pub: &[u8],
    a_proof: &[u8],
    salt: &[u8],
    key: &[u8],
    group: &SrpGroup,
) -> Result<Vec<u8>, tlv::Error> {
    let mut dhn = D::new();
    dhn.update(&group.n.to_bytes_be());
    let hn = BigUint::from_bytes_be(&dhn.finalize());

    let mut dhg = D::new();
    dhg.update(&group.g.to_bytes_be());
    let hg = BigUint::from_bytes_be(&dhg.finalize());

    let hng = hn.bitxor(hg);

    let mut dhi = D::new();
    dhi.update(b"Pair-Setup");
    let hi = dhi.finalize();

    let mut d = D::new();
    // M = H(H(N) xor H(g), H(I), s, A, B, K)
    d.update(&hng.to_bytes_be());
    d.update(&hi);
    d.update(salt);
    d.update(a_pub);
    d.update(b_pub);
    d.update(key);

    if a_proof == d.finalize().as_slice() {
        // H(A, M, K)
        let mut d = D::new();
        d.update(a_pub);
        d.update(a_proof);
        d.update(key);
        Ok(d.finalize().as_slice().to_vec())
    } else {
        Err(tlv::Error::Authentication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_client_proof() {
        let b_pub = [
            66, 254, 195, 54, 248, 36, 231, 56, 186, 118, 54, 42, 5, 66, 113, 73, 150, 54, 54, 156,
            16, 76, 252, 50, 206, 135, 204, 173, 159, 174, 68, 125, 23, 251, 75, 215, 136, 102,
            165, 73, 107, 249, 205, 158, 32, 192, 157, 134, 17, 70, 51, 225, 74, 3, 195, 74, 124,
            137, 26, 48, 245, 253, 212, 197, 29, 141, 12, 5, 36, 191, 77, 203, 137, 72, 27, 38,
            231, 136, 43, 60, 134, 137, 192, 134, 218, 13, 90, 232, 166, 245, 168, 29, 34, 91, 121,
            185, 82, 144, 146, 215, 6, 120, 204, 126, 217, 146, 135, 243, 55, 1, 99, 159, 192, 53,
            80, 103, 19, 87, 253, 193, 104, 98, 228, 167, 138, 214, 84, 135, 232, 225, 185, 158,
            114, 48, 228, 110, 137, 109, 32, 21, 227, 177, 18, 241, 240, 240, 125, 115, 135, 46,
            249, 172, 206, 170, 41, 76, 169, 163, 62, 77, 94, 51, 116, 69, 188, 218, 171, 90, 171,
            128, 163, 84, 233, 52, 134, 77, 70, 130, 207, 84, 55, 218, 155, 199, 67, 214, 10, 25,
            142, 134, 34, 65, 43, 52, 113, 92, 234, 213, 101, 32, 12, 34, 87, 31, 82, 132, 175,
            144, 116, 36, 47, 119, 221, 46, 103, 44, 80, 74, 201, 196, 249, 230, 127, 123, 80, 118,
            216, 163, 249, 186, 218, 184, 181, 144, 72, 187, 101, 164, 150, 209, 45, 247, 70, 184,
            92, 155, 146, 175, 162, 54, 95, 155, 174, 116, 38, 190, 166, 137, 133, 51, 253, 21,
            169, 129, 167, 61, 102, 49, 115, 24, 245, 130, 171, 6, 196, 177, 44, 49, 149, 39, 239,
            142, 253, 49, 123, 226, 140, 40, 43, 193, 131, 20, 30, 17, 57, 252, 1, 68, 55, 1, 227,
            226, 24, 73, 204, 237, 230, 2, 81, 203, 54, 204, 157, 174, 22, 134, 126, 132, 106, 66,
            115, 27, 189, 157, 131, 173, 205, 36, 140, 160, 183, 171, 62, 97, 7, 73, 93, 114, 199,
            50, 192, 22, 56, 132, 172, 61, 208, 84, 102, 85, 60, 216, 170, 249, 29, 66, 158, 48,
            221, 78, 166, 50, 20, 203, 99, 206, 6, 112, 163, 194, 43, 165, 176, 193, 62, 157, 184,
            57, 235, 221, 175, 46, 7, 152,
        ];
        let a_pub = [
            21, 187, 129, 247, 180, 115, 7, 3, 46, 219, 190, 0, 89, 8, 126, 225, 37, 159, 98, 137,
            226, 202, 13, 228, 31, 55, 17, 244, 180, 120, 159, 97, 141, 126, 131, 219, 227, 64, 81,
            24, 99, 206, 12, 218, 27, 95, 43, 216, 151, 228, 46, 89, 133, 153, 100, 55, 134, 206,
            65, 165, 244, 228, 121, 88, 78, 54, 19, 220, 48, 86, 150, 92, 21, 168, 219, 226, 214,
            254, 33, 26, 135, 134, 247, 49, 58, 114, 245, 76, 67, 182, 223, 191, 93, 1, 131, 58,
            76, 62, 49, 123, 11, 10, 164, 221, 249, 114, 77, 154, 179, 95, 207, 127, 9, 154, 30,
            91, 99, 67, 160, 94, 33, 239, 178, 32, 173, 245, 25, 91, 153, 189, 34, 216, 16, 100,
            160, 145, 150, 208, 85, 135, 237, 13, 225, 234, 144, 137, 224, 217, 187, 77, 10, 51,
            240, 45, 72, 9, 184, 164, 195, 153, 55, 66, 98, 110, 208, 204, 45, 204, 14, 46, 195,
            119, 25, 83, 120, 223, 17, 167, 79, 75, 68, 182, 105, 97, 176, 15, 153, 95, 170, 16,
            33, 172, 134, 114, 73, 69, 5, 49, 78, 133, 250, 44, 136, 75, 179, 33, 213, 48, 77, 236,
            16, 148, 221, 74, 185, 209, 83, 94, 167, 180, 101, 170, 91, 129, 143, 228, 180, 251,
            185, 210, 16, 74, 26, 248, 65, 208, 73, 182, 161, 120, 59, 223, 98, 166, 58, 94, 69,
            250, 93, 75, 29, 36, 41, 211, 242, 115, 253, 65, 28, 204, 117, 167, 194, 113, 98, 180,
            54, 96, 170, 81, 125, 134, 19, 213, 2, 213, 178, 163, 108, 74, 215, 177, 52, 217, 75,
            225, 144, 220, 3, 164, 87, 156, 233, 1, 99, 218, 251, 89, 45, 165, 227, 98, 232, 143,
            165, 141, 246, 196, 230, 209, 136, 35, 75, 218, 63, 15, 94, 150, 79, 113, 245, 247, 28,
            139, 115, 136, 232, 175, 49, 228, 11, 70, 234, 131, 126, 167, 31, 234, 202, 34, 27,
            240, 195, 201, 1, 29, 98, 229, 254, 146, 87, 223, 113, 5, 218, 217, 250, 58, 115, 53,
            9, 162, 175, 42, 176, 10, 195, 48, 155, 56, 58, 246, 219, 98, 166, 41, 34, 216, 225,
            29, 28, 1, 18, 89,
        ];
        let a_proof = [
            33, 202, 204, 21, 29, 11, 142, 163, 254, 113, 245, 137, 104, 89, 101, 247, 182, 215,
            41, 42, 213, 171, 173, 142, 172, 183, 214, 187, 204, 48, 253, 153, 224, 246, 18, 246,
            72, 5, 95, 165, 27, 245, 255, 22, 229, 250, 129, 33, 8, 40, 1, 194, 131, 19, 51, 75,
            37, 179, 34, 60, 222, 13, 182, 81,
        ];
        let salt = [
            67, 81, 86, 223, 10, 171, 140, 180, 0, 188, 111, 77, 228, 110, 173, 185,
        ];
        let shared_secret = [
            39, 162, 132, 15, 245, 98, 72, 107, 190, 101, 117, 162, 228, 189, 241, 14, 132, 216,
            104, 7, 65, 72, 90, 154, 86, 129, 171, 235, 197, 55, 174, 216, 183, 170, 12, 101, 219,
            128, 62, 155, 113, 212, 250, 40, 137, 178, 199, 215, 68, 139, 218, 112, 205, 68, 52,
            66, 95, 11, 116, 251, 143, 93, 206, 89,
        ];

        let b_proof =
            verify_client_proof::<Sha512>(&b_pub, &a_pub, &a_proof, &salt, &shared_secret, &G_3072)
                .unwrap();

        assert_eq!(
            b_proof,
            vec![
                53, 222, 231, 209, 7, 123, 202, 208, 135, 119, 183, 90, 79, 154, 55, 155, 63, 56,
                215, 210, 4, 20, 229, 119, 234, 168, 107, 137, 48, 172, 180, 244, 184, 142, 170,
                120, 188, 106, 94, 135, 122, 4, 211, 21, 190, 26, 121, 180, 13, 192, 173, 246, 172,
                223, 161, 192, 52, 251, 187, 66, 52, 170, 18, 85
            ]
        );
    }
}

/// Initial enrollment is permitted only while no controller is stored.
async fn save_initial_pairing(
    storage: &pointer::Storage,
    pairing: &Pairing,
    max_peers: Option<usize>,
) -> Result<(), tlv::Error> {
    let mut storage = storage.lock().await;
    if storage.count_pairings().await? != 0 {
        return Err(tlv::Error::Unavailable);
    }
    if max_peers == Some(0) {
        return Err(tlv::Error::MaxPeers);
    }
    storage.save_pairing(pairing).await?;
    Ok(())
}

#[cfg(test)]
mod enrollment_tests {
    use super::*;
    use crate::storage::FileStorage;
    use futures::lock::Mutex;
    use std::sync::Arc;

    #[tokio::test]
    async fn initial_enrollment_is_exclusive_and_paired_start_is_rejected() {
        let path = std::env::temp_dir().join(format!("hap-enrollment-{}", Uuid::new_v4()));
        let store = FileStorage::new(path.as_os_str()).await.unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let first = Pairing::new(Uuid::new_v4(), Permissions::Admin, [1; 32]);
        let second = Pairing::new(Uuid::new_v4(), Permissions::Admin, [2; 32]);
        assert!(save_initial_pairing(&storage, &first, Some(0))
            .await
            .is_err());
        assert_eq!(storage.lock().await.count_pairings().await.unwrap(), 0);
        let (a, b) = futures::join!(
            save_initial_pairing(&storage, &first, None),
            save_initial_pairing(&storage, &second, None)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(storage.lock().await.count_pairings().await.unwrap(), 1);
        let winner = if a.is_ok() { first.id } else { second.id };
        assert!(save_initial_pairing(&storage, &second, None).await.is_err());
        assert!(storage.lock().await.load_pairing(&winner).await.is_ok());
        let config = Arc::new(Mutex::new(crate::Config::default()));
        let mut handler = PairSetup::new();
        handler.session = Some(Session {
            salt: [0; 16],
            verifier: vec![],
            b: [0; 64],
            b_pub: vec![],
            shared_secret: None,
        });
        assert!(handle_start(&mut handler, config, storage.clone())
            .await
            .is_err());
        assert!(handler.session.is_none());
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod complete_exchange_tests {
    use super::*;
    use crate::{event::EventEmitter, storage::FileStorage};
    use futures::lock::Mutex;
    use std::sync::{Arc, RwLock};

    async fn exchange(bad_proof: bool) {
        let path = std::env::temp_dir().join(format!("hap-setup-wire-{}", Uuid::new_v4()));
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(
            FileStorage::new(path.as_os_str()).await.unwrap(),
        )));
        let settings = crate::Config::default();
        let pin = settings.pin.to_string();
        let accessory_key = settings.device_ed25519_keypair.verifying_key();
        let accessory_id = settings.device_id.to_string();
        let config = Arc::new(Mutex::new(settings));
        let emitter = Arc::new(Mutex::new(EventEmitter::new()));
        let controller = Arc::new(RwLock::new(None));
        let mut handler = PairSetup::new();
        let m1 = handler
            .parse(Body::from(vec![6, 1, 1, 0, 1, 0]))
            .await
            .unwrap_or_else(|_| panic!("M1 parse"));
        let m2 = handler
            .handle(
                m1,
                controller.clone(),
                config.clone(),
                storage.clone(),
                emitter.clone(),
            )
            .await
            .unwrap_or_else(|_| panic!("M1 handle"))
            .encode();
        let m2 = tlv::try_decode(&m2).unwrap();
        assert_eq!(m2[&(Type::State as u8)], vec![2]);
        let salt = &m2[&(Type::Salt as u8)];
        let mut ephemeral = [0; 64];
        rand::rngs::StdRng::from_rng(&mut rand::rng()).fill_bytes(&mut ephemeral);
        let client = SrpClient::<sha2_srp::Sha512>::new(&G_3072);
        let public = client.compute_public_ephemeral(&ephemeral);
        // The HAP candidate uses unpadded g in the proof transcript, unlike
        // srp 0.6's generic proof helper. Derive the client
        // secret with SRP, then build and verify the wire transcript here.
        let key = client
            .process_reply(&ephemeral, b"Pair-Setup", pin.as_bytes(), salt, &m2[&(Type::PublicKey as u8)])
            .unwrap()
            .key().to_vec();
        let key = Sha512::digest(&key).to_vec();
        let hn = Sha512::digest(&G_3072.n.to_bytes_be());
        let hg = Sha512::digest(&G_3072.g.to_bytes_be());
        let xor: Vec<u8> = hn.iter().zip(hg.iter()).map(|(n, g)| n ^ g).collect();
        let mut transcript = Sha512::new();
        transcript.update(&xor);
        transcript.update(Sha512::digest(b"Pair-Setup"));
        transcript.update(salt);
        transcript.update(&public);
        transcript.update(&m2[&(Type::PublicKey as u8)]);
        transcript.update(&key);
        let mut proof = transcript.finalize().to_vec();
        let mut server_proof = Sha512::new();
        server_proof.update(&public);
        server_proof.update(&proof);
        server_proof.update(&key);
        let server_proof = server_proof.finalize();
        if bad_proof {
            proof[0] ^= 1;
        }
        let m3 = vec![
            Value::State(3),
            Value::PublicKey(public),
            Value::Proof(proof),
        ]
        .encode();
        let m3 = handler
            .parse(Body::from(m3))
            .await
            .unwrap_or_else(|_| panic!("M3 parse"));
        let m4 = handler
            .handle(
                m3,
                controller.clone(),
                config.clone(),
                storage.clone(),
                emitter.clone(),
            )
            .await;
        if bad_proof {
            assert!(m4.is_err());
        } else {
            let m4 = tlv::try_decode(&m4.unwrap_or_else(|_| panic!("M3 handle")).encode()).unwrap();
            assert_eq!(m4[&(Type::State as u8)], vec![4]);
            assert_eq!(m4[&(Type::Proof as u8)], server_proof.as_slice());
        };
        let id = Uuid::new_v4();
        let signing = ed25519_dalek::SigningKey::generate(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
        let derive = |salt: &[u8], info: &[u8]| {
            let mut output = [0; 32];
            hkdf::Hkdf::<sha2::Sha512>::new(Some(salt), &key)
                .expand(info, &mut output)
                .unwrap();
            output
        };
        let x = derive(
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        );
        let signed = [
            x.as_slice(),
            id.to_string().as_bytes(),
            signing.verifying_key().as_bytes(),
        ]
        .concat();
        let mut data = vec![
            Value::Identifier(id.to_string()),
            Value::PublicKey(signing.verifying_key().to_bytes().to_vec()),
            Value::Signature(signing.sign(&signed).to_bytes().to_vec()),
        ]
        .encode();
        let encryption_key = derive(b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
        let cipher = ChaCha20Poly1305::new(Array::from_slice(&encryption_key));
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(b"PS-Msg05");
        let tag = cipher
            .encrypt_in_place_detached(Array::from_slice(&nonce), &[], &mut data)
            .unwrap();
        data.extend(tag);
        let m5 = vec![Value::State(5), Value::EncryptedData(data)].encode();
        let step = handler
            .parse(Body::from(m5.clone()))
            .await
            .unwrap_or_else(|_| panic!("M5 parse"));
        let result = handler
            .handle(
                step,
                controller.clone(),
                config.clone(),
                storage.clone(),
                emitter.clone(),
            )
            .await;
        if bad_proof {
            assert!(result.is_err());
            assert_eq!(storage.lock().await.count_pairings().await.unwrap(), 0);
        } else {
            let m6 =
                tlv::try_decode(&result.unwrap_or_else(|_| panic!("M5 handle")).encode()).unwrap();
            assert_eq!(m6[&(Type::State as u8)], vec![6]);
            let data = &m6[&(Type::EncryptedData as u8)];
            let mut plaintext = data[..data.len() - 16].to_vec();
            nonce[4..].copy_from_slice(b"PS-Msg06");
            cipher
                .decrypt_in_place_detached(
                    Array::from_slice(&nonce),
                    &[],
                    &mut plaintext,
                    Array::from_slice(&data[data.len() - 16..]),
                )
                .unwrap();
            let sub = tlv::try_decode(&plaintext).unwrap();
            assert_eq!(sub[&(Type::Identifier as u8)], accessory_id.as_bytes());
            assert_eq!(sub[&(Type::PublicKey as u8)], accessory_key.as_bytes());
            let x = derive(
                b"Pair-Setup-Accessory-Sign-Salt",
                b"Pair-Setup-Accessory-Sign-Info",
            );
            let signed = [
                x.as_slice(),
                accessory_id.as_bytes(),
                accessory_key.as_bytes(),
            ]
            .concat();
            accessory_key
                .verify_strict(
                    &signed,
                    &ed25519_dalek::Signature::from_slice(&sub[&(Type::Signature as u8)]).unwrap(),
                )
                .unwrap();
            let pairing = storage.lock().await.load_pairing(&id).await.unwrap();
            assert_eq!(pairing.public_key, signing.verifying_key().to_bytes());
            assert_eq!(pairing.permissions, Permissions::Admin);
        }
        let repeated = handler
            .parse(Body::from(m5))
            .await
            .unwrap_or_else(|_| panic!("repeat parse"));
        assert!(handler
            .handle(repeated, controller, config, storage.clone(), emitter)
            .await
            .is_err());
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[tokio::test]
    async fn m1_to_m6_authenticates_both_sides_and_stores_controller() {
        exchange(false).await;
    }
    #[tokio::test]
    async fn invalid_srp_proof_cannot_continue_to_valid_encrypted_enrollment() {
        exchange(true).await;
    }
}
