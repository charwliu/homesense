use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::{accessory::HapAccessory, pointer, Result};
pub use ip::IpServer;

mod ip;

/// [`Server`](Server) is implemented by the transport methods this crate supports. Currently, that's just
/// [`IpServer`](IpServer).
#[async_trait]
pub trait Server {
    /// Returns a [`BoxFuture`](BoxFuture) run handle to the server that can be passed to an executor.
    fn run_handle(&self) -> BoxFuture<Result<()>>;
    /// Returns a pointer to the [`Config`](crate::Config) of the server.
    fn config_pointer(&self) -> pointer::Config;
    /// Returns a pointer to the [`Storage`](crate::storage::Storage) of the server.
    fn storage_pointer(&self) -> pointer::Storage;
    /// Adds an accessory to the server and returns a pointer to it.
    async fn add_accessory<A: HapAccessory + 'static>(
        &self,
        accessory: A,
    ) -> Result<pointer::Accessory>;
    /// Takes a pointer to an accessory and removes it from the server.
    async fn remove_accessory(&self, accessory: &pointer::Accessory) -> Result<()>;
    // /// Every accessory must support a manufacturer-defined mechanism to restore itself to a “factory reset” state
    // where /// all pairing information is erased and restored to factory default settings. This method is doing
    // just that. async fn factory_reset(&mut self) -> Result<()>;
}

/// HAP status returned by a grouped write adapter.
pub use crate::transport::http::Status as WriteStatus;

/// One authenticated request, accepted atomically by the application.
/// Implementations must be bounded and nonblocking, must not acquire HAP locks,
/// and must not change state on error. Success accepts intent, not observed state.
/// Session authentication is supplied by the server; dispatch authorization and
/// durable acceptance remain the application's responsibility.
pub trait WriteBatchHandler: Send + Sync {
    fn write(&self, controller: uuid::Uuid, body: &[u8]) -> std::result::Result<(), WriteStatus>;
}

/// Shared accessory handle returned by Server::add_accessory.
pub type AccessoryHandle = crate::pointer::Accessory;
pub use crate::event::{Event as ServerEvent, EventEmitter};
