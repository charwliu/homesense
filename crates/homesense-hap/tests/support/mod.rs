//! Test-only HAP controller: actual HTTP and encrypted records.
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{AeadInPlace, array::Array};
use rand::SeedableRng;
use std::{
    io::{Read, Write},
    net::TcpStream,
};
use uuid::Uuid;
pub fn post_tlv_path(stream: &mut TcpStream, path: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/pairing+tlv8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
        header.push(byte[0]);
        if header.len() > 8192 {
            return Err("header too large".into());
        }
    }
    let header = String::from_utf8(header).map_err(|e| e.to_string())?;
    if !header.starts_with("HTTP/1.1 200") {
        return Err(format!("unexpected response: {header}"));
    }
    let length = header
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>())
        })
        .ok_or("missing content-length")?
        .map_err(|e| e.to_string())?;
    if length > 65536 {
        return Err("body too large".into());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(body)
}
pub fn tlv_items(bytes: &[u8]) -> Result<std::collections::BTreeMap<u8, Vec<u8>>, String> {
    let mut result = std::collections::BTreeMap::<u8, Vec<u8>>::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes.len() - offset < 2 {
            return Err("truncated TLV header".into());
        }
        let kind = bytes[offset];
        let length = usize::from(bytes[offset + 1]);
        offset += 2;
        let value = bytes
            .get(offset..offset + length)
            .ok_or("truncated TLV value")?;
        result.entry(kind).or_default().extend_from_slice(value);
        offset += length;
    }
    Ok(result)
}
pub fn encode_tlv(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut result = Vec::new();
    for (kind, value) in items {
        for chunk in value.chunks(255) {
            result.extend_from_slice(&[*kind, chunk.len() as u8]);
            result.extend_from_slice(chunk);
        }
    }
    result
}

pub fn verify(
    stream: &mut TcpStream,
    id: Uuid,
    signing: &ed25519_dalek::SigningKey,
    accessory_id: &str,
    accessory_key: &ed25519_dalek::VerifyingKey,
) -> Session {
    use chacha20poly1305::ChaCha20Poly1305;
    use chacha20poly1305::aead::{AeadInPlace, KeyInit, array::Array};
    use ed25519_dalek::Signer;
    use x25519_dalek::{EphemeralSecret, PublicKey};
    let secret =
        EphemeralSecret::random_from_rng(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
    let public = PublicKey::from(&secret);
    let m1 = encode_tlv(&[(6, &[1]), (3, public.as_bytes())]);
    let m2 = tlv_items(&post_tlv_path(stream, "/pair-verify", &m1).unwrap()).unwrap();
    assert_eq!(m2[&6], [2]);
    let peer = PublicKey::from(<[u8; 32]>::try_from(m2[&3].as_slice()).unwrap());
    let shared = secret.diffie_hellman(&peer);
    assert!(shared.was_contributory());
    let derive = |salt: &[u8], info: &[u8]| {
        let mut key = [0; 32];
        hkdf::Hkdf::<sha2::Sha512>::new(Some(salt), shared.as_bytes())
            .expand(info, &mut key)
            .unwrap();
        key
    };
    let key = derive(b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info");
    let cipher = ChaCha20Poly1305::new(Array::from_slice(&key));
    let encrypted = &m2[&5];
    let mut plaintext = encrypted[..encrypted.len() - 16].to_vec();
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(b"PV-Msg02");
    cipher
        .decrypt_in_place_detached(
            Array::from_slice(&nonce),
            &[],
            &mut plaintext,
            Array::from_slice(&encrypted[encrypted.len() - 16..]),
        )
        .unwrap();
    let sub = tlv_items(&plaintext).unwrap();
    assert_eq!(sub[&1], accessory_id.as_bytes());
    let signed = [
        peer.as_bytes().as_slice(),
        accessory_id.as_bytes(),
        public.as_bytes().as_slice(),
    ]
    .concat();
    accessory_key
        .verify_strict(
            &signed,
            &ed25519_dalek::Signature::from_slice(&sub[&10]).unwrap(),
        )
        .unwrap();
    let claimed = id.to_string();
    let signed = [
        public.as_bytes().as_slice(),
        claimed.as_bytes(),
        peer.as_bytes().as_slice(),
    ]
    .concat();
    let mut data = encode_tlv(&[
        (1, claimed.as_bytes()),
        (10, &signing.sign(&signed).to_bytes()),
    ]);
    nonce[4..].copy_from_slice(b"PV-Msg03");
    let tag = cipher
        .encrypt_in_place_detached(Array::from_slice(&nonce), &[], &mut data)
        .unwrap();
    data.extend(tag);
    let m3 = encode_tlv(&[(6, &[3]), (5, &data)]);
    let m4 = tlv_items(&post_tlv_path(stream, "/pair-verify", &m3).unwrap()).unwrap();
    assert_eq!(m4[&6], [4]);
    assert!(!m4.contains_key(&7));
    let write_key = derive(b"Control-Salt", b"Control-Write-Encryption-Key");
    let read_key = derive(b"Control-Salt", b"Control-Read-Encryption-Key");
    let writer = ChaCha20Poly1305::new(Array::from_slice(&write_key));
    let reader = ChaCha20Poly1305::new(Array::from_slice(&read_key));
    Session {
        writer,
        reader,
        sent: 0,
        received: 0,
    }
}

pub struct Session {
    writer: ChaCha20Poly1305,
    reader: ChaCha20Poly1305,
    sent: u64,
    received: u64,
}
impl Session {
    pub fn request(&mut self, stream: &mut TcpStream, request: &[u8]) -> (String, Vec<u8>) {
        assert!(request.len() < 65536);
        for chunk in request.chunks(1024) {
            let mut data = chunk.to_vec();
            let length = (chunk.len() as u16).to_le_bytes();
            let mut nonce = [0; 12];
            nonce[4..].copy_from_slice(&self.sent.to_le_bytes());
            let tag = self
                .writer
                .encrypt_in_place_detached(Array::from_slice(&nonce), &length, &mut data)
                .unwrap();
            stream.write_all(&length).unwrap();
            stream.write_all(&data).unwrap();
            stream.write_all(&tag).unwrap();
            self.sent += 1;
        }
        let mut response = Vec::new();
        for _ in 0..128 {
            let mut length = [0; 2];
            stream.read_exact(&mut length).unwrap();
            let count = u16::from_le_bytes(length) as usize;
            assert!(count <= 1024);
            let mut data = vec![0; count];
            let mut tag = [0; 16];
            stream.read_exact(&mut data).unwrap();
            stream.read_exact(&mut tag).unwrap();
            let mut nonce = [0; 12];
            nonce[4..].copy_from_slice(&self.received.to_le_bytes());
            self.reader
                .decrypt_in_place_detached(
                    Array::from_slice(&nonce),
                    &length,
                    &mut data,
                    Array::from_slice(&tag),
                )
                .unwrap();
            self.received += 1;
            response.extend(data);
            if let Some(end) = response.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&response[..end]).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or_else(|| {
                        assert!(headers.starts_with("HTTP/1.1 204"));
                        0
                    });
                if response.len() >= end + 4 + length {
                    assert_eq!(response.len(), end + 4 + length);
                    return (headers.to_owned(), response[end + 4..].to_vec());
                }
            }
        }
        panic!("encrypted response exceeded record bound");
    }
}
