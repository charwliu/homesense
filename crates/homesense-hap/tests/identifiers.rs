use hap::{
    Config,
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_core::DeviceId;
use homesense_hap::{
    identifiers::{AccessoryDefinition, AccessoryKey},
    storage::IdentityStore,
};
use std::path::PathBuf;
use uuid::Uuid;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("homesense-ids-{}", Uuid::new_v4())))
    }
    async fn store(&self) -> IdentityStore {
        IdentityStore::open(&self.0).await.unwrap()
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.0.join("identity.sqlite")).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn key(id: &str) -> AccessoryKey {
    AccessoryKey::Device(DeviceId::new(id).unwrap())
}
fn definition(id: &str, resources: &[&str]) -> AccessoryDefinition {
    AccessoryDefinition {
        key: key(id),
        resources: resources.iter().map(|s| s.to_string()).collect(),
    }
}
async fn initialized(root: &Root) -> IdentityStore {
    let mut store = root.store().await;
    store.save_config(&Config::default()).await.unwrap();
    store
}
#[tokio::test]
async fn reboot_reorder_removal_and_extension_preserve_identifiers() {
    let root = Root::new();
    let store = initialized(&root).await;
    let a = definition("a", &["service:light", "light:on"]);
    let b = definition("b", &["service:light", "light:on"]);
    let original = store
        .allocate_identifiers(vec![
            b.clone(),
            a.clone(),
            AccessoryDefinition {
                key: AccessoryKey::Bridge,
                resources: vec!["service:info".into()],
            },
        ])
        .await
        .unwrap();
    assert_eq!(original[&AccessoryKey::Bridge].aid, 1);
    assert_eq!(original[&key("a")].aid, 2);
    assert_eq!(original[&key("b")].aid, 3);
    drop(store);
    let store = root.store().await;
    let newer = store
        .allocate_identifiers(vec![
            definition("c", &["light:on"]),
            definition("a", &["light:brightness", "light:on"]),
        ])
        .await
        .unwrap();
    assert_eq!(newer[&key("c")].aid, 4);
    assert_eq!(
        newer[&key("a")].instances["light:on"],
        original[&key("a")].instances["light:on"]
    );
    assert!(
        newer[&key("a")].instances["light:brightness"]
            > *original[&key("a")].instances.values().max().unwrap()
    );
    let restored = store.allocate_identifiers(vec![b, a]).await.unwrap();
    assert_eq!(restored[&key("b")], original[&key("b")]);
    assert_eq!(restored[&key("a")], original[&key("a")]);
    let mut store = store;
    assert!(store.delete_config().await.is_err());
}
#[tokio::test]
async fn concurrent_connections_share_one_identifier_space() {
    let root = Root::new();
    let one = initialized(&root).await;
    let two = root.store().await;
    let (a, b) = tokio::join!(
        one.allocate_identifiers(vec![definition("same", &["on"]), definition("a", &["on"])]),
        two.allocate_identifiers(vec![definition("same", &["on"]), definition("b", &["on"])])
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a[&key("same")], b[&key("same")]);
    assert_ne!(a[&key("a")].aid, b[&key("b")].aid);
}
#[tokio::test]
async fn failed_allocation_rolls_back_all_new_resources_and_aids() {
    let root = Root::new();
    let store = initialized(&root).await;
    let full = AccessoryDefinition {
        key: key("z"),
        resources: (0..128).map(|n| format!("r{n}")).collect(),
    };
    store.allocate_identifiers(vec![full]).await.unwrap();
    assert!(
        store
            .allocate_identifiers(vec![
                definition("a", &["on"]),
                definition("z", &["overflow"])
            ])
            .await
            .is_err()
    );
    let next = store
        .allocate_identifiers(vec![definition("b", &["on"])])
        .await
        .unwrap();
    assert_eq!(next[&key("b")].aid, 3);
    assert_eq!(
        root.db()
            .query_row(
                "SELECT count(*) FROM accessory_ids WHERE device='device:a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert!(
        store
            .allocate_identifiers(vec![definition("x", &["on", "on"])])
            .await
            .is_err()
    );
}
#[tokio::test]
async fn missing_identity_or_mapping_never_recreates_existing_pairing_identifiers() {
    let root = Root::new();
    let mut store = root.store().await;
    assert!(
        store
            .allocate_identifiers(vec![definition("a", &["on"])])
            .await
            .is_err()
    );
    store.save_config(&Config::default()).await.unwrap();
    let pairing = Pairing::new(Uuid::new_v4(), Permissions::Admin, [7; 32]);
    store.save_pairing(&pairing).await.unwrap();
    assert!(
        store
            .allocate_identifiers(vec![definition("a", &["on"])])
            .await
            .is_err()
    );
    store.delete_pairing(&pairing.id).await.unwrap();
    store.save_aid_cache(&[1, 2]).await.unwrap();
    assert!(
        store
            .allocate_identifiers(vec![definition("a", &["on"])])
            .await
            .is_err()
    );
    store.delete_aid_cache().await.unwrap();
    store
        .allocate_identifiers(vec![definition("a", &["on"])])
        .await
        .unwrap();
    root.db()
        .execute_batch("DELETE FROM instance_ids; DELETE FROM accessory_ids;")
        .unwrap();
    assert!(
        store
            .allocate_identifiers(vec![definition("a", &["on"])])
            .await
            .is_err()
    );
}
#[tokio::test]
async fn schema_one_migration_preserves_existing_identity_and_pairing() {
    let root = Root::new();
    let mut store = initialized(&root).await;
    let config = store.load_config().await.unwrap();
    let pairing = Pairing::new(Uuid::new_v4(), Permissions::Admin, [8; 32]);
    store.save_pairing(&pairing).await.unwrap();
    drop(store);
    root.db()
        .execute_batch("DROP TABLE instance_ids; DROP TABLE accessory_ids; PRAGMA user_version=1;")
        .unwrap();
    let store = root.store().await;
    assert_eq!(
        store.load_config().await.unwrap().device_id,
        config.device_id
    );
    assert_eq!(store.load_pairing(&pairing.id).await.unwrap(), pairing);
    assert_eq!(
        root.db()
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(
        store
            .allocate_identifiers(vec![definition("a", &["on"])])
            .await
            .is_err()
    );
}
