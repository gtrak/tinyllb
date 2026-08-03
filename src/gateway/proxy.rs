use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::Span;
use uuid::Uuid;

use crate::flow::identify;
use crate::gateway::error::ProxyError;
use crate::gateway::stream::{MetricStream, RequestActiveGuard};
use crate::scheduler::lifecycle::LifecycleGuard;
use crate::scheduler::mode_label;

use super::AppState;

/// Maximum allowed request body size (32 MiB).
const MAX_BODY_SIZE: u64 = 32 * 1024 * 1024;

/// Hop-by-hop headers that must not be forwarded (RFC 7230 §6.1).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Headers to exclude entirely (reqwest sets Host itself).
const EXCLUDE_HEADERS: &[&str] = &["host"];

/// Strip hop-by-hop and other headers that shouldn't be forwarded.
fn filter_headers(headers: &HeaderMap) -> HeaderMap {
    let mut exclude: HashSet<&str> = HOP_BY_HOP
        .iter()
        .copied()
        .chain(EXCLUDE_HEADERS.iter().copied())
        .collect();

    // Also strip any header names listed in the Connection header.
    if let Some(conn) = headers.get(axum::http::header::CONNECTION) {
        if let Ok(conn_str) = conn.to_str() {
            for name in conn_str.split(',') {
                exclude.insert(name.trim());
            }
        }
    }

    let mut out = HeaderMap::new();
    for (name, value) in headers.iter() {
        if !exclude.contains(name.as_str()) {
            out.append(name, value.clone());
        }
    }
    out
}

/// Filter backend response headers, stripping hop-by-hop and connection-specific headers.
fn filter_response_headers(headers: &HeaderMap) -> HeaderMap {
    filter_headers(headers)
}

/// Filter backend response headers for the streaming path, additionally stripping
/// Content-Length because axum will use chunked transfer encoding.
fn filter_response_headers_streaming(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = filter_headers(headers);
    filtered.remove(axum::http::header::CONTENT_LENGTH);
    filtered
}

/// Determine if the request body indicates streaming mode.
/// Parses the body as JSON and checks for `"stream": true`.
fn body_wants_streaming(body: &Bytes) -> bool {
    if body.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        value
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    } else {
        false
    }
}

/// Inject `stream_options.include_usage: true` into a streaming request body.
///
/// vLLM (and OpenAI-compatible backends) only emit a final `usage` chunk in an
/// SSE stream when the client asks for it. Without it, the proxy never sees
/// `completion_tokens` and undercounts streaming traffic. Forcing it here keeps
/// token accounting correct regardless of client behavior.
///
/// Returns `Some` with the modified body when injection is needed, `None`
/// otherwise (non-streaming, already requested, or unparseable body).
fn inject_include_usage(body: &Bytes) -> Option<Bytes> {
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    // Only touch streaming requests.
    if obj.get("stream").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    // Already requesting usage — nothing to do.
    if obj
        .get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return None;
    }
    obj.entry("stream_options")
        .and_modify(|o| {
            if let Some(map) = o.as_object_mut() {
                map.insert("include_usage".to_string(), serde_json::Value::Bool(true));
            }
        })
        .or_insert_with(|| serde_json::json!({ "include_usage": true }));
    Some(serde_json::to_vec(&value).ok()?.into())
}

/// Extract `max_tokens` from the request body for WFQ work unit tracking.
/// Falls back to a default of 1024 if `max_tokens` is absent or unparseable.
fn extract_max_tokens(body: &Bytes) -> f64 {
    if body.is_empty() {
        return 1024.0;
    }
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        value
            .get("max_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(1024.0)
    } else {
        1024.0
    }
}

/// Build the backend URL by joining the base URL with the request path and query.
fn build_backend_url(
    backend_url: &url::Url,
    path: &str,
    query: Option<&str>,
) -> Result<url::Url, ProxyError> {
    // Join the path first.
    let mut url = backend_url.join(path).map_err(|e| {
        ProxyError::Internal(format!("failed to build backend URL for '{}': {}", path, e))
    })?;
    // Preserve the query string.
    if let Some(q) = query {
        url.set_query(Some(q));
    }
    Ok(url)
}

