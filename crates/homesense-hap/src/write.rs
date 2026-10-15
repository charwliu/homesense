//! Native HAP HTTP grouped-write adapter. Runtime admission is supplied by the
//! owner and must be atomic; this adapter never writes a physical gateway.
use crate::requests::{Batch, LightRequests, RequestError};
use hap::server::{WriteBatchHandler, WriteStatus};
use homesense_core::{DeviceId, Percent};
use uuid::Uuid;

pub struct LightWriteHandler<R, S> {
    requests: LightRequests,
    restore: R,
    submit: S,
}
impl<R, S> LightWriteHandler<R, S> {
    pub fn new(requests: LightRequests, restore: R, submit: S) -> Self {
        Self {
            requests,
            restore,
            submit,
        }
    }
}
impl<R, S> WriteBatchHandler for LightWriteHandler<R, S>
where
    R: Fn(&DeviceId) -> Option<Percent> + Send + Sync,
    S: Fn(Batch) -> Result<(), WriteStatus> + Send + Sync,
{
    fn write(&self, controller: Uuid, body: &[u8]) -> Result<(), WriteStatus> {
        let batch = self
            .requests
            .normalize(controller, body, &self.restore)
            .map_err(|error| match error {
                RequestError::UnknownCharacteristic => WriteStatus::ResourceDoesNotExist,
                RequestError::MissingRestoreLevel => WriteStatus::ServiceCommunicationFailure,
                _ => WriteStatus::InvalidValueInRequest,
            })?;
        // Even subscription-only requests pass the owner for current authorization.
        // The HAP layer commits subscriptions only after successful admission.
        (self.submit)(batch)
    }
}

/// Asynchronous runtime adapter. No synchronous fallback can accept a write.
pub struct AsyncLightWriteHandler<R, S> {
    requests: LightRequests,
    restore: R,
    submit: S,
}
impl<R, S> AsyncLightWriteHandler<R, S> {
    pub fn new(requests: LightRequests, restore: R, submit: S) -> Self {
        Self {
            requests,
            restore,
            submit,
        }
    }
}
impl<R, S, F> WriteBatchHandler for AsyncLightWriteHandler<R, S>
where
    R: Fn(&DeviceId) -> Option<Percent> + Send + Sync,
    S: Fn(Batch) -> F + Send + Sync,
    F: std::future::Future<Output = Result<(), WriteStatus>> + Send,
{
    fn write(&self, _: Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        Err(WriteStatus::ServiceCommunicationFailure)
    }
    fn write_async<'a>(
        &'a self,
        controller: Uuid,
        body: &'a [u8],
    ) -> hap::futures::future::BoxFuture<'a, Result<(), WriteStatus>> {
        Box::pin(async move {
            let batch = self
                .requests
                .normalize(controller, body, &self.restore)
                .map_err(|error| match error {
                    RequestError::UnknownCharacteristic => WriteStatus::ResourceDoesNotExist,
                    RequestError::MissingRestoreLevel => WriteStatus::ServiceCommunicationFailure,
                    _ => WriteStatus::InvalidValueInRequest,
                })?;
            for intent in &batch.intents {
                log::debug!(
                    "hap_light_intent device={:?} command={:?}",
                    intent.device,
                    intent.command
                );
            }
            (self.submit)(batch).await
        })
    }
}
