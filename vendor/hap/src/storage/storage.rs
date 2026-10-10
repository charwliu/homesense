use async_trait::async_trait;
use uuid::Uuid;

use crate::{pairing::Pairing, Config, Result};

/// [`Storage`](Storage) is implemented by the persistent data storage methods HAP supports. Currently, that's just
/// [`FileStorage`](crate::storage::FileStorage).
#[async_trait]
pub trait Storage: Send + Sync {
    /// Loads the [`Config`](Config) from the [`Storage`](Storage).
    async fn load_config(&self) -> Result<Config>;
    /// Saves the [`Config`](Config) to the [`Storage`](Storage).
    async fn save_config(&mut self, config: &Config) -> Result<()>;
    /// Deletes the [`Config`](Config) from the [`Storage`](Storage).
    async fn delete_config(&mut self) -> Result<()>;
    /// Loads the AID cache from the [`Storage`](Storage).
    async fn load_aid_cache(&self) -> Result<Vec<u64>>;
    /// Saves the AID cache to the [`Storage`](Storage).
    async fn save_aid_cache(&mut self, aid_cache: &[u64]) -> Result<()>;
    /// Deletes the AID cache from the [`Storage`](Storage).
    async fn delete_aid_cache(&mut self) -> Result<()>;
    /// Loads a [`Pairing`](Pairing) from the [`Storage`](Storage).
    async fn load_pairing(&self, id: &Uuid) -> Result<Pairing>;
    /// Saves a [`Pairing`](Pairing) to the [`Storage`](Storage).
    async fn save_pairing(&mut self, pairing: &Pairing) -> Result<()>;
    /// Deletes the [`Pairing`](Pairing) from the [`Storage`](Storage).
    async fn delete_pairing(&mut self, id: &Uuid) -> Result<()>;
    /// Atomically authorize and add/update a pairing without replacing its key.
    /// Demoting the last administrator revokes every pairing; return revoked IDs.
    async fn save_pairing_authorized(
        &mut self,
        _actor: &Uuid,
        _pairing: &Pairing,
        _max_peers: Option<usize>,
    ) -> Result<Vec<Uuid>> {
        Err(crate::Error::Storage)
    }
    /// Atomically authorize an administrator and remove a controller. Removing
    /// the last administrator also removes every remaining controller. Returns
    /// revoked IDs only after commit. Backends without transactions fail closed.
    async fn remove_pairing_authorized(
        &mut self,
        _actor: &Uuid,
        _target: &Uuid,
    ) -> Result<Vec<Uuid>> {
        Err(crate::Error::Storage)
    }
    /// Loads all [`Pairing`](Pairing)s from the [`Storage`](Storage).
    async fn list_pairings(&self) -> Result<Vec<Pairing>>;
    /// Returns the count of [`Pairing`](Pairing)s stored on the [`Storage`](Storage).
    async fn count_pairings(&self) -> Result<usize>;
    /// Loads arbitrary bytes from the [`Storage`](Storage).
    async fn load_bytes(&self, key: &str) -> Result<Vec<u8>>;
    /// Saves arbitrary bytes to the [`Storage`](Storage).
    async fn save_bytes(&mut self, key: &str, value: &[u8]) -> Result<()>;
    /// Deletes a set of arbitrary bytes from the [`Storage`](Storage).
    async fn delete_bytes(&mut self, key: &str) -> Result<()>;
}
