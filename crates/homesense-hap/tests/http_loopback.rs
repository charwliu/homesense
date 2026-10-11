//! Real TCP/HTTP boundary; no mDNS and no physical bus.
use hap::{
    Config,
    server::{IpServer, Server},
    storage::Storage,
};
use homesense_hap::storage::IdentityStore;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
mod support;
use support::{encode_tlv, post_tlv_path, tlv_items};

fn request(address: SocketAddr, request: &[u8]) -> Result<String, String> {
    let end = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            Ok(stream) => break stream,
            Err(error) => {
                if Instant::now() >= end {
                    return Err(error.to_string());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream.write_all(request).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("EOF before HTTP headers".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > 8192 {
            return Err("HTTP header exceeds bound".into());
        }
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[tokio::test]
async fn real_http_rejects_unpaired_reads_and_writes_then_releases_listener() {
    let directory = std::env::temp_dir().join(format!("homesense-http-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut store = IdentityStore::open(&directory).await.unwrap();
    let config = Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    store.save_config(&config).await.unwrap();
    let server = IpServer::new_loopback(config, store.clone()).await.unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let responses=tokio::task::spawn_blocking(move || {
        let read=request(address,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let write=request(address,b"PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")?;
        Ok::<_,String>((read,write))
    }).await.unwrap();
    task.abort();
    let _ = task.await;
    let rebound = TcpListener::bind(address);
    let pairings = store.count_pairings().await.unwrap();
    drop(server);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
    let (read, write) = responses.unwrap();
    assert!(read.starts_with("HTTP/1.1 401"), "{read}");
    assert!(write.starts_with("HTTP/1.1 401"), "{write}");
    assert_eq!(pairings, 0);
    assert!(rebound.is_ok(), "listener not released");
}

fn post_tlv(stream: &mut TcpStream, body: &[u8]) -> Result<Vec<u8>, String> {
    post_tlv_path(stream, "/pair-setup", body)
}

#[tokio::test]
async fn real_http_pair_setup_rejects_wrong_proof_without_persisting_a_controller() {
    let directory = std::env::temp_dir().join(format!("homesense-pair-http-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut store = IdentityStore::open(&directory).await.unwrap();
    let config = Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    store.save_config(&config).await.unwrap();
    let server = IpServer::new_loopback(config, store.clone()).await.unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Reuse the readiness helper, then keep all SRP steps on one connection.
        request(
            address,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        let m2 = tlv_items(&post_tlv(&mut stream, &[6, 1, 1, 0, 1, 0])?)?;
        if m2.get(&6) != Some(&vec![2])
            || m2.get(&2).map(Vec::len) != Some(16)
            || m2.get(&3).map(Vec::len) != Some(384)
        {
            return Err("invalid M2".into());
        }
        let mut m3 = vec![6, 1, 3, 3, 1, 1, 4, 64];
        m3.extend_from_slice(&[0; 64]);
        let m4 = tlv_items(&post_tlv(&mut stream, &m3)?)?;
        if m4.get(&6) != Some(&vec![4]) || !m4.contains_key(&7) {
            return Err("wrong proof accepted".into());
        }
        let mut m5 = vec![6, 1, 5, 5, 16];
        m5.extend_from_slice(&[0; 16]);
        let m6 = tlv_items(&post_tlv(&mut stream, &m5)?)?;
        if m6.get(&6) != Some(&vec![6]) || !m6.contains_key(&7) {
            return Err("failed session continued".into());
        }
        Ok(())
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    let count = store.count_pairings().await.unwrap();
    drop(server);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
    result.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn real_http_pair_setup_authenticates_both_sides_and_persists_identity() {
    exercise_authenticated_lifecycle("127.0.0.1:0").await;
}

#[tokio::test]
async fn ipv6_pairing_encrypted_reads_restart_and_revocation() {
    exercise_authenticated_lifecycle("[::1]:0").await;
}

async fn exercise_authenticated_lifecycle(bind: &str) {
    use aead::{AeadInPlace, KeyInit, array::Array};
    use chacha20poly1305::ChaCha20Poly1305;
    use ed25519_dalek::Signer;
    use rand::{Rng, SeedableRng};
    use sha2::{Digest, Sha512};
    use srp::{client::SrpClient, groups::G_3072};
    let directory =
        std::env::temp_dir().join(format!("homesense-http-positive-{}", Uuid::new_v4()));
    let reservation = TcpListener::bind(bind).unwrap();
    let address = reservation.local_addr().unwrap();
    let mut store = IdentityStore::open(&directory).await.unwrap();
    let config = Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let pin = config.pin.to_string();
    let accessory_key = config.device_ed25519_keypair.verifying_key();
    let accessory_id = config.device_id.to_string();
    store.save_config(&config).await.unwrap();
    let server = IpServer::new_loopback(config, store.clone()).await.unwrap();
    use hap::{
        accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
        server::{AccessoryHandle, WriteBatchHandler, WriteStatus},
    };
    use std::sync::Arc;
    struct Reject;
    impl WriteBatchHandler for Reject {
        fn write(&self, _: Uuid, _: &[u8]) -> Result<(), WriteStatus> {
            Err(WriteStatus::InsufficientPrivileges)
        }
    }
    let mut graph = Vec::new();
    for aid in 1..=3 {
        let handle: AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(
            LightbulbAccessory::new(
                aid,
                AccessoryInformation {
                    name: format!("Study Light {aid}"),
                    ..Default::default()
                },
            )
            .unwrap(),
        )));
        graph.push(handle);
    }
    server
        .install_startup_graph(graph.clone(), Arc::new(Reject))
        .await
        .unwrap();
    drop(reservation);
    let running = server.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let result = tokio::task::spawn_blocking(move || {
        request(
            address,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let m2 = tlv_items(&post_tlv(&mut stream, &[6, 1, 1, 0, 1, 0]).unwrap()).unwrap();
        let salt = &m2[&2];
        let mut ephemeral = [0; 64];
        rand::rngs::StdRng::from_rng(&mut rand::rng()).fill_bytes(&mut ephemeral);
        let client = SrpClient::<sha2_srp::Sha512>::new(&G_3072);
        let public = client.compute_public_ephemeral(&ephemeral);
        // The HAP candidate uses unpadded g in the proof transcript, unlike
        // srp 0.6's generic proof helper. Derive the client
        // secret with SRP, then build and verify the wire transcript here.
        let key = client
            .process_reply(&ephemeral, b"Pair-Setup", pin.as_bytes(), salt, &m2[&3])
            .unwrap()
            .key()
            .to_vec();
        let key = Sha512::digest(&key).to_vec();
        let hn = Sha512::digest(&G_3072.n.to_bytes_be());
        let hg = Sha512::digest(&G_3072.g.to_bytes_be());
        let xor: Vec<u8> = hn.iter().zip(hg.iter()).map(|(n, g)| n ^ g).collect();
        let mut transcript = Sha512::new();
        transcript.update(&xor);
        transcript.update(Sha512::digest(b"Pair-Setup"));
        transcript.update(salt);
        transcript.update(&public);
        transcript.update(&m2[&3]);
        transcript.update(&key);
        let proof = transcript.finalize().to_vec();
        let mut server_proof = Sha512::new();
        server_proof.update(&public);
        server_proof.update(&proof);
        server_proof.update(&key);
        let server_proof = server_proof.finalize();

        let m3 = encode_tlv(&[(6, &[3]), (3, &public), (4, &proof)]);
        let m4 = tlv_items(&post_tlv(&mut stream, &m3).unwrap()).unwrap();
        assert_eq!(m4[&6], vec![4]);
        assert_eq!(m4[&4], server_proof.as_slice());
        let id = Uuid::new_v4();
        let signing = ed25519_dalek::SigningKey::generate(&mut rand::rngs::StdRng::from_rng(
            &mut rand::rng(),
        ));
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
            id.to_string().to_uppercase().as_bytes(),
            signing.verifying_key().as_bytes(),
        ]
        .concat();
        let mut data = encode_tlv(&[
            (1, id.to_string().to_uppercase().as_bytes()),
            (3, signing.verifying_key().as_bytes()),
            (10, &signing.sign(&signed).to_bytes()),
        ]);
        let encryption_key = derive(b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
        let cipher = ChaCha20Poly1305::new(Array::from_slice(&encryption_key));
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(b"PS-Msg05");
        let tag = cipher
            .encrypt_in_place_detached(Array::from_slice(&nonce), &[], &mut data)
            .unwrap();
        data.extend(tag);
        let m5 = encode_tlv(&[(6, &[5]), (5, &data)]);
        let m6 = tlv_items(&post_tlv(&mut stream, &m5).unwrap()).unwrap();
        assert_eq!(m6[&6], vec![6]);
        let data = &m6[&5];
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
        let sub = tlv_items(&plaintext).unwrap();
        assert_eq!(sub[&1], accessory_id.as_bytes());
        assert_eq!(sub[&3], accessory_key.as_bytes());
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
                &ed25519_dalek::Signature::from_slice(&sub[&10]).unwrap(),
            )
            .unwrap();

        verify_and_read(&mut stream, id, &signing, &accessory_id, &accessory_key);
        (id, signing, accessory_id, accessory_key)
    })
    .await;
    task.abort();
    let _ = task.await;
    drop(server);
    drop(store);
    let reopened = IdentityStore::open(&directory).await.unwrap();
    let (id, signing, accessory_id, accessory_key) = result.unwrap();
    let pairing = reopened.load_pairing(&id).await.unwrap();
    assert_eq!(reopened.count_pairings().await.unwrap(), 1);
    assert_eq!(pairing.wire_identifier(), id.to_string().to_uppercase());
    assert_eq!(pairing.public_key, signing.verifying_key().to_bytes());
    assert_eq!(pairing.permissions, hap::pairing::Permissions::Admin);
    let persisted_config = reopened.load_config().await.unwrap();
    let configuration_number = persisted_config.configuration_number;
    assert_eq!(persisted_config.device_id.to_string(), accessory_id);
    assert_eq!(
        persisted_config.device_ed25519_keypair.verifying_key(),
        accessory_key
    );
    let restarted = IpServer::new_loopback(persisted_config, reopened.clone())
        .await
        .unwrap();
    restarted
        .install_startup_graph(graph, Arc::new(Reject))
        .await
        .unwrap();
    assert_eq!(
        reopened.load_config().await.unwrap().configuration_number,
        configuration_number
    );
    let running = restarted.clone();
    let task = tokio::spawn(async move { running.run_handle().await });
    let reverified = tokio::task::spawn_blocking(move || {
        request(
            address,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        // Reconnect with the original controller key, without another Pair Setup.
        for attempt in 0..2 {
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut session = verify_and_read(&mut stream, id, &signing, &accessory_id, &accessory_key);
            if attempt == 1 {
                let body = encode_tlv(&[(6, &[1]), (0, &[4]), (1, id.to_string().to_uppercase().as_bytes())]);
                let mut request = format!("POST /pairings HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/pairing+tlv8\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
                request.extend_from_slice(&body);
                let (headers, response) = session.request(&mut stream, &request);
                assert!(headers.starts_with("HTTP/1.1 200"));
                let values = tlv_items(&response).unwrap();
                assert_eq!(values.get(&6), Some(&vec![2]));
                assert!(!values.contains_key(&7));
                let (headers, _) = session.request(&mut stream, b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");
                assert!(headers.starts_with("HTTP/1.1 401"), "revoked session remained authorized: {headers}");
            }
        }
    })
    .await;
    task.abort();
    let _ = task.await;
    assert_eq!(reopened.count_pairings().await.unwrap(), 0);
    assert_eq!(reopened.load_config().await.unwrap().configuration_number, configuration_number);
    drop(restarted);
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
    reverified.unwrap();
}

fn verify_and_read(
    stream: &mut TcpStream,
    id: Uuid,
    signing: &ed25519_dalek::SigningKey,
    accessory_id: &str,
    accessory_key: &ed25519_dalek::VerifyingKey,
) -> support::Session {
    let mut session = support::verify(stream, id, signing, accessory_id, accessory_key);
    // Exercise the same list/add/remove sequence used by home hubs. Every
    // response is authenticated and TLV errors must not hide behind HTTP 200.
    fn pairings(stream: &mut TcpStream, session: &mut support::Session, body: &[u8]) -> Vec<u8> {
        let mut request = format!("POST /pairings HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/pairing+tlv8\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        request.extend_from_slice(body);
        let (headers, response) = session.request(stream, &request);
        assert!(headers.starts_with("HTTP/1.1 200"));
        let values = tlv_items(&response).unwrap();
        assert_eq!(values.get(&6), Some(&vec![2]));
        assert!(!values.contains_key(&7), "pairing error: {values:?}");
        response
    }
    let list = pairings(stream, &mut session, &encode_tlv(&[(6, &[1]), (0, &[5])]));
    assert_eq!(tlv_items(&list).unwrap()[&1], id.to_string().to_uppercase().as_bytes());
    let second = Uuid::new_v4().to_string().to_uppercase();
    pairings(stream, &mut session, &encode_tlv(&[
        (6, &[1]), (0, &[3]), (1, second.as_bytes()),
        (3, signing.verifying_key().as_bytes()), (11, &[1]),
    ]));
    let list = pairings(stream, &mut session, &encode_tlv(&[(6, &[1]), (0, &[5])]));
    // Inspect each TLV field without collapsing repeated identifiers.
    let mut identifiers = Vec::new();
    let mut offset = 0;
    while offset < list.len() {
        let length = list[offset + 1] as usize;
        if list[offset] == 1 { identifiers.push(list[offset + 2..offset + 2 + length].to_vec()); }
        offset += 2 + length;
    }
    assert_eq!(identifiers.len(), 2);
    assert!(identifiers.contains(&second.as_bytes().to_vec()));
    assert!(identifiers.contains(&id.to_string().to_uppercase().into_bytes()));
    pairings(stream, &mut session, &encode_tlv(&[(6, &[1]), (0, &[4]), (1, second.as_bytes())]));
    let list = pairings(stream, &mut session, &encode_tlv(&[(6, &[1]), (0, &[5])]));
    assert_eq!(tlv_items(&list).unwrap()[&1], id.to_string().to_uppercase().as_bytes());

    let (headers, body) = session.request(
        stream,
        b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(headers.starts_with("HTTP/1.1 200"));
    assert!(body.len() > 1024, "fixture must span encrypted records");
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let accessories = body["accessories"].as_array().unwrap();
    assert_eq!(accessories.len(), 3);
    for (index, accessory) in accessories.iter().enumerate() {
        assert_eq!(accessory["aid"], index + 1);
        let services = accessory["services"].as_array().unwrap();
        assert!(services.iter().any(|service| service["type"] == "43"));
        assert!(
            services
                .iter()
                .flat_map(|service| service["characteristics"].as_array().unwrap())
                .any(|characteristic| characteristic["value"]
                    == format!("Study Light {}", index + 1))
        );
    }
    session
}
