use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http_body_util::BodyExt;
use std::collections::HashSet;
use std::sync::Arc;

use crate::gateway::error::ProxyError;
use crate::gateway::stream::{MetricStream, RequestActiveGuard};
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
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, ProxyError> {
    // Extract all needed parts before consuming the request body.
    let original_path = req.uri().path().to_string();
    let query: Option<String> = req.uri().query().map(|q| q.to_string());
    let method = req.method().clone();
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

    // Check if the request explicitly wants streaming.
    let wants_streaming = body_wants_streaming(&body_bytes);

    // Build the backend URL, preserving the query string.
    let backend_url = build_backend_url(&state.backend_url, &original_path, query.as_deref())?;

    // Admit through the scheduler: blocks until a slot is available,
    // returns a RAII ticket that releases the slot on drop.
    // Under backpressure, this may reject with 429.
    let _ticket = match state.scheduler.admit().await {
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

    // Build and send the request to the backend, passing raw bytes (byte-preserving).
    let mut builder = state.client.request(method, backend_url).body(body_bytes);

    // Apply filtered headers.
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }

    // Send the request to the backend.
    let response = match builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            // Network error — increment vllm_errors_total.
            state.metrics.errors_total.inc();
            return Err(ProxyError::Network(e));
        }
    };

    let status = response.status();
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
        return Ok(ProxyError::BackendError {
            status,
            headers: filter_response_headers(&response_headers),
            body: body_bytes.to_vec(),
        }
        .into_response());
    }

    // Streaming path: if SSE or body wanted streaming, use MetricStream.
    // Do NOT forward Content-Length — axum will use chunked transfer encoding.
    // MetricStream owns both the RequestActiveGuard and the QueueTicket so the
    // admission slot stays held until the stream completes (or the client
    // disconnects), not when the handler returns.
    if is_sse || wants_streaming {
        let stream = MetricStream::new(response, state.metrics.clone(), _ticket);
        let body = Body::from_stream(stream);
        let mut resp = Response::new(body);
        *resp.status_mut() = status;

        // Copy filtered headers (hop-by-hop stripped, Content-Length removed).
        for (name, value) in filter_response_headers_streaming(&response_headers).iter() {
            resp.headers_mut().append(name, value.clone());
        }
        return Ok(resp);
    }

    // Non-streaming path: collect the full body and return with filtered headers.
    // The ticket (admission slot) is held until body collection finishes.
    let _guard = RequestActiveGuard::new(Arc::clone(&state.metrics));
    let body_bytes = collect_response_body(response, "normal-response").await?;

    // Best-effort: extract completion_tokens from the JSON response.
    let completion_tokens = extract_completion_tokens(&body_bytes);
    if completion_tokens > 0 {
        state
            .metrics
            .tokens_generated_total
            .inc_by(completion_tokens as f64);
    }

    let mut resp = Response::new(Body::from(body_bytes.to_vec()));
    *resp.status_mut() = status;

    // Copy filtered response headers (hop-by-hop stripped).
    for (name, value) in filter_response_headers(&response_headers).iter() {
        resp.headers_mut().append(name, value.clone());
    }
    Ok(resp)
}
