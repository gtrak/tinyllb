use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use prometheus::Counter;
use std::sync::Arc;

use crate::scheduler::lifecycle::LifecycleGuard;
use crate::scheduler::QueueTicket;

/// RAII guard that decrements `vllm_requests_active` on drop.
///
/// Moved here so `MetricStream` can own the guard for the duration
/// of the stream (preventing early decrement when the handler returns).
pub struct RequestActiveGuard {
    metrics: Arc<crate::metrics::Metrics>,
}

impl RequestActiveGuard {
    pub fn new(metrics: Arc<crate::metrics::Metrics>) -> Self {
        metrics.requests_active.inc();
        Self { metrics }
    }
}

impl Drop for RequestActiveGuard {
    fn drop(&mut self) {
        self.metrics.requests_active.dec();
    }
}

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

/// Accumulates bytes and extracts `completion_tokens` from any SSE `usage`
/// JSON objects found.
struct TokenAccumulator {
    buffer: Vec<u8>,
}

impl TokenAccumulator {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feed new bytes into the accumulator and return extracted tokens.
    fn feed(&mut self, chunk: &[u8]) -> i64 {
        self.buffer.extend_from_slice(chunk);
        self.extract_tokens()
    }

    fn extract_tokens(&mut self) -> i64 {
        let mut found_tokens: i64 = 0;

        while let Some(pos) = self
            .buffer
            .windows(19)
            .position(|w| w == b"\"completion_tokens\"")
        {
            let after_key = pos + 19;
            if after_key < self.buffer.len() {
                let rest = &self.buffer[after_key..];
                if let Some(colon_pos) = rest.iter().position(|b| *b == b':') {
                    let after_colon = after_key + colon_pos + 1;
                    if let Some(num_start) = rest[colon_pos + 1..]
                        .iter()
                        .position(|b| *b != b' ' && *b != b'\t')
                    {
                        let val_start = after_colon + num_start;
                        let val_end = rest[colon_pos + 1 + num_start..]
                            .iter()
                            .position(|b| {
                                *b == b',' || *b == b'}' || *b == b' ' || *b == b'\n' || *b == b'\r'
                            })
                            .map(|p| val_start + p)
                            .unwrap_or(self.buffer.len());
                        if let Ok(num_str) = std::str::from_utf8(&self.buffer[val_start..val_end]) {
                            if let Ok(tokens) = num_str.trim().parse::<i64>() {
                                found_tokens += tokens;
                            }
                        }
                    }
                }
            }

            self.buffer.drain(..after_key);
        }

        found_tokens
    }
}

/// A stream wrapper that instruments SSE passthrough for token tracking.
///
/// Wraps the reqwest response stream directly and uses a `TokenAccumulator`
/// to best-effort parse `usage.completion_tokens` from streaming JSON payloads.
/// If parsing fails, tokens are silently skipped — the stream never
/// breaks due to metrics collection.
///
/// The `_queue_ticket` field holds the admission slot for the stream's
/// entire lifetime, ensuring the slot is released when the stream ends
/// (or the client disconnects), not when the handler returns.
///
/// The `lifecycle_guard` tracks whether the request completed normally
/// and reports accounting to the scheduler on Drop (credit restoration
/// on cancel, actual-token accounting on completion).
pub struct MetricStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    accumulator: TokenAccumulator,
    tokens_counter: Arc<Counter>,
    _active_guard: RequestActiveGuard,
    _queue_ticket: QueueTicket,
    lifecycle_guard: LifecycleGuard,
    /// Optional deadline for request timeout. When the deadline passes,
    /// the stream returns an error (which drops the guard as cancelled).
    deadline: Option<std::time::Instant>,
}

impl MetricStream {
    pub fn new(
        response: reqwest::Response,
        metrics: Arc<crate::metrics::Metrics>,
        queue_ticket: QueueTicket,
        lifecycle_guard: LifecycleGuard,
        deadline: Option<std::time::Instant>,
    ) -> Self {
        Self {
            inner: Box::pin(response.bytes_stream()),
            accumulator: TokenAccumulator::new(),
            tokens_counter: Arc::new(metrics.tokens_generated_total.clone()),
            _active_guard: RequestActiveGuard::new(metrics),
            _queue_ticket: queue_ticket,
            lifecycle_guard,
            deadline,
        }
    }
}

impl Stream for MetricStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check deadline before polling. If the deadline has passed, return an
        // error so the stream terminates and the LifecycleGuard drops as cancelled.
        if let Some(deadline) = self.deadline {
            if std::time::Instant::now() >= deadline {
                return Poll::Ready(Some(Err(std::io::Error::other(
                    "request timeout while streaming",
                ))));
            }
        }

        match futures::ready!(self.inner.as_mut().poll_next(cx)) {
            Some(Ok(chunk)) => {
                let tokens = self.accumulator.feed(&chunk);
                if tokens > 0 {
                    self.tokens_counter.inc_by(tokens as f64);
                    // Track delivered tokens for lifecycle accounting.
                    self.lifecycle_guard.add_delivered_tokens(tokens);
                    self.lifecycle_guard.record_token();
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Some(Err(err)) => Poll::Ready(Some(Err(std::io::Error::other(err.to_string())))),
            None => {
                // Backend stream completed normally — mark for lifecycle accounting.
                self.lifecycle_guard.mark_completed();
                Poll::Ready(None)
            }
        }
    }
}
