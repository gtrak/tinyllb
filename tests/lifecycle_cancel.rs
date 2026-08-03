//! Lifecycle and cancellation tests (issue 13).
//!
//! Verifies that scheduler resources (admission slot and flow credit) are
//! correctly released on every completion path:
//! - request_completed (normal streaming completion)
//! - client disconnect mid-stream
//! - timeout cancellation
//! - explicit cancel via DELETE (deferred for V1)
//!
//! Tests check:
//! - Admission slot released (active_flows returns to baseline)
//! - Credit restored on cancel (DRR)
//! - request_events_total counter reflects correct events
//! - Normal completion charges actual delivered tokens (DRR)

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures::Stream;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use llm_qdisc_proxy::config::{Algorithm, Backpressure, BackpressureMode};
use llm_qdisc_proxy::flow::FlowRegistry;
use llm_qdisc_proxy::gateway;
use llm_qdisc_proxy::metrics;
use llm_qdisc_proxy::scheduler::Scheduler;

// ---------------------------------------------------------------------------
// SSE stream wrapper for tests
// ---------------------------------------------------------------------------

struct SseStream {
    chunks: std::vec::IntoIter<Bytes>,
}

impl SseStream {
    fn new(chunks: Vec<Bytes>) -> Self {
        Self {
            chunks: chunks.into_iter(),
        }
    }
}

impl Stream for SseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        std::task::Poll::Ready(this.chunks.next().map(Ok))
    }
}

// ---------------------------------------------------------------------------
// Stub handlers
// ---------------------------------------------------------------------------

/// Slow streaming handler for disconnect tests.
async fn slow_streaming_handler(_req: Request<Body>) -> Response<Body> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(16);

    tokio::spawn(async move {
        let chunks = vec![
            Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n"),
            Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n"),
            Bytes::from("data: [DONE]\n\n"),
        ];
        for chunk in chunks {
            if tx.send(chunk).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let stream = SlowSseStream { rx };
    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    resp
}

/// Stream that reads from a tokio channel.
struct SlowSseStream {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
}

impl Stream for SlowSseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

// ---------------------------------------------------------------------------
// Stub backend with usage-data SSE and non-streaming support
// ---------------------------------------------------------------------------

/// Unified handler for /v1/chat/completions that supports both streaming and
/// non-streaming based on the request body's "stream" field.
async fn chat_completions_handler(req: Request<Body>) -> Response<Body> {
    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();

    let wants_stream = if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
        json.get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    } else {
        false
    };

    if wants_stream {
        streaming_handler_with_usage_inner().await
    } else {
        nonstream_handler_inner().await
    }
}

async fn streaming_handler_with_usage_inner() -> Response<Body> {
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n"),
        // Include usage frame with completion_tokens.
        Bytes::from("data: {\"usage\":{\"completion_tokens\":42,\"prompt_tokens\":10,\"total_tokens\":52}}\n\n"),
        Bytes::from("data: [DONE]\n\n"),
    ];
    let stream = SseStream::new(chunks);
    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    resp
}

async fn nonstream_handler_inner() -> Response<Body> {
    let json = r#"{"choices":[{"message":{"content":"hello world"},"index":0}],"usage":{"completion_tokens":5,"prompt_tokens":3,"total_tokens":8}}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

async fn start_stub_backend_with_usage() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/v1/completions", post(nonstream_handler_inner));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

async fn start_stub_backend_slow() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(slow_streaming_handler))
        .route("/v1/completions", post(nonstream_handler_inner));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

/// Stub handler that never responds (hangs forever).
async fn hanging_handler(_req: Request<Body>) -> Response<Body> {
    // Sleep for a very long time — the timeout should kill this.
    tokio::time::sleep(Duration::from_secs(60)).await;
    Response::new(Body::empty())
}

