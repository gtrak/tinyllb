//! Integration tests for the transient re-forward feature (plan 007, task 04):
//! transient llama.cpp intake errors (context-exceed where the prompt fits
//! slot capacity) and transient network errors (backend restart) are
//! re-forwarded with bounded exponential backoff; permanent context-exceed,
//! non-llama.cpp (vLLM-shaped) errors, and the disabled policy pass through
//! unchanged with zero re-forwards.

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use tinyllb::config::{BackpressureMode, TransientRetry};
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Fast-backoff transient retry policy so tests do not sleep the default
/// 500ms–4s.
fn fast_transient_policy(max_attempts: u32) -> TransientRetry {
    TransientRetry {
        max_attempts,
        backoff_start: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
    }
}

fn build_proxy_app(
    backend_url: &str,
    transient_retry: TransientRetry,
) -> (Router, Arc<tinyllb::metrics::Metrics>) {
    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = scheduler::Scheduler::new_with_defaults(
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        transient_retry,
        ..gateway::AppState::test_default(
            gateway::build_client(),
            Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
            metrics.clone(),
            Arc::new(scheduler),
            flow_registry,
        )
    };

    let gateway_router = gateway::create_router().with_state(state.clone());
    let app = Router::new().merge(gateway_router).with_state(state);

    (app, metrics)
}

async fn collect_body_bytes(resp: Response<Body>) -> Bytes {
    axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap()
}

/// POST a chat-completions request to the proxy app and return the response.
async fn post_chat(app: &Router, body: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

const CHAT_BODY: &str = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
const STREAM_CHAT_BODY: &str =
    r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}],"stream":true}"#;

// ---------------------------------------------------------------------------
// Helper: response body builders
// ---------------------------------------------------------------------------

/// llama.cpp `exceed_context_size_error` body with
/// `n_prompt_tokens >= n_ctx` — permanent (prompt cannot fit slot capacity).
fn permanent_ctx_error_body() -> String {
    serde_json::json!({
        "error": {
            "code": 400,
            "type": "exceed_context_size_error",
            "message": "prompt is too long for the context",
            "n_prompt_tokens": 300_000,
            "n_ctx": 262_144
        }
    })
    .to_string()
}

/// llama.cpp `exceed_context_size_error` body with
/// `n_prompt_tokens < n_ctx` — transient (prompt fits slot capacity once the
/// backend's transient state clears).
fn transient_ctx_error_body() -> String {
    serde_json::json!({
        "error": {
            "code": 400,
            "type": "exceed_context_size_error",
            "message": "prompt is too long for the context",
            "n_prompt_tokens": 100,
            "n_ctx": 262_144
        }
    })
    .to_string()
}

fn chat_completion_body(content: &str) -> String {
    serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": content},
            "index": 0
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })
    .to_string()
}

