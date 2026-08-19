use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::StreamExt;
use futures::Stream;
use prometheus::Counter;
use tokio::sync::mpsc;

use crate::gateway::retry::{bump_temperature, classify_frame, SseFrameParser};
use crate::scheduler::lifecycle::LifecycleGuard;
use crate::scheduler::QueueTicket;

/// Thin wrapper making a tokio::sync::mpsc::Receiver a futures::Stream.
struct ReceiverStream<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // SAFETY: Pin projection of self.rx (receiver is !Unpin, but we never
        // move it out — just poll it in place).
        let pin: Pin<&mut mpsc::Receiver<T>> = unsafe { self.map_unchecked_mut(|s| &mut s.rx) };
        Pin::into_inner(pin).poll_recv(cx)
    }
}

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

// @lat: [[gateway#Streaming Passthrough and Token Accounting]]
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

/// Best-effort extract `usage.completion_tokens` from an SSE frame's `data:` JSON line.
/// Falls back to `total_tokens`. Returns 0 on parse failure. Never panics.
#[allow(dead_code)]
fn completion_tokens_from_frame(frame: &[u8]) -> i64 {
    for line in frame.split(|b| *b == b'\n') {
        let line = line.strip_prefix(b"data: ").or_else(|| line.strip_prefix(b"data:")).unwrap_or(line);
        let trimmed = trim_ascii(line);
        let json = match serde_json::from_slice::<serde_json::Value>(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = json
            .get("usage")
            .and_then(|u| {
                u.get("completion_tokens")
                    .and_then(|t| t.as_i64())
                    .or_else(|| u.get("total_tokens").and_then(|t| t.as_i64()))
            })
            .unwrap_or(0);
        if t > 0 {
            return t;
        }
    }
    0
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !is_ascii_ws(*c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !is_ascii_ws(*c)).map_or(0, |i| i + 1);
    if start >= end {
        &[]
    } else {
        &b[start..end]
    }
}

fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

/// Spawn a retry-capable streaming task and return the client-facing body.
///
/// The spawned task owns the `QueueTicket`, `LifecycleGuard`, and
/// `RequestActiveGuard` for the entire retry loop.  It frames the backend
/// SSE stream with `SseFrameParser`, forwards frames to an mpsc channel
/// (the client body), and swaps the inner stream on a premature-stop retry.
///
/// Token accounting (metrics + lifecycle) only applies to the **accepted**
/// attempt.  Failed attempts' tokens are silently discarded.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_retry_stream(
    state: super::AppState,
    response: reqwest::Response,
    backend_url: url::Url,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
    forwarded_body: bytes::Bytes,
    queue_ticket: QueueTicket,
    lifecycle_guard: LifecycleGuard,
) -> axum::body::Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let metrics = state.metrics.clone();
    let policy = state.retry_policy.clone();
    let client = state.client.clone();
    let request_timeout = state.request_timeout;
    let mut stall_rx = state.stall_rx.clone();
    // Pre-parse forwarded body once.  If it fails to parse as JSON, we cannot
    // bump temperature — fall back to raw passthrough of the first stream
    // (no retry) and mark_completed on end.
    let forwarded_value: Option<serde_json::Value> = serde_json::from_slice(&forwarded_body).ok();

    tokio::spawn(async move {
        let _active_guard = RequestActiveGuard::new(metrics.clone());
        let _queue_ticket = queue_ticket;
        let lifecycle_guard = lifecycle_guard;
        let mut inner = response.bytes_stream();

        // Premature-stop retries (degenerate model output) have their own
        // budget with temperature bumping.
        let mut attempt: u32 = 0;
        let can_retry = forwarded_value.is_some() && policy.enabled && policy.max_retries > 0;
        let fwd_value = forwarded_value;

        // Transport retries (stream EOF / stall abort without a terminal
        // frame) retry the ORIGINAL request: they are connection-layer
        // failures, not sampling failures, so they get a separate, more
        // generous budget with exponential backoff and never bump the
        // temperature. The request_timeout deadline is per-attempt so a
        // legitimate long thinking turn is not killed mid-flight.
        const MAX_TRANSPORT_RETRIES: u32 = 8;
        let mut transport_retries: u32 = 0;

        // Whether the stream completed normally (terminal frame accepted and
        // forwarded). When false at loop exit, the body is terminated with an
        // error instead of a clean close — hyper turns that into an abrupt
        // response termination, which clients treat as a transport failure
        // and auto-retry (same class as a connection reset). A clean close of
        // a truncated stream looks like a completed response to the client
        // and is silently accepted.
        let mut stream_ok = false;

        'retry: loop {
            // Per-attempt deadline.
            let deadline = request_timeout.map(|t| std::time::Instant::now() + t);

            let mut parser = SseFrameParser::new();
            let mut saw_content = false;
            let mut saw_tool_calls = false;
            let mut accepted = false;

            // Read loop outcome.
            enum Outcome {
                Eof,
                Deadline,
            }
            let outcome = loop {
                // Inference-watchdog: if the monitor declares the engine
                // deadlocked, abort this stream (dropping `inner` on the
                // next assignment closes the backend connection) and retry.
                if *stall_rx.borrow() {
                    tracing::warn!(
                        transport_retries = transport_retries,
                        "backend stall watchdog fired — aborting stream to retry"
                    );
                    break Outcome::Eof;
                }

                let chunk = tokio::select! {
                    changed = stall_rx.changed() => {
                        if changed.is_err() {
                            // Monitor dropped — ignore and keep streaming.
                        }
                        continue;
                    }
                    c = StreamExt::next(&mut inner) => match c {
                        Some(Ok(c)) => c,
                        Some(Err(_)) | None => break Outcome::Eof,
                    },
                };

                // Deadline check.
                if let Some(dl) = deadline {
                    if std::time::Instant::now() >= dl {
                        break Outcome::Deadline;
                    }
                }

                let frames = parser.feed(&chunk);
                for frame in frames {
                    let cls = classify_frame(&frame);

                    if !accepted {
                        // Track content/tool_calls for premature-stop detection.
                        if cls.has_content {
                            saw_content = true;
                        }
                        if cls.has_tool_calls {
                            saw_tool_calls = true;
                        }

                        if let Some(ref _fr) = cls.finish_reason {
                            let fr = _fr.clone();
                            // Terminal frame: decide whether to accept or retry.
                            let premature =
                                (fr == "stop" || fr == "length") && !saw_content && !saw_tool_calls && attempt < policy.max_retries && can_retry;

                            if premature {
                                metrics.premature_stop_detected_total.inc();
                                metrics.premature_stop_retries_total.inc();
                                attempt += 1;

                                let retry_value =
                                    bump_temperature(fwd_value.as_ref().unwrap(), attempt, &policy);
                                let bumped_temp = retry_value.get("temperature").and_then(|v| v.as_f64());
                                tracing::warn!(
                                    retry_attempt = attempt,
                                    bumped_temperature = ?bumped_temp,
                                    "premature-stop retry (streaming): degenerate stop with no content/tool_calls, retrying with bumped temperature"
                                );
                                let notice = format!(
                                    ": tinyllb: premature-stop retry attempt={} fired — discarded failed reasoning, retrying\n\n",
                                    attempt
                                );
                                if tx.send(Ok(bytes::Bytes::from(notice))).await.is_err() {
                                    return; // client disconnected
                                }
                                let retry_bytes = match serde_json::to_vec(&retry_value) {
                                    Ok(b) => b,
                                    Err(_) => {
                                        metrics.premature_stop_exhausted_total.inc();
                                        break 'retry; // fail-open (aborted body, client retries)
                                    }
                                };

                                let mut rh = headers.clone();
                                rh.remove(axum::http::header::CONTENT_LENGTH);
                                let mut rb = client.request(method.clone(), backend_url.clone()).body(retry_bytes);
                                for (n, v) in rh.iter() {
                                    rb = rb.header(n, v);
                                }

                                let send = if let Some(t) = request_timeout {
                                    match tokio::time::timeout(t, rb.send()).await {
                                        Ok(x) => x.map_err(|_| ()),
                                        Err(_) => Err(()),
                                    }
                                } else {
                                    rb.send().await.map_err(|_| ())
                                };

                                match send {
                                    Ok(r) if r.status().is_success() => {
                                        inner = r.bytes_stream();
                                        continue 'retry; // restart with fresh parser/flags
                                    }
                                    _ => {
                                        metrics.premature_stop_exhausted_total.inc();
                                        break 'retry; // fail-open (aborted body, client retries)
                                    }
                                }
                            } else {
                                // Accepted terminal: forward + count combined usage.
                                accepted = true;
                                // If we accepted only because retries are exhausted and the turn is still
                                // degenerate (premature-shaped: stop + no content + no tool_calls), count
                                // it as an exhausted fail-open (matches the non-streaming post-loop check).
                                if (fr == "stop" || fr == "length") && !saw_content && !saw_tool_calls && attempt >= policy.max_retries {
                                    metrics.premature_stop_exhausted_total.inc();
                                }
                                if cls.has_usage {
                                    let t = completion_tokens_from_frame(&frame);
                                    if t > 0 {
                                        metrics.tokens_generated_total.inc_by(t as f64);
                                        lifecycle_guard.add_delivered_tokens(t);
                                        lifecycle_guard.record_token();
                                    }
                                }
                                if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                                    return;
                                }
                            }
                        } else if cls.is_done {
                            // [DONE] marker — forward only if already accepted.
                            if accepted && tx.send(Ok(Bytes::from(frame))).await.is_err() {
                                return;
                            }
                        } else if cls.has_usage {
                            // Usage frame before terminal: forward if accepted.
                            if accepted {
                                let t = completion_tokens_from_frame(&frame);
                                if t > 0 {
                                    metrics.tokens_generated_total.inc_by(t as f64);
                                    lifecycle_guard.add_delivered_tokens(t);
                                    lifecycle_guard.record_token();
                                }
                                if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                                    return;
                                }
                            }
                        } else {
                            // Non-terminal reasoning/content delta: forward live.
                            if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                                return;
                            }
                        }
                    } else {
                        // Accepted: forward everything; count usage tokens.
                        if cls.has_usage {
                            let t = completion_tokens_from_frame(&frame);
                            if t > 0 {
                                metrics.tokens_generated_total.inc_by(t as f64);
                                lifecycle_guard.add_delivered_tokens(t);
                                lifecycle_guard.record_token();
                            }
                        }
                        if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                            return;
                        }
                    }
                }
            };

            // Flush any leftover incomplete frame.
            let leftover = parser.finish();
            for f in leftover {
                if tx.send(Ok(Bytes::from(f))).await.is_err() {
                    return;
                }
            }

            if accepted {
                lifecycle_guard.mark_completed();
                stream_ok = true;
                break 'retry;
            }

            if matches!(outcome, Outcome::Deadline) {
                // Attempt exceeded the per-attempt request timeout. Nothing
                // salvageable — abort the body (client retry is the remedy).
                tracing::warn!(
                    timeout_secs = request_timeout.map(|t| t.as_secs()),
                    "stream attempt exceeded request timeout without a terminal frame"
                );
                break 'retry;
            }

            // Outcome::Eof — stream ended (or was aborted by the stall
            // watchdog) without a terminal frame. Only retry when nothing
            // substantive was delivered (content/tool_calls would be
            // duplicated on the client; reasoning-only is tolerated).
            if saw_content || saw_tool_calls {
                tracing::warn!(
                    "stream ended without terminal frame after partial content/tool_calls — aborting body to avoid duplication"
                );
                break 'retry;
            }

            // Transport retry: same request, backoff, generous budget.
            while transport_retries < MAX_TRANSPORT_RETRIES {
                transport_retries += 1;
                metrics.stream_eof_retries_total.inc();
                let backoff = std::time::Duration::from_millis(
                    (250u64 << (transport_retries - 1).min(4)).min(4000),
                );
                tracing::warn!(
                    transport_retry = transport_retries,
                    backoff_ms = backoff.as_millis() as u64,
                    "stream retry (eof/stall): stream ended without terminal frame, re-sending original request"
                );
                let notice = format!(
                    ": tinyllb: stream retry {} (backend stream ended early) — re-sending request\n\n",
                    transport_retries
                );
                if tx.send(Ok(bytes::Bytes::from(notice))).await.is_err() {
                    return; // client disconnected
                }
                tokio::time::sleep(backoff).await;

                let mut rh = headers.clone();
                rh.remove(axum::http::header::CONTENT_LENGTH);
                let mut rb = client
                    .request(method.clone(), backend_url.clone())
                    .body(forwarded_body.clone());
                for (n, v) in rh.iter() {
                    rb = rb.header(n, v);
                }

                let send = if let Some(t) = request_timeout {
                    match tokio::time::timeout(t, rb.send()).await {
                        Ok(x) => x.map_err(|_| ()),
                        Err(_) => Err(()),
                    }
                } else {
                    rb.send().await.map_err(|_| ())
                };

                match send {
                    Ok(r) if r.status().is_success() => {
                        inner = r.bytes_stream();
                        continue 'retry;
                    }
                    _ => {
                        // Send failed — loop retries with further backoff.
                        continue;
                    }
                }
            }

            // Transport retries exhausted: give up. Do NOT fabricate a
            // terminal frame and do NOT close the body cleanly — an abrupt
            // termination is a transport error the client can retry.
            metrics.premature_stop_exhausted_total.inc();
            tracing::warn!(
                transport_retries = transport_retries,
                "stream retries exhausted — aborting body"
            );
            break 'retry;
        }

        if !stream_ok {
            // The stream did not complete with an accepted terminal frame.
            // End the body with an error rather than a clean close: hyper
            // terminates the response abruptly, which clients (opencode)
            // handle as a transport failure and auto-retry — the same
            // recovery path as a connection reset. A clean close would look
            // like a completed (but truncated) response and kill the thread.
            let _ = tx
                .send(Err(std::io::Error::other(
                    "tinyllb: backend stream ended without a terminal frame",
                )))
                .await;
        }
        // tx, _queue_ticket, _active_guard, lifecycle_guard drop here.
    });

    axum::body::Body::from_stream(ReceiverStream { rx })
}
