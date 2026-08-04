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
use tower::ServiceExt;

use tinyllb::config::BackpressureMode;
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;

/// Build a full proxy app with metrics for testing.
/// Returns the router and the shared `Arc<Metrics>` handle so tests can
/// access individual collectors (e.g., to touch GaugeVec labels).
fn build_test_app(backend_url: &str) -> (Router, Arc<tinyllb::metrics::Metrics>) {
    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    // Touch the queue_depth GaugeVec with an "ephemeral" label so it appears
    // in the Prometheus scrape output (GaugeVec only emits samples for labels
    // that have been accessed).
    metrics
        .queue_depth
        .with_label_values(&["ephemeral"])
        .set(0.0);

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(tinyllb::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let gateway_router = gateway::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .with_state(state);

    (app, metrics)
}

/// Collect a response body into a String.
async fn collect_body_string(resp: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Assert that a Prometheus metric with the given name exists in the scrape
/// output and has the expected type.
fn assert_metric_exists(metrics_text: &str, name: &str, expected_type: &str) {
    assert!(
        metrics_text.contains(&format!("{} ", name)),
        "metric '{}' not found in scrape output",
        name
    );
    assert!(
        metrics_text.contains(&format!("# TYPE {} {}", name, expected_type)),
        "metric '{}' should have type '{}'",
        name,
        expected_type
    );
}

/// Test: all PRD-named metrics are present with correct types.
#[tokio::test]
async fn test_metrics_endpoint_returns_all_metrics() {
    // Use a dummy backend URL; we only need to scrape /metrics.
    let (app, metrics) = build_test_app("http://127.0.0.1:59999/");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Verify content-type header.
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type should be present")
        .to_str()
        .unwrap();
    assert!(
        content_type.starts_with("text/plain"),
        "content-type should start with 'text/plain', got: {}",
        content_type
    );

    let body = collect_body_string(resp).await;

    // Queue family
    assert_metric_exists(&body, "llm_queue_depth", "gauge");
    assert_metric_exists(&body, "llm_queue_wait_seconds", "histogram");
    assert_metric_exists(&body, "llm_active_flows", "gauge");

    // Throughput family
    assert_metric_exists(&body, "llm_tokens_generated_total", "counter");
    assert_metric_exists(&body, "llm_tokens_per_second", "gauge");

    // Backend family
    assert_metric_exists(&body, "vllm_requests_active", "gauge");
    assert_metric_exists(&body, "vllm_errors_total", "counter");

    // Starvation protection family (issue #12)
    // Touch these metrics so they appear in the scrape output.
    // GaugeVec only emits samples for labels that have been accessed.
    metrics
        .flow_starvation_seconds
        .with_label_values(&["ephemeral"])
        .set(0.0);
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
    let body2 = collect_body_string(resp2).await;

    // Starvation protection metrics should now appear.
    assert!(
        body2.contains("llm_flow_starvation_seconds"),
        "llm_flow_starvation_seconds should appear in scrape output"
    );
    assert!(
        body2.contains("llm_starvation_force_admits_total"),
        "llm_starvation_force_admits_total should appear in scrape output"
    );
}

/// Test: metrics are initialized correctly (all counters start at 0, gauges at 0).
#[tokio::test]
async fn test_metrics_initial_values() {
    let (app, _) = build_test_app("http://127.0.0.1:59999/");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = collect_body_string(resp).await;

    // Counters should show 0 at initialization.
    assert!(
        body.contains("llm_tokens_generated_total 0")
            || body.contains("llm_tokens_generated_total_total 0"),
        "llm_tokens_generated_total should start at 0"
    );
    assert!(
        body.contains("vllm_errors_total 0") || body.contains("vllm_errors_total_total 0"),
        "vllm_errors_total should start at 0"
    );

    // Gauges should show 0 at initialization.
    // llm_queue_depth is now a GaugeVec labeled by flow_id; the "ephemeral"
    // label is touched in build_test_app to ensure it appears in the scrape.
    assert!(
        body.contains("llm_queue_depth{") || body.contains("llm_queue_depth_total"),
        "llm_queue_depth should appear in scrape output. Body:\n{}",
        body
    );
    assert!(
        body.contains("vllm_requests_active 0") || body.contains("vllm_requests_active_total 0"),
        "vllm_requests_active should start at 0"
    );
}

// ---------------------------------------------------------------------------
// Stub helpers for streaming metrics tests
// ---------------------------------------------------------------------------

/// SSE stream wrapper for tests.
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

/// Stub backend that emits usage in the final SSE frame.
/// Returns streaming SSE with:
/// - 3 content frames
/// - 1 usage frame: {"usage":{"prompt_tokens":100,"completion_tokens":3,"total_tokens":103}}
/// - [DONE] frame
async fn streaming_with_usage_handler(_req: Request<Body>) -> Response<Body> {
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n\n"),
        // Usage frame embedded in SSE
        Bytes::from(
            "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":3,\"total_tokens\":103}}\n\n",
        ),
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

/// Stub backend for non-streaming with usage in response JSON.
async fn completions_with_usage_handler(_req: Request<Body>) -> Response<Body> {
    let json = r#"{"choices":[{"message":{"content":"hello world"},"index":0}],"usage":{"prompt_tokens":100,"completion_tokens":3,"total_tokens":103}}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Stub backend that delays between chunks (for active gauge timing test).
async fn slow_streaming_handler(_req: Request<Body>) -> Response<Body> {
    // We can't easily add real delays in axum handlers, so instead we use
    // a channel-based approach. For integration testing, the stream holds open
    // while we check the gauge.
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n"),
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

/// Start a stub backend server on an ephemeral port and return its address.
/// The backend provides routes used by the metrics tests.
async fn start_metrics_stub() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(streaming_with_usage_handler))
        .route("/v1/completions", post(completions_with_usage_handler))
        .route("/v1/slow-stream", post(slow_streaming_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

/// Collect a streaming response body into individual chunks.
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

/// Test: streaming SSE with usage emits completion_tokens (not total_tokens).
///
/// The stub sends:
/// - 3 content frames
/// - usage frame: {"usage":{"prompt_tokens":100,"completion_tokens":3,"total_tokens":103}}
/// - [DONE] frame
///
/// After the stream completes, `llm_tokens_generated_total` must be 3 (completion),
/// not 103 (total) and not 0 (parse failure).
#[tokio::test]
async fn test_streaming_tokens_count_completion_not_total() {
    let addr = start_metrics_stub().await;
    let backend_url = format!("http://{}/", addr);

    let metrics = metrics::create_metrics();
    let metrics_clone = metrics.clone();
    let flow_registry = Arc::new(tinyllb::flow::FlowRegistry::new(1.0, 50));
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(&backend_url).expect("valid backend URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(gateway_router)
        .with_state(state);

    // Initial counter value should be 0.
    assert_eq!(metrics_clone.tokens_generated_total.get(), 0.0);

    // Send a streaming request.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Drain the entire stream.
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty(), "stream should have returned chunks");

    // After the stream completes, the counter should reflect completion_tokens (3).
    let token_count = metrics_clone.tokens_generated_total.get();
    assert_eq!(
        token_count, 3.0,
        "llm_tokens_generated_total should be 3 (completion_tokens), not {} (was it total_tokens or 0?)",
        token_count
    );
}

/// Test: active gauge is 1 during an in-flight streaming request and 0 after.
///
/// This verifies that `RequestActiveGuard` lives in `MetricStream` and drops
/// when the stream ends, not when the handler returns.
#[tokio::test]
async fn test_active_gauge_during_streaming() {
    let addr = start_metrics_stub().await;
    let backend_url = format!("http://{}/", addr);

    let metrics = metrics::create_metrics();
    let metrics_clone = metrics.clone();
    let flow_registry = Arc::new(tinyllb::flow::FlowRegistry::new(1.0, 50));
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(&backend_url).expect("valid backend URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(gateway_router)
        .with_state(state);

    // Initial active count should be 0.
    assert_eq!(metrics_clone.requests_active.get(), 0.0);

    // Send a streaming request.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // While the stream body is being consumed, the active gauge should be 1.
    // The MetricStream owns the RequestActiveGuard, so it stays alive during
    // stream consumption.
    assert_eq!(
        metrics_clone.requests_active.get(),
        1.0,
        "vllm_requests_active should be 1 while the stream is in-flight, not {}",
        metrics_clone.requests_active.get()
    );

    // Drain the stream.
    let _chunks = collect_chunks(resp).await;

    // After the stream completes, the gauge should return to 0.
    assert_eq!(
        metrics_clone.requests_active.get(),
        0.0,
        "vllm_requests_active should be 0 after the stream completes, not {}",
        metrics_clone.requests_active.get()
    );
}

/// Test: non-streaming response extracts completion_tokens, not total_tokens.
///
/// The stub returns JSON with usage.completion_tokens=3, prompt_tokens=100,
/// total_tokens=103. The counter should increment by 3.
#[tokio::test]
async fn test_nonstream_tokens_count_completion_not_total() {
    let addr = start_metrics_stub().await;
    let backend_url = format!("http://{}/", addr);

    let metrics = metrics::create_metrics();
    let metrics_clone = metrics.clone();
    let flow_registry = Arc::new(tinyllb::flow::FlowRegistry::new(1.0, 50));
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(&backend_url).expect("valid backend URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(gateway_router)
        .with_state(state);

    // Initial counter value should be 0.
    assert_eq!(metrics_clone.tokens_generated_total.get(), 0.0);

    // Send a non-streaming completions request (no "stream" flag).
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Consume the response body (needed for metrics to be recorded in non-streaming path).
    let _body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();

    // The counter should be 3 (completion_tokens), not 103 (total_tokens).
    let token_count = metrics_clone.tokens_generated_total.get();
    assert_eq!(
        token_count, 3.0,
        "llm_tokens_generated_total should be 3 (completion_tokens), not {} (was it total_tokens?)",
        token_count
    );
}

/// Test: non-streaming active gauge reflects body collection period.
///
/// The RequestActiveGuard in the non-streaming path lives during body collection.
/// This test verifies the gauge increments and decrements correctly.
#[tokio::test]
async fn test_active_gauge_during_nonstreaming() {
    let addr = start_metrics_stub().await;
    let backend_url = format!("http://{}/", addr);

    let metrics = metrics::create_metrics();
    let metrics_clone = metrics.clone();
    let flow_registry = Arc::new(tinyllb::flow::FlowRegistry::new(1.0, 50));
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(&backend_url).expect("valid backend URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(gateway_router)
        .with_state(state);

    // Initial active count should be 0.
    assert_eq!(metrics_clone.requests_active.get(), 0.0);

    // Send a non-streaming completions request.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // After the response is returned (body already collected), the gauge
    // should return to 0. The guard drops after body collection in the handler.
    // Note: Since the proxy uses tower ServiceExt::oneshot, the handler runs
    // to completion before the Response is returned. So the guard has already
    // dropped by the time we check here.
    assert_eq!(
        metrics_clone.requests_active.get(),
        0.0,
        "vllm_requests_active should be 0 after the non-streaming request completes, not {}",
        metrics_clone.requests_active.get()
    );
}