/// A normal 200 SSE stream body (delta frames + terminal + [DONE]).
fn sse_stream_body() -> String {
    [
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat()
}

// ---------------------------------------------------------------------------
// Helper: raw-TCP stub that drops the first connection (backend restart)
// ---------------------------------------------------------------------------

/// Bind a raw-TCP "backend" on an ephemeral port. It simulates a backend
/// restart under live traffic: the FIRST accepted connection is allowed to
/// send its request, then the socket is dropped without a response (the
/// client observes the connection dying mid-request — RST/EOF). All
/// subsequent connections are answered with a minimal HTTP/1.1 200 JSON
/// response. Returns the address and a counter of connections served.
async fn start_reset_then_serve_stub() -> (std::net::SocketAddr, Arc<AtomicU32>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Arc::new(AtomicU32::new(0));

    let served_clone = served.clone();
    tokio::spawn(async move {
        let mut attempt: u32 = 0;
        loop {
            let (mut sock, _peer) = listener.accept().await.unwrap();
            attempt += 1;
            let served = served_clone.clone();
            tokio::spawn(async move {
                if attempt == 1 {
                    // Give the client time to send the request, then drop the
                    // connection without responding (died mid-request).
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    drop(sock);
                } else {
                    serve_minimal_http_200(&mut sock).await;
                    served.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });

    (addr, served)
}

/// Locate the end of an HTTP request head (`\r\n\r\n`), if present.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Minimal HTTP/1.1 server: read the request head plus the `Content-Length`
/// body bytes, then reply with a 200 JSON body and close the connection
/// (`connection: close` keeps the exchange one-shot).
async fn serve_minimal_http_200(sock: &mut tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    let _content_length: u64 = loop {
        match sock.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        let Some(pos) = find_head_end(&buf) else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
        let cl = head
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<u64>().ok())
            })
            .unwrap_or(0);
        // Drain the full request body before replying so the client's write
        // cannot race with our close.
        let need = (pos + 4) as u64 + cl;
        while (buf.len() as u64) < need {
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        break cl;
    };

    let body = chat_completion_body("recovered");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.flush().await;
}

// ---------------------------------------------------------------------------
// Helper: parse SSE data payloads from client response body
// ---------------------------------------------------------------------------

/// Parse a client SSE response body into a Vec of `data:` payloads.
fn parse_sse_data_payloads(body_bytes: &Bytes) -> Vec<String> {
    let body_str = String::from_utf8_lossy(body_bytes);
    let mut payloads = Vec::new();
    for event in body_str.split("\n\n") {
        if event.trim().is_empty() {
            continue;
        }
        for line in event.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                payloads.push(payload.trim().to_string());
            }
        }
    }
    payloads
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1. Permanent context-exceed (n_prompt_tokens >= n_ctx): passed through
/// verbatim, zero re-forwards.
#[tokio::test]
async fn permanent_passthrough() {
    let call_count = Arc::new(AtomicU32::new(0));
    let error_b = permanent_ctx_error_body();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let error_b = error_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(error_b));
            *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(resp.status(), 400);

    // Body is echoed verbatim.
    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"]["type"], "exceed_context_size_error");
    assert_eq!(json["error"]["n_prompt_tokens"], 300_000);
    assert_eq!(json["error"]["n_ctx"], 262_144);

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "permanent error must not be re-forwarded"
    );
    assert_eq!(state_metrics.backend_retries_total.get() as u64, 0);
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}

/// 2. Transient context-exceed (n_prompt_tokens < n_ctx) on attempt 1, then a
/// normal 200 on attempt 2: client gets the 200 body, exactly one re-forward.
#[tokio::test]
async fn transient_then_success() {
    let call_count = Arc::new(AtomicU32::new(0));
    let error_b = transient_ctx_error_body();
    let good_b = chat_completion_body("hello");

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let error_b = error_b.clone();
        let good_b = good_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let mut resp = Response::new(Body::from(error_b));
                *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                resp
            } else {
                let mut resp = Response::new(Body::from(good_b));
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                resp
            }
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(resp.status(), 200);

    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "hello");

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "backend should be called twice (initial + 1 re-forward)"
    );
    assert_eq!(state_metrics.backend_retries_total.get() as u64, 1);
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}

/// 3. Stub always returns the transient 400: the re-forward budget
/// (max_attempts = 3) is exhausted and the last error is forwarded verbatim.
/// Plan 007 task 04: an exhausted transient retry budget increments
/// `backend_retry_exhausted_total` (last error response forwarded to client).
#[tokio::test]
async fn transient_exhaustion() {
    let call_count = Arc::new(AtomicU32::new(0));
    let error_b = transient_ctx_error_body();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let error_b = error_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(error_b));
            *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(resp.status(), 400);

    // Last (exhausted) error body forwarded verbatim.
    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"]["type"], "exceed_context_size_error");
    assert_eq!(json["error"]["n_prompt_tokens"], 100);
    assert_eq!(json["error"]["n_ctx"], 262_144);

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        4,
        "backend should be called 4 times (initial + 3 re-forwards)"
    );
    assert_eq!(
        state_metrics.backend_retries_total.get() as u64,
        3,
        "re-forward budget is max_attempts"
    );
    assert_eq!(
        state_metrics.backend_retry_exhausted_total.get() as u64,
        1,
        "exhausted transient retry budget must increment backend_retry_exhausted_total"
    );
}

