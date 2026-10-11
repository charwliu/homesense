//! Native HAP scene writes. The owner authorizes every addressed scene and
//! accepts all starts before returning success. No automatic retry is performed.
use crate::{
    requests::RequestError,
    scene_requests::{SceneBatch, SceneRequests},
};
use hap::server::{WriteBatchHandler, WriteStatus};
use uuid::Uuid;
pub struct SceneWriteHandler<S, R> {
    requests: SceneRequests,
    submit: S,
    reset: R,
}
impl<S, R> SceneWriteHandler<S, R> {
    pub fn new(requests: SceneRequests, submit: S, reset: R) -> Self {
        Self {
            requests,
            submit,
            reset,
        }
    }
}
impl<S, R, F, G> WriteBatchHandler for SceneWriteHandler<S, R>
where
    S: Fn(SceneBatch) -> F + Send + Sync,
    F: std::future::Future<Output = Result<(), WriteStatus>> + Send,
    R: Fn(Vec<String>) -> G + Send + Sync,
    G: std::future::Future<Output = Result<(), WriteStatus>> + Send,
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
                .normalize(controller, body)
                .map_err(|e| match e {
                    RequestError::UnknownCharacteristic => WriteStatus::ResourceDoesNotExist,
                    _ => WriteStatus::InvalidValueInRequest,
                })?;
            let starts = batch.starts.clone();
            (self.submit)(batch).await?;
            // A reset failure can occur after acceptance. Return failure without retrying
            // the physical action; the runtime receipt remains authoritative.
            if !starts.is_empty() {
                (self.reset)(starts).await?;
            }
            Ok(())
        })
    }
}
