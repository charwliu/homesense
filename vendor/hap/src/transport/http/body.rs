//! Streaming request and buffered response bodies for Hyper 1.x.
use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
pub(crate) struct Body(UnsyncBoxBody<Bytes, crate::Error>);
impl Body {
    pub fn empty() -> Self {
        Self::from(Bytes::new())
    }
    pub async fn data(&mut self) -> Option<crate::Result<Bytes>> {
        while let Some(frame) = self.0.frame().await {
            match frame {
                Err(error) => return Some(Err(error)),
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        return Some(Ok(data));
                    }
                }
            }
        }
        None
    }
    #[cfg(test)]
    pub fn channel() -> (Sender, Self) {
        use futures::StreamExt;
        let (sender, receiver) = futures::channel::mpsc::channel::<Bytes>(1);
        let stream = receiver.map(|bytes| Ok::<_, crate::Error>(Frame::data(bytes)));
        (
            Sender(sender),
            Self(http_body_util::StreamBody::new(stream).boxed_unsync()),
        )
    }
}
impl From<Incoming> for Body {
    fn from(body: Incoming) -> Self {
        Self(body.map_err(crate::Error::from).boxed_unsync())
    }
}
impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        Self(
            Full::new(bytes)
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
    }
}
impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from(Bytes::from(bytes))
    }
}
impl From<String> for Body {
    fn from(bytes: String) -> Self {
        Self::from(Bytes::from(bytes))
    }
}
impl From<&'static str> for Body {
    fn from(bytes: &'static str) -> Self {
        Self::from(Bytes::from(bytes))
    }
}
impl HttpBody for Body {
    type Data = Bytes;
    type Error = crate::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<crate::Result<Frame<Bytes>>>> {
        Pin::new(&mut self.0).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}
#[cfg(test)]
pub(crate) struct Sender(futures::channel::mpsc::Sender<Bytes>);
#[cfg(test)]
impl Sender {
    pub async fn send_data(
        &mut self,
        bytes: Bytes,
    ) -> Result<(), futures::channel::mpsc::SendError> {
        use futures::SinkExt;
        self.0.send(bytes).await
    }
}
#[cfg(test)]
pub(crate) async fn to_bytes(body: Body) -> crate::Result<Bytes> {
    Ok(body.collect().await?.to_bytes())
}