/// 4. Policy disabled (max_attempts = 0): the transient 400 passes through
/// with zero behavioral change.
#[tokio::test]
async fn disabled_passthrough() {
    let call_count = Arc::new(AtomicU32::new(0));
    let error_b = transient_ctx_error_body();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let error_b = error_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(error_b));
            *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(0));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(resp.status(), 400);

    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"]["type"], "exceed_context_size_error");
    assert_eq!(json["error"]["n_prompt_tokens"], 100);

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "disabled policy: backend called exactly once"
    );
    assert_eq!(state_metrics.backend_retries_total.get() as u64, 0);
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}

/// 5. Transient network error (backend restart): the first connection dies
/// mid-request (accepted, then dropped without a response), the re-forward
/// succeeds on a fresh connection. Client gets the 200 body, exactly one
/// re-forward.
#[tokio::test]
async fn network_error_then_success() {
    let (addr, served) = start_reset_then_serve_stub().await;
    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(
        resp.status(),
        200,
        "client should receive the 200 from the re-forward after the connection died"
    );

    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "recovered");

    assert_eq!(
        served.load(Ordering::SeqCst),
        1,
        "exactly one connection served (the re-forward)"
    );
    assert_eq!(
        state_metrics.backend_retries_total.get() as u64,
        1,
        "one re-forward after the transient network error"
    );
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}

/// 6. Streaming request: transient intake 400 on attempt 1 (before any SSE
/// bytes), then a 200 SSE stream on attempt 2. The client receives the SSE
/// stream (data frames + terminating [DONE]) and exactly one re-forward was
/// issued.
#[tokio::test]
async fn streaming_intake_transient() {
    let call_count = Arc::new(AtomicU32::new(0));
    let error_b = transient_ctx_error_body();
    let sse_b = sse_stream_body();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let error_b = error_b.clone();
        let sse_b = sse_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let mut resp = Response::new(Body::from(error_b));
                *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                resp
            } else {
                let mut resp = Response::new(Body::from(sse_b));
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/event-stream"),
                );
                resp
            }
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, STREAM_CHAT_BODY).await;
    assert_eq!(resp.status(), 200);

    let body_bytes = collect_body_bytes(resp).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains("data:"),
        "client should receive SSE data frames"
    );
    assert!(body_str.contains("hello"), "client should receive the stream content");

    let payloads = parse_sse_data_payloads(&body_bytes);
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(
        done_count, 1,
        "exactly one terminating [DONE] frame"
    );

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "backend should be called twice (initial + 1 re-forward)"
    );
    assert_eq!(state_metrics.backend_retries_total.get() as u64, 1);
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}

/// 7. Regression: a vLLM-shaped 4xx (non-llama.cpp error body) classifies as
/// `NotLlamacpp` and passes through with zero re-forwards — the feature does
/// not change vLLM error handling.
#[tokio::test]
async fn vllm_shaped_4xx_regression() {
    let vllm_error = serde_json::json!({
        "error": {
            "message": "simulated non-llama.cpp backend error",
            "type": "some_other_error"
        }
    })
    .to_string();
    let call_count = Arc::new(AtomicU32::new(0));

    // Direct classification assertion.
    assert_eq!(
        tinyllb::gateway::retry::classify_llamacpp_error(vllm_error.as_bytes()),
        tinyllb::gateway::retry::LlamacppErrorClass::NotLlamacpp,
        "vLLM-shaped body must classify as NotLlamacpp"
    );

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let vllm_error = vllm_error.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(vllm_error));
            *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let (app, state_metrics) = build_proxy_app(&backend_url, fast_transient_policy(3));

    let resp = post_chat(&app, CHAT_BODY).await;
    assert_eq!(resp.status(), 400);

    let body_bytes = collect_body_bytes(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        json["error"]["type"], "some_other_error",
        "non-llama.cpp body is forwarded verbatim"
    );

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "non-llama.cpp 4xx must not be re-forwarded"
    );
    assert_eq!(state_metrics.backend_retries_total.get() as u64, 0);
    assert_eq!(state_metrics.backend_retry_exhausted_total.get() as u64, 0);
}
