use hap::{
    Config,
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_hap::storage::IdentityStore;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use uuid::Uuid;
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "homesense-hap-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn identity(&self) -> PathBuf {
        self.0.join("identity")
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn identity_pairing_and_aids_survive_reopen() {
    let root = Root::new();
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    assert!(!store.has_config().await.unwrap());
    assert!(store.load_config().await.is_err());
    let mut config = Config::default();
    let identity = config.device_ed25519_keypair.to_keypair_bytes();
    store.save_config(&config).await.unwrap();
    config.name = "HomeSense renamed".into();
    store.save_config(&config).await.unwrap();
    let pairing = Pairing::new(Uuid::new_v4(), Permissions::Admin, [7; 32]);
    store.save_pairing(&pairing).await.unwrap();
    store.save_aid_cache(&[1, 2, 5]).await.unwrap();
    drop(store);
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    let restored = store.load_config().await.unwrap();
    assert_eq!(restored.device_ed25519_keypair.to_keypair_bytes(), identity);
    assert_eq!(restored.device_id, config.device_id);
    assert_eq!(restored.pin, config.pin);
    assert_eq!(restored.name, config.name);
    assert_eq!(store.load_aid_cache().await.unwrap(), [1, 2, 5]);
    assert_eq!(store.load_pairing(&pairing.id).await.unwrap(), pairing);
    assert_eq!(store.count_pairings().await.unwrap(), 1);
    assert!(store.delete_config().await.is_err());
    store.delete_pairing(&pairing.id).await.unwrap();
    assert_eq!(store.count_pairings().await.unwrap(), 0);
}
#[tokio::test]
async fn changing_identity_or_corrupt_storage_never_silently_resets_pairings() {
    let root = Root::new();
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    let original = Config::default();
    store.save_config(&original).await.unwrap();
    assert!(store.save_config(&Config::default()).await.is_err());
    assert_eq!(
        store.load_config().await.unwrap().device_id,
        original.device_id
    );
    drop(store);
    let db = rusqlite::Connection::open(root.identity().join("identity.sqlite")).unwrap();
    db.execute(
        "UPDATE entries SET value=?1 WHERE key='config'",
        [b"broken".as_slice()],
    )
    .unwrap();
    drop(db);
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    assert!(store.has_config().await.unwrap());
    assert!(store.load_config().await.is_err());
    assert!(store.save_config(&Config::default()).await.is_err());
}
#[tokio::test]
async fn namespaces_bounds_and_pairing_limit_are_enforced() {
    let root = Root::new();
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    let config = Config::default();
    store.save_config(&config).await.unwrap();
    store.save_bytes("config", b"unrelated").await.unwrap();
    assert_eq!(
        store.load_config().await.unwrap().device_id,
        config.device_id
    );
    assert!(store.save_bytes("../config", b"bad").await.is_err());
    assert!(store.save_bytes("large", &vec![0; 65537]).await.is_err());
    for i in 1..=32 {
        store
            .save_pairing(&Pairing::new(
                Uuid::from_u128(i),
                Permissions::User,
                [1; 32],
            ))
            .await
            .unwrap();
    }
    assert!(
        store
            .save_pairing(&Pairing::new(
                Uuid::from_u128(33),
                Permissions::User,
                [1; 32]
            ))
            .await
            .is_err()
    );
    store
        .save_pairing(&Pairing::new(
            Uuid::from_u128(1),
            Permissions::Admin,
            [1; 32],
        ))
        .await
        .unwrap();
    assert_eq!(store.list_pairings().await.unwrap().len(), 32);
}
#[cfg(unix)]
#[tokio::test]
async fn storage_permissions_are_private_and_insecure_existing_directory_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let _store = IdentityStore::open(root.identity()).await.unwrap();
    assert_eq!(
        std::fs::metadata(root.identity())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.identity().join("identity.sqlite"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let insecure = root.0.join("insecure");
    std::fs::create_dir(&insecure).unwrap();
    std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(IdentityStore::open(&insecure).await.is_err());
    assert!(!insecure.join("identity.sqlite").exists());
    let linked = root.0.join("linked");
    std::os::unix::fs::symlink(root.identity(), &linked).unwrap();
    assert!(IdentityStore::open(linked).await.is_err());
}
#[tokio::test]
async fn busy_database_does_not_acknowledge_lost_write() {
    let root = Root::new();
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    store.save_bytes("value", b"old").await.unwrap();
    let db = rusqlite::Connection::open(root.identity().join("identity.sqlite")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(store.save_bytes("value", b"new").await.is_err());
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.load_bytes("value").await.unwrap(), b"old");
}

#[tokio::test]
async fn missing_identity_with_existing_pairing_does_not_allow_reinitialization() {
    let root = Root::new();
    let mut store = IdentityStore::open(root.identity()).await.unwrap();
    store
        .save_pairing(&Pairing::new(Uuid::new_v4(), Permissions::Admin, [1; 32]))
        .await
        .unwrap();
    assert!(!store.has_config().await.unwrap());
    assert!(store.save_config(&Config::default()).await.is_err());
    assert_eq!(store.count_pairings().await.unwrap(), 1);
}
