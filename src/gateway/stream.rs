use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;

/// A stream adapter wrapping the reqwest response bytes stream that maps
/// errors to `std::io::Error` so it can be consumed by
/// `axum::body::Body::from_stream`.
///
/// Each chunk is yielded immediately without buffering, enabling true
/// SSE (text/event-stream) passthrough.
pub struct PassthroughStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
}

impl PassthroughStream {
    pub fn new(response: reqwest::Response) -> Self {
        Self {
            inner: Box::pin(response.bytes_stream()),
        }
    }
}

impl Stream for PassthroughStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = futures::ready!(self.inner.as_mut().poll_next(cx));
        Poll::Ready(item.map(|result| {
            result.map_err(|err| {
                tracing::error!(error = %err, "error reading backend response body");
                std::io::Error::other(err.to_string())
            })
        }))
    }
}
