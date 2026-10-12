//! Test controller for the complete HAP SRP setup transcript.
use super::support::{encode_tlv, post_tlv_path, tlv_items};
use std::net::TcpStream;
use uuid::Uuid;
pub fn setup(
    stream: &mut TcpStream,
    pin: &str,
    id: Uuid,
    signing: &ed25519_dalek::SigningKey,
    accessory_id: &str,
    accessory_key: &ed25519_dalek::VerifyingKey,
) {
    use chacha20poly1305::{
        ChaCha20Poly1305,
        aead::{AeadInOut, KeyInit},
    };
    use ed25519_dalek::Signer;
    use rand::{Rng, SeedableRng};
    use sha2::{Digest, Sha512};
    use srp::{client::SrpClient, groups::G_3072};
    let m2 =
        tlv_items(&post_tlv_path(stream, "/pair-setup", &[6, 1, 1, 0, 1, 0]).unwrap()).unwrap();
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
    let hn = Sha512::digest(G_3072.n.to_bytes_be());
    let hg = Sha512::digest(G_3072.g.to_bytes_be());
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
    let m4 = tlv_items(&post_tlv_path(stream, "/pair-setup", &m3).unwrap()).unwrap();
    assert_eq!(m4[&6], vec![4]);
    assert_eq!(m4[&4], server_proof.as_slice());
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
    let cipher = ChaCha20Poly1305::new((&encryption_key).into());
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(b"PS-Msg05");
    let tag = cipher
        .encrypt_inout_detached((&nonce).into(), &[], data.as_mut_slice().into())
        .unwrap();
    data.extend(tag);
    let m5 = encode_tlv(&[(6, &[5]), (5, &data)]);
    let m6 = tlv_items(&post_tlv_path(stream, "/pair-setup", &m5).unwrap()).unwrap();
    assert_eq!(m6[&6], vec![6]);
    let data = &m6[&5];
    let mut plaintext = data[..data.len() - 16].to_vec();
    nonce[4..].copy_from_slice(b"PS-Msg06");
    cipher
        .decrypt_inout_detached(
            (&nonce).into(),
            &[],
            plaintext.as_mut_slice().into(),
            data[data.len() - 16..].try_into().unwrap(),
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
}