/// Stub backend that hangs (never responds within the timeout window).
async fn start_stub_backend_hang() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(hanging_handler))
        .route("/v1/completions", post(hanging_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

// ---------------------------------------------------------------------------
// Proxy builders
// ---------------------------------------------------------------------------

fn build_proxy_with_fifo(backend_url: &str) -> (Router, Arc<metrics::Metrics>) {
    let m = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Fifo,
        4,
        m.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: m.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: Backpressure::default(),
        request_timeout: None,
        context: None,
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(llm_qdisc_proxy::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let admin_router = llm_qdisc_proxy::api::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router)
        .with_state(state);

    (app, m)
}

fn build_proxy_with_drr(backend_url: &str) -> (Router, Arc<metrics::Metrics>, Arc<Scheduler>) {
    let m = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(10.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Drr,
        4,
        m.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let scheduler_arc = Arc::new(scheduler);
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: m.clone(),
        scheduler: scheduler_arc.clone(),
        flow_registry,
        backpressure: Backpressure::default(),
        request_timeout: None,
        context: None,
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(llm_qdisc_proxy::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let admin_router = llm_qdisc_proxy::api::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router)
        .with_state(state);

    (app, m, scheduler_arc)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn collect_chunks(resp: Response<Body>) -> Vec<Bytes> {
    use futures::StreamExt;
    let mut chunks = Vec::new();
    let mut stream = resp.into_body().into_data_stream();
    while let Some(item) = stream.next().await {
        match item {
            Ok(bytes) => chunks.push(bytes),
            Err(e) => panic!("stream error: {}", e),
        }
    }
    chunks
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test: normal streaming completion emits request_completed event
/// and does not emit request_cancelled.
#[tokio::test]
async fn test_normal_completion_emits_completed_event() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m) = build_proxy_with_fifo(&backend_url);

    assert_eq!(m.active_flows.get(), 0.0);

    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty(), "streaming response should have chunks");

    // After completion, gauges should be 0.
    assert_eq!(m.active_flows.get(), 0.0);
    assert_eq!(m.requests_active.get(), 0.0);

    // Verify events.
    let started = m
        .request_events_total
        .with_label_values(&["request_started"])
        .get();
    assert_eq!(started, 1.0, "request_started should be 1");

    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(completed, 1.0, "request_completed should be 1");

    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(cancelled, 0.0, "request_cancelled should be 0");
}

/// Test: client disconnect mid-stream releases the admission slot AND
/// emits request_cancelled event.
#[tokio::test]
async fn test_client_disconnect_releases_slot_and_emits_cancelled() {
    let addr = start_stub_backend_slow().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m) = build_proxy_with_fifo(&backend_url);

    let initial_active = m.active_flows.get();
    let initial_requests = m.requests_active.get();

    // Spawn a task that sends a streaming request and drops it immediately.
    let app_clone = app.clone();
    let drop_handle = tokio::spawn(async move {
        let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app_clone
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        // Drop the response immediately without draining the stream.
        drop(resp);
    });

    drop_handle.await.expect("task should not panic");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // After disconnect, gauges should return to initial.
    assert_eq!(
        m.active_flows.get(),
        initial_active,
        "active_flows should return to initial after disconnect"
    );
    assert_eq!(
        m.requests_active.get(),
        initial_requests,
        "requests_active should return to initial after disconnect"
    );

    // Verify events.
    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(
        cancelled, 1.0,
        "request_cancelled should be 1 on disconnect"
    );

    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(
        completed, 0.0,
        "request_completed should be 0 on disconnect"
    );
}

/// Test: request_completed event emitted for non-streaming path.
#[tokio::test]
async fn test_nonstreaming_completion_emits_completed_event() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m) = build_proxy_with_fifo(&backend_url);

    // Send a non-streaming request.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body_bytes).contains("hello world"));

    // Verify events.
    let started = m
        .request_events_total
        .with_label_values(&["request_started"])
        .get();
    assert_eq!(started, 1.0, "request_started should be 1");

    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(completed, 1.0, "request_completed should be 1");

    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(cancelled, 0.0, "request_cancelled should be 0");
}

