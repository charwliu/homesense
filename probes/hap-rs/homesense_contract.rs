//! Characterization tests for the fixed upstream candidate, not acceptance tests.
use rand::SeedableRng;
use hap::characteristic::{Characteristic, brightness::BrightnessCharacteristic, HapCharacteristic};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[tokio::test]
async fn read_callback_does_not_invoke_write_callback() {
    let updates = Arc::new(AtomicUsize::new(0)); let count = updates.clone();
    let mut c = Characteristic::<bool>::default();
    c.on_read(Some(|| Ok(Some(true))));
    c.on_update(Some(move |_: &bool, _: &bool| { count.fetch_add(1, Ordering::SeqCst); Ok(()) }));
    assert!(c.get_value().await.unwrap());
    // Observed reads must never submit device commands.
    assert_eq!(updates.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn accepted_write_waits_for_observation_before_changing_cache() {
    let mut c = Characteristic::<bool>::default();
    c.on_update(Some(|_: &bool, _: &bool| Ok(())));
    c.set_value(true).await.unwrap();
    assert!(!c.get_value().await.unwrap());
    c.set_observed_value(true).await.unwrap();
    assert!(c.get_value().await.unwrap());
}
#[tokio::test]
async fn upstream_failed_callback_preserves_cached_value() {
    let mut c = Characteristic::<bool>::default();
    c.on_update(Some(|_: &bool, _: &bool| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err(std::io::Error::other("admission rejected").into())
    }));
    assert!(c.set_value(true).await.is_err()); assert!(!c.get_value().await.unwrap());
}
#[tokio::test]
async fn upstream_brightness_metadata_does_not_enforce_bounds() {
    let mut c = BrightnessCharacteristic::new(10, 1);
    c.set_value(serde_json::json!(101)).await.unwrap();
    assert_eq!(c.get_value().await.unwrap(), serde_json::json!(101));
}

fn hex<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&value[2*i..2*i+2], 16).unwrap())
}
#[test]
fn ed25519_rfc8032_vector_and_changed_message_rejection() {
    use signature::Signer;
    // RFC 8032 section 7.1 TEST 1: public, non-secret test vector.
    let secret = hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public = hex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let expected = hex::<64>("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
    let key = ed25519_dalek::SigningKey::from_bytes(&secret);
    assert_eq!(key.verifying_key().to_bytes(), public);
    let signature = key.sign(b"");
    assert_eq!(signature.to_bytes(), expected);
    key.verifying_key().verify_strict(b"", &signature).unwrap();
    assert!(key.verifying_key().verify_strict(b"changed", &signature).is_err());
}
#[test]
fn x25519_peers_derive_identical_secret() {
    let a = x25519_dalek::EphemeralSecret::random_from_rng(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
    let b = x25519_dalek::EphemeralSecret::random_from_rng(&mut rand::rngs::StdRng::from_rng(&mut rand::rng()));
    let ap = x25519_dalek::PublicKey::from(&a);
    let bp = x25519_dalek::PublicKey::from(&b);
    assert_eq!(a.diffie_hellman(&bp).as_bytes(), b.diffie_hellman(&ap).as_bytes());
}
#[test]
fn identity_roundtrip_preserves_key_and_rejects_mismatched_public_half() {
    let config = hap::Config::default();
    let mut json = serde_json::to_value(&config).unwrap();
    let restored: hap::Config = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(config.device_ed25519_keypair.to_keypair_bytes(), restored.device_ed25519_keypair.to_keypair_bytes());
    let bytes = json["device_ed25519_keypair"].as_array_mut().unwrap();
    assert_eq!(bytes.len(), 64);
    bytes[63] = serde_json::json!(bytes[63].as_u64().unwrap() ^ 1);
    assert!(serde_json::from_value::<hap::Config>(json).is_err());
}

#[tokio::test]
async fn observed_feedback_bypasses_control_callback_on_generated_characteristic() {
    use hap::characteristic::CharacteristicCallbacks;
    let count = Arc::new(AtomicUsize::new(0)); let calls = count.clone();
    let mut c = BrightnessCharacteristic::new(10, 1);
    c.on_update(Some(move |_: &i32, _: &i32| { calls.fetch_add(1, Ordering::SeqCst); Ok(()) }));
    c.set_value(serde_json::json!(73)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(c.get_value().await.unwrap(), serde_json::json!(0));
    c.set_observed_value(serde_json::json!(73)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(c.get_value().await.unwrap(), serde_json::json!(73));
}