/// Collect a reqwest response body into bytes.
async fn collect_response_body(
    response: reqwest::Response,
    label: &str,
) -> Result<Bytes, ProxyError> {
    response.bytes().await.map_err(|e| {
        tracing::error!(error = %e, label, "failed to read backend response body");
        ProxyError::Network(e)
    })
}

/// Parse `usage.completion_tokens` from a JSON response body (best-effort).
/// Falls back to `total_tokens` if `completion_tokens` is absent.
/// Returns the extracted token count or 0 if parsing fails.
fn extract_completion_tokens(body: &[u8]) -> i64 {
    let value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    // Prefer completion_tokens; fall back to total_tokens if absent.
    value
        .get("usage")
        .and_then(|u| {
            u.get("completion_tokens")
                .and_then(|t| t.as_i64())
                .or_else(|| {
                    tracing::debug!("completion_tokens absent, falling back to total_tokens");
                    u.get("total_tokens").and_then(|t| t.as_i64())
                })
        })
        .unwrap_or(0)
}

/// Handle a proxied request.
#[tracing::instrument(skip_all, fields(
    flow_id = tracing::field::Empty,
    request_id = tracing::field::Empty,
    method = %req.method(),
    path = %req.uri().path(),
    stream = tracing::field::Empty,
))]
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, ProxyError> {
    // Record late-bound fields (resolved inside the handler body).
    let span = Span::current();

    // Generate a unique request ID, echoed back in X-Request-ID header.
    let request_id = Uuid::new_v4().to_string();
    span.record("request_id", &request_id);
    // Extract all needed parts before consuming the request body.
    let original_path = req.uri().path().to_string();
    let query: Option<String> = req.uri().query().map(|q| q.to_string());
    let method = req.method().clone();
    // Keep a reference to the original headers for flow ID resolution.
    let original_headers = req.headers().clone();
    let headers = filter_headers(req.headers());

    // Body size guard: reject if Content-Length exceeds MAX_BODY_SIZE.
    if let Some(cl_header) = req.headers().get(axum::http::header::CONTENT_LENGTH) {
        if let Ok(cl_str) = cl_header.to_str() {
            if let Ok(size) = cl_str.parse::<u64>() {
                if size > MAX_BODY_SIZE {
                    return Err(ProxyError::TooLarge);
                }
            }
        }
    }

    // Collect the request body with a size limit to guard against unbounded
    // chunked bodies (no Content-Length). Cap at MAX_BODY_SIZE.
    let limited = http_body_util::Limited::new(req.into_body(), MAX_BODY_SIZE as usize);
    let collected = limited.collect().await.map_err(|e| {
        // http_body_util::Limited emits LengthLimitError when the body
        // exceeds the cap — map it to 413, not 500.
        if e.downcast_ref::<http_body_util::LengthLimitError>()
            .is_some()
        {
            ProxyError::TooLarge
        } else {
            ProxyError::Internal(format!("failed to read request body: {}", e))
        }
    })?;
    let body_bytes = collected.to_bytes();

    // If the body hit the limit, reject it.
    if body_bytes.len() >= MAX_BODY_SIZE as usize {
        return Err(ProxyError::TooLarge);
    }

    // Resolve the flow ID from headers + body (header takes precedence).
    let flow_id = identify::resolve(&original_headers, &body_bytes);

    // Check if the request explicitly wants streaming.
    let wants_streaming = body_wants_streaming(&body_bytes);
    // Record late-bound fields in the request span.
    span.record("flow_id", flow_id.to_string());
    span.record("stream", wants_streaming);

    // Extract max_tokens from the request body for WFQ work unit tracking.
    let work_unit = extract_max_tokens(&body_bytes);
    // Build the backend URL, preserving the query string.
    let backend_url = build_backend_url(&state.backend_url, &original_path, query.as_deref())?;

    // Force `stream_options.include_usage` on streaming requests so the
    // backend always reports a final usage chunk (token accounting depends on
    // it). Only forwarded requests are modified; the client-facing body is
    // unaffected. When modified, the forwarded Content-Length must be dropped
    // (reqwest recomputes it from the new body).
    let mut headers = headers;
    let mut builder = state.client.request(method, backend_url);
    if let Some(injected) = inject_include_usage(&body_bytes) {
        headers.remove(axum::http::header::CONTENT_LENGTH);
        builder = builder.body(injected);
    } else {
        builder = builder.body(body_bytes);
    }

    // Admit through the scheduler: blocks until a slot is available,
    // returns a RAII ticket that releases the slot on drop.
    // Under backpressure, this may reject with 429.
    // Clone flow_id so we can use it for lifecycle tracking.
    let flow_id_for_admit = flow_id.clone();
    let _ticket = match state.scheduler.admit(flow_id_for_admit, work_unit).await {
        Ok(ticket) => ticket,
        Err(rejected) => {
            // Increment backpressure rejection counter.
            state
                .metrics
                .backpressure_rejections_total
                .with_label_values(&[mode_label(state.backpressure.mode)])
                .inc();
            return Err(ProxyError::Rejected {
                retry_after: rejected.retry_after,
            });
        }
    };

    // Create LifecycleGuard BEFORE send so connect-phase timeouts are covered.
    // If send() or the entire request times out, the guard drops as cancelled
    // (emits request_cancelled, restores credit, releases slot).
    let lifecycle = LifecycleGuard::new(
        flow_id.clone(),
        work_unit as i64,
        state.scheduler.clone(),
        state.metrics.clone(),
        Some(state.scheduler.flow_progress_tracker()),
    );

    // Measure backend forward duration.
    let forward_start = std::time::Instant::now();
    let bf_span = tracing::info_span!("backend_forward",
        flow_id = %flow_id,
        request_id = %request_id,
        status = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        tokens = tracing::field::Empty,
    );

    // Build and send the request to the backend, passing raw bytes (byte-preserving).
    // Apply filtered headers.
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }

    // Send the request with optional timeout.
    // Wrapping send() in timeout covers connect + response header phase.
    // Use Future::instrument (Send-safe) so the span is current while polled.
    use tracing::Instrument;
    let response = async {
        if let Some(timeout) = state.request_timeout {
            match tokio::time::timeout(timeout, builder.send()).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => {
                    // Network error — guard drops as cancelled.
                    state.metrics.errors_total.inc();
                    Err(ProxyError::Network(e))
                }
                Err(_) => {
                    // Timeout — guard drops as cancelled (emit cancelled, restore credit).
                    Err(ProxyError::Timeout)
                }
            }
        } else {
            builder.send().await.map_err(|e| {
                state.metrics.errors_total.inc();
                ProxyError::Network(e)
            })
        }
    }
    .instrument(bf_span.clone())
    .await;

    let response = match response {
        Ok(r) => r,
        Err(ProxyError::Network(e)) => {
            // Guard drops as cancelled.
            return Err(ProxyError::Network(e));
        }
        Err(ProxyError::Timeout) => {
            // Guard drops as cancelled.
            return Err(ProxyError::Timeout);
        }
        Err(e) => return Err(e),
    };

    let status = response.status();
    let forward_duration_ms = forward_start.elapsed().as_millis();
    bf_span.record("status", status.as_u16());
    bf_span.record("duration_ms", forward_duration_ms);
    let response_headers = response.headers().clone();
    let content_type = response_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let is_sse = content_type.starts_with("text/event-stream");

    // If the backend returned an error status (4xx/5xx), collect the body and
    // return it verbatim with filtered headers.
    if status.is_client_error() || status.is_server_error() {
        // Count 5xx errors (not 4xx — those are client errors).
        if status.is_server_error() {
            state.metrics.errors_total.inc();
        }
        let body_bytes = collect_response_body(response, "error-response").await?;
        // Backend completed (even with error status) — mark as completed
        // with 0 delivered tokens (charges full estimated cost).
        lifecycle.mark_completed();
        let mut resp = Response::new(Body::from(body_bytes.to_vec()));
        *resp.status_mut() = status;
        *resp.headers_mut() = filter_response_headers(&response_headers);
        // Echo X-Request-ID back in response.
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static("x-request-id"),
            axum::http::HeaderValue::from_str(&request_id).expect("valid UUID header value"),
        );
        return Ok(resp);
    }

    // Streaming path: if SSE or body wanted streaming, use MetricStream.
    // Do NOT forward Content-Length — axum will use chunked transfer encoding.
    // MetricStream owns the RequestActiveGuard and the QueueTicket so the
    // admission slot stays held until the stream completes (or the client
    // disconnects), not when the handler returns.
    if is_sse || wants_streaming {
        // Compute deadline for stream timeout (if configured).
        let deadline = state.request_timeout.map(|t| std::time::Instant::now() + t);
        let stream = MetricStream::new(
            response,
            state.metrics.clone(),
            _ticket,
            lifecycle,
            deadline,
        );
        let body = Body::from_stream(stream);
        let mut resp = Response::new(body);
        *resp.status_mut() = status;

        // Copy filtered headers (hop-by-hop stripped, Content-Length removed).
        for (name, value) in filter_response_headers_streaming(&response_headers).iter() {
            resp.headers_mut().append(name, value.clone());
        }
        // Echo X-Request-ID back in response.
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static("x-request-id"),
            axum::http::HeaderValue::from_str(&request_id).expect("valid UUID header value"),
        );
        return Ok(resp);
    }

    // Non-streaming path: collect the full body and return with filtered headers.
    // The ticket (admission slot) is held until body collection finishes.
    let _guard = RequestActiveGuard::new(Arc::clone(&state.metrics));

    // Collect the response body with optional timeout.
    let body_bytes = if let Some(timeout) = state.request_timeout {
        match tokio::time::timeout(timeout, collect_response_body(response, "normal-response"))
            .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(e)) => {
                // Body collection error — guard drops as cancelled.
                return Err(e);
            }
            Err(_) => {
                // Timeout during body collection — guard drops as cancelled.
                return Err(ProxyError::Timeout);
            }
        }
    } else {
        collect_response_body(response, "normal-response").await?
    };

    // Best-effort: extract completion_tokens from the JSON response.
    let completion_tokens = extract_completion_tokens(&body_bytes);
    if completion_tokens > 0 {
        state
            .metrics
            .tokens_generated_total
            .inc_by(completion_tokens as f64);
        lifecycle.add_delivered_tokens(completion_tokens);
    }
    // Record tokens in backend_forward span (non-streaming path only).
    bf_span.record("tokens", completion_tokens);

    // Mark the request as completed normally.
    lifecycle.mark_completed();
    // lifecycle guard drops here, emitting request_completed event and reporting
    // accounting to the scheduler.

    let mut resp = Response::new(Body::from(body_bytes.to_vec()));
    *resp.status_mut() = status;

    // Copy filtered response headers (hop-by-hop stripped).
    for (name, value) in filter_response_headers(&response_headers).iter() {
        resp.headers_mut().append(name, value.clone());
    }
    // Echo X-Request-ID back in response.
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("x-request-id"),
        axum::http::HeaderValue::from_str(&request_id).expect("valid UUID header value"),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inject(body: &str) -> Option<String> {
        inject_include_usage(&Bytes::from(body.to_string()))
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
    }

    #[test]
    fn streaming_without_options_gets_include_usage() {
        let out = inject(r#"{"model":"local","stream":true,"max_tokens":10}"#).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], true);
        assert_eq!(v["stream"], true);
        assert_eq!(v["max_tokens"], 10);
    }

    #[test]
    fn streaming_with_include_usage_preserved() {
        let body = r#"{"model":"local","stream":true,"stream_options":{"include_usage":true}}"#;
        assert!(inject(body).is_none(), "no re-encode when already set");
    }

    #[test]
    fn streaming_merges_other_stream_options() {
        let body =
            r#"{"model":"local","stream":true,"stream_options":{"chunk":{"some":"opt"}}}"#;
        let out = inject(body).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], true);
        assert_eq!(v["stream_options"]["chunk"]["some"], "opt");
    }

    #[test]
    fn non_streaming_untouched() {
        let body = r#"{"model":"local","stream":false,"max_tokens":10}"#;
        assert!(inject(body).is_none(), "non-streaming bodies must pass through");
    }

    #[test]
    fn unparseable_body_untouched() {
        assert!(inject("not json").is_none());
        assert!(inject("").is_none());
    }
}