/// Test: full streaming response with usage data tracks tokens.
#[tokio::test]
async fn test_streaming_with_usage_tracks_tokens() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m) = build_proxy_with_fifo(&backend_url);

    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty());

    // Stub returns usage with completion_tokens=42.
    let tokens = m.tokens_generated_total.get();
    assert!(
        tokens >= 42.0,
        "tokens_generated_total should be >= 42, got {}",
        tokens
    );

    // Verify token_received events were emitted.
    let token_received = m
        .request_events_total
        .with_label_values(&["token_received"])
        .get();
    assert!(
        token_received >= 1.0,
        "token_received should be >= 1, got {}",
        token_received
    );
}

/// Test: request_timeout config field exists and can be loaded.
#[tokio::test]
async fn test_request_timeout_config_exists() {
    use llm_qdisc_proxy::config::Config;

    // Default config should have request_timeout = None.
    let config = Config::default();
    assert!(
        config.request_timeout.is_none(),
        "default config should have no request_timeout"
    );

    // Config with explicit timeout.
    let config_with_timeout = Config {
        request_timeout: Some(Duration::from_secs(60)),
        ..Default::default()
    };
    assert_eq!(
        config_with_timeout.request_timeout,
        Some(Duration::from_secs(60))
    );
}

/// Test: multiple concurrent requests all get proper lifecycle events.
#[tokio::test]
async fn test_concurrent_requests_lifecycle_events() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m) = build_proxy_with_fifo(&backend_url);

    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true}"#;

    let handles: Vec<_> = (0..3)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/chat/completions")
                            .header("content-type", "application/json")
                            .body(Body::from(body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), 200);
                let _ = collect_chunks(resp).await;
            })
        })
        .collect();

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // All 3 should have emitted request_started and request_completed.
    let started = m
        .request_events_total
        .with_label_values(&["request_started"])
        .get();
    assert_eq!(started, 3.0, "request_started should be 3, got {}", started);

    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(
        completed, 3.0,
        "request_completed should be 3, got {}",
        completed
    );

    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(
        cancelled, 0.0,
        "request_cancelled should be 0, got {}",
        cancelled
    );
}

/// Test: DRR credit is restored on cancel.
///
/// Records the credit BEFORE the request and asserts it returns to
/// approximately the same value after cancel + restore. This would FAIL
/// if no restoration occurs (credit would remain at -100).
#[tokio::test]
async fn test_drr_credit_restored_on_cancel() {
    let addr = start_stub_backend_slow().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m, scheduler) = build_proxy_with_drr(&backend_url);

    // Use the scheduler's credit accessor.
    let flow_id = llm_qdisc_proxy::flow::FlowId::new("test-flow-cancel");

    // Record credit BEFORE the request (baseline).
    let credit_before = scheduler.credit(&flow_id);

    // Send a streaming request and drop it immediately.
    let app_clone = app.clone();
    let drop_handle = tokio::spawn(async move {
        let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":100}"#;
        let resp = app_clone
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("x-llm-flow-id", "test-flow-cancel")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        drop(resp);
    });

    drop_handle.await.expect("task should not panic");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Credit should be restored to near the pre-request value.
    // Without restore, credit would be -100 (admission consumed 100).
    // With restore, credit returns to ~0 (the baseline).
    let credit_after = scheduler.credit(&flow_id);
    let diff = (credit_after - credit_before).abs_diff(0) as i64;
    assert!(
        diff <= 5,
        "credit should be restored to pre-request value, before={}, after={}, diff={}",
        credit_before,
        credit_after,
        diff
    );

    // Verify cancel event.
    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(
        cancelled, 1.0,
        "request_cancelled should be 1 on disconnect"
    );
}

