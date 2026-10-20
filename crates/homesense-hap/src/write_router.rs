//! Dispatch a whole authenticated request to exactly one atomic owner. Mixed
//! light/scene batches are rejected before either owner observes the request.
use hap::server::{WriteBatchHandler, WriteStatus};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uuid::Uuid;
pub type RouteGroup = (Vec<(u64,u64)>, Arc<dyn WriteBatchHandler>);
pub struct WriteRouter {
    routes: BTreeMap<(u64, u64), usize>,
    owners: Vec<Arc<dyn WriteBatchHandler>>,
}
#[derive(Deserialize)]
struct Envelope {
    characteristics: Vec<Address>,
}
#[derive(Deserialize)]
struct Address {
    aid: u64,
    iid: u64,
}
impl WriteRouter {
    pub fn new(
        groups: Vec<RouteGroup>,
    ) -> Result<Self, WriteStatus> {
        // A legacy bridge may have no remaining published accessories after
        // light/panel removal. It still needs to serve its paired identity.
        if groups.len() > 16 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let mut routes = BTreeMap::new();
        let mut owners = vec![];
        for (addresses, owner) in groups {
            let index = owners.len();
            if addresses.is_empty() {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            for address in addresses {
                if address.0 <= 1
                    || address.1 == 0
                    || routes.insert(address, index).is_some()
                    || routes.len() > 16384
                {
                    return Err(WriteStatus::InvalidValueInRequest);
                }
            }
            owners.push(owner);
        }
        Ok(Self { routes, owners })
    }
    fn owner(&self, body: &[u8]) -> Result<&Arc<dyn WriteBatchHandler>, WriteStatus> {
        if body.len() > 65536 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let envelope: Envelope =
            serde_json::from_slice(body).map_err(|_| WriteStatus::InvalidValueInRequest)?;
        if envelope.characteristics.is_empty() || envelope.characteristics.len() > 128 {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        let mut selected = None;
        let mut seen = BTreeSet::new();
        for address in envelope.characteristics {
            let key = (address.aid, address.iid);
            if !seen.insert(key) {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            let route = *self
                .routes
                .get(&key)
                .ok_or(WriteStatus::ResourceDoesNotExist)?;
            if selected.is_some_and(|i| i != route) {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            selected = Some(route);
        }
        Ok(&self.owners[selected.ok_or(WriteStatus::InvalidValueInRequest)?])
    }
}
impl WriteBatchHandler for WriteRouter {
    fn write(&self, _: Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        Err(WriteStatus::ServiceCommunicationFailure)
    }
    fn write_async<'a>(
        &'a self,
        controller: Uuid,
        body: &'a [u8],
    ) -> hap::futures::future::BoxFuture<'a, Result<(), WriteStatus>> {
        Box::pin(async move { self.owner(body)?.write_async(controller, body).await })
    }
}