/// Test: usage frame token delivery is tracked correctly.
#[tokio::test]
async fn test_usage_frame_token_delivery_tracked() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m, _scheduler) = build_proxy_with_drr(&backend_url);

    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":100}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty());

    // Verify tokens were tracked from usage frame.
    let tokens = m.tokens_generated_total.get();
    assert!(
        tokens >= 42.0,
        "tokens should be >= 42 (from usage frame), got {}",
        tokens
    );
}

// ---------------------------------------------------------------------------
// Timeout tests
// ---------------------------------------------------------------------------

/// Build a DRR proxy with an explicit request_timeout.
fn build_proxy_with_drr_and_timeout(
    backend_url: &str,
    timeout: Duration,
) -> (Router, Arc<metrics::Metrics>, Arc<Scheduler>) {
    let m = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(10.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Drr,
        4,
        m.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let scheduler_arc = Arc::new(scheduler);
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: m.clone(),
        scheduler: scheduler_arc.clone(),
        flow_registry,
        backpressure: Backpressure::default(),
        request_timeout: Some(timeout),
        context: None,
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(llm_qdisc_proxy::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let admin_router = llm_qdisc_proxy::api::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router)
        .with_state(state);

    (app, m, scheduler_arc)
}

/// Test: timeout cancels the request, releases slot, and restores credit.
///
/// Uses a hanging backend + short timeout. This would FAIL on the old code
/// because request_timeout was never wired — the request would hang forever
/// and the test would time out.
#[tokio::test]
async fn test_timeout_cancels_and_restores_credit() {
    let addr = start_stub_backend_hang().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m, scheduler) =
        build_proxy_with_drr_and_timeout(&backend_url, Duration::from_millis(300));

    let flow_id = llm_qdisc_proxy::flow::FlowId::new("test-timeout-flow");

    // Record pre-request state.
    let credit_before = scheduler.credit(&flow_id);
    let initial_active = m.active_flows.get();
    let initial_requests = m.requests_active.get();

    // Send a streaming request to the hanging backend.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":100}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "test-timeout-flow")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    // The response should be a 408 (request timeout).
    assert_eq!(
        resp.status(),
        408,
        "expected 408 Request Timeout, got {}",
        resp.status()
    );

    // Wait for metrics to settle.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Active flows should return to baseline.
    assert_eq!(
        m.active_flows.get(),
        initial_active,
        "active_flows should return to baseline after timeout"
    );
    assert_eq!(
        m.requests_active.get(),
        initial_requests,
        "requests_active should return to baseline after timeout"
    );

    // Credit should be restored to pre-request value.
    let credit_after = scheduler.credit(&flow_id);
    let diff = (credit_after - credit_before).abs_diff(0) as i64;
    assert!(
        diff <= 5,
        "credit should be restored after timeout, before={}, after={}, diff={}",
        credit_before,
        credit_after,
        diff
    );

    // Verify cancelled event was emitted.
    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(cancelled, 1.0, "request_cancelled should be 1 on timeout");

    // No completed event.
    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(completed, 0.0, "request_completed should be 0 on timeout");
}

/// Test: request_timeout set but backend is fast — request completes normally.
///
/// Verifies that the timeout wrapper does not interfere with normal completion
/// when the backend responds before the timeout expires.
#[tokio::test]
async fn test_timeout_not_triggered_when_backend_is_fast() {
    let addr = start_stub_backend_with_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, m, _scheduler) =
        build_proxy_with_drr_and_timeout(&backend_url, Duration::from_secs(30));

    // Send a streaming request to a fast backend with timeout configured.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "expected 200, got {}", resp.status());
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty(), "streaming response should have chunks");

    // Verify events: completed, not cancelled.
    let completed = m
        .request_events_total
        .with_label_values(&["request_completed"])
        .get();
    assert_eq!(completed, 1.0, "request_completed should be 1");

    let cancelled = m
        .request_events_total
        .with_label_values(&["request_cancelled"])
        .get();
    assert_eq!(cancelled, 0.0, "request_cancelled should be 0");
}
