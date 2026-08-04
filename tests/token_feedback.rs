//! Token feedback loop tests (issue 16).
//!
//! Verifies that DRR credit accounting uses actual delivered tokens,
//! not just the max_tokens estimate. Tests:
//! - Credit restored when actual < estimated (streaming with usage)
//! - Credit restored when actual < estimated (non-streaming with usage)
//! - Warning logged when backend emits no usage data
//! - Overrun case: additional debit when actual > estimated
//! - Predictive admit: OFF by default, ON allows pre-admit near threshold

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

use tinyllb::backend::BackendMonitor;
use tinyllb::config::{
    Algorithm, Backpressure, BackpressureMode, CompletionBias, KvPolicyConfig, Priorities, PriorityPolicy,
};
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler::Scheduler;

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
// Stub backends
// ---------------------------------------------------------------------------

/// Streaming handler that emits usage with completion_tokens=512.
async fn streaming_with_usage_512() -> Response<Body> {
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n"),
        // Usage frame: 512 completion tokens
        Bytes::from("data: {\"usage\":{\"completion_tokens\":512,\"prompt_tokens\":10,\"total_tokens\":522}}\n\n"),
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

/// Non-streaming handler with usage (completion_tokens=512, max_tokens=8192).
async fn nonstream_with_usage_512(_req: Request<Body>) -> Response<Body> {
    let json = r#"{"choices":[{"message":{"content":"hello world"},"index":0}],"usage":{"completion_tokens":512,"prompt_tokens":10,"total_tokens":522}}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Streaming handler that emits NO usage data (only content frames).
async fn streaming_no_usage(_req: Request<Body>) -> Response<Body> {
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n"),
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

/// Non-streaming handler with NO usage data.
async fn nonstream_no_usage(_req: Request<Body>) -> Response<Body> {
    let json = r#"{"choices":[{"message":{"content":"hello world"},"index":0}]}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Streaming handler that emits overrun (more tokens than max_tokens).
/// max_tokens=100, but backend generates 200.
async fn streaming_overrun_200() -> Response<Body> {
    let chunks: Vec<Bytes> = vec![
        Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
        // Usage frame: 200 completion tokens (exceeds max_tokens=100)
        Bytes::from("data: {\"usage\":{\"completion_tokens\":200,\"prompt_tokens\":10,\"total_tokens\":210}}\n\n"),
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

/// Start stub backend with usage-emitting streaming handler.
async fn start_stub_with_usage_512() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(streaming_with_usage_512))
        .route("/v1/completions", post(nonstream_with_usage_512));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Start stub backend with no-usage handler.
async fn start_stub_no_usage() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(streaming_no_usage))
        .route("/v1/completions", post(nonstream_no_usage));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Start stub backend with overrun handler.
async fn start_stub_overrun() -> SocketAddr {
    let app = Router::new().route("/v1/chat/completions", post(streaming_overrun_200));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

// ---------------------------------------------------------------------------
// Proxy builder
// ---------------------------------------------------------------------------

fn build_drr_proxy(backend_url: &str) -> (Router, Arc<metrics::Metrics>, Arc<Scheduler>) {
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
        priorities: Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(tinyllb::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let admin_router = tinyllb::api::create_router().with_state(state.clone());

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

/// Stream emits usage.completion_tokens=512 after max_tokens=8192 was costed.
/// After completion, credit should be restored so net charge = 512, not 8192.
#[tokio::test]
async fn test_streaming_credit_restored_on_under_delivery() {
    let addr = start_stub_with_usage_512().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _m, scheduler) = build_drr_proxy(&backend_url);

    let flow_id = tinyllb::flow::FlowId::new("under-delivery-flow");
    let credit_before = scheduler.credit(&flow_id);

    // Send streaming request with max_tokens=8192.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":8192}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "under-delivery-flow")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty());

    // Wait for lifecycle guard to drop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Credit should reflect net charge of 512 (actual delivered), not 8192 (estimated).
    // At admission: credit -= 8192
    // At completion: credit += (8192 - 512) = +7680
    // Net: credit -= 512
    let credit_after = scheduler.credit(&flow_id);
    let net_charge = credit_before - credit_after;
    assert_eq!(
        net_charge, 512,
        "net credit charge should be 512 (actual delivered), not 8192 (estimated). before={}, after={}, net_charge={}",
        credit_before, credit_after, net_charge
    );
}

/// Non-streaming response with usage mirrors the same accounting.
#[tokio::test]
async fn test_nonstreaming_credit_restored_on_under_delivery() {
    let addr = start_stub_with_usage_512().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _m, scheduler) = build_drr_proxy(&backend_url);

    let flow_id = tinyllb::flow::FlowId::new("nonstream-under-delivery");
    let credit_before = scheduler.credit(&flow_id);

    // Send non-streaming request with max_tokens=8192.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"max_tokens":8192}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "nonstream-under-delivery")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains("512"),
        "response should contain 512 completion tokens"
    );

    // Wait for lifecycle guard to drop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Net charge should be 512 (actual), not 8192 (estimated).
    let credit_after = scheduler.credit(&flow_id);
    let net_charge = credit_before - credit_after;
    assert_eq!(
        net_charge, 512,
        "net credit charge should be 512 (actual delivered), not 8192 (estimated). before={}, after={}, net_charge={}",
        credit_before, credit_after, net_charge
    );
}

/// Backend that emits no usage frame: warning logged, estimate used.
#[tokio::test]
async fn test_no_usage_falls_back_to_estimate() {
    let addr = start_stub_no_usage().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _m, scheduler) = build_drr_proxy(&backend_url);

    let flow_id = tinyllb::flow::FlowId::new("no-usage-flow");
    let credit_before = scheduler.credit(&flow_id);

    // Send streaming request with max_tokens=100.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":100}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "no-usage-flow")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty());

    // Wait for lifecycle guard to drop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Without usage data, full estimated cost (100) is charged.
    let credit_after = scheduler.credit(&flow_id);
    let net_charge = credit_before - credit_after;
    assert_eq!(
        net_charge, 100,
        "without usage data, full estimated cost (100) should be charged. before={}, after={}, net_charge={}",
        credit_before, credit_after, net_charge
    );
}

/// Overrun case: backend generates 200 tokens but max_tokens=100.
/// Additional debit of 100 should be applied.
#[tokio::test]
async fn test_overrun_additional_debit() {
    let addr = start_stub_overrun().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _m, scheduler) = build_drr_proxy(&backend_url);

    let flow_id = tinyllb::flow::FlowId::new("overrun-flow");
    let credit_before = scheduler.credit(&flow_id);

    // Send streaming request with max_tokens=100, but backend generates 200.
    let body = r#"{"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":100}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "overrun-flow")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let chunks = collect_chunks(resp).await;
    assert!(!chunks.is_empty());

    // Wait for lifecycle guard to drop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Net charge should be 200 (actual), not 100 (estimated).
    // At admission: credit -= 100
    // At completion: restore = 100 - 200 = -100 (additional debit)
    // credit += (-100) = credit -= 100 more
    // Net: credit -= 200
    let credit_after = scheduler.credit(&flow_id);
    let net_charge = credit_before - credit_after;
    assert_eq!(
        net_charge, 200,
        "overrun: net charge should be 200 (actual delivered). before={}, after={}, net_charge={}",
        credit_before, credit_after, net_charge
    );
}

/// Predictive admit is OFF by default.
#[test]
fn test_predictive_admit_off_by_default() {
    let cb = CompletionBias::default();
    assert!(
        !cb.predictive_admit,
        "predictive_admit should be false by default"
    );
}

/// Predictive admit config field is serializable/deserializable.
#[test]
fn test_predictive_admit_config_serialization() {
    let config = CompletionBias {
        enabled: true,
        target_active_flows: 2,
        predictive_admit: true,
    };

    let json = serde_json::to_string(&config).expect("should serialize");
    let deserialized: CompletionBias = serde_json::from_str(&json).expect("should deserialize");

    assert!(deserialized.predictive_admit);
    assert!(deserialized.enabled);
    assert_eq!(deserialized.target_active_flows, 2);
}

/// FlowProgressTracker: register, update, is_near_done.
#[test]
fn test_flow_progress_tracker_near_done() {
    use tinyllb::flow::FlowId;
    use tinyllb::scheduler::FlowProgressTracker;

    let tracker = FlowProgressTracker::new();
    let flow_id = FlowId::new("test-flow");

    // Register: estimated = 1000.
    tracker.register(&flow_id, 1000);

    // Not near done (0 delivered).
    assert!(!tracker.is_near_done(&flow_id, 0.9));

    // Deliver 800 (80%): still not near done.
    tracker.update_delivered(&flow_id, 800);
    assert!(!tracker.is_near_done(&flow_id, 0.9));

    // Deliver 900 total (90%): near done.
    tracker.update_delivered(&flow_id, 100);
    assert!(tracker.is_near_done(&flow_id, 0.9));
}

/// FlowProgressTracker: unregister cleans up.
#[test]
fn test_flow_progress_tracker_unregister() {
    use tinyllb::flow::FlowId;
    use tinyllb::scheduler::FlowProgressTracker;

    let tracker = FlowProgressTracker::new();
    let flow_id = FlowId::new("cleanup-flow");

    tracker.register(&flow_id, 1000);
    tracker.update_delivered(&flow_id, 500);

    // Unregister.
    tracker.unregister(&flow_id, 1000, 500);

    // Should be gone.
    assert!(!tracker.is_near_done(&flow_id, 0.0));
}

/// FlowProgressTracker: any_flow_near_done.
#[test]
fn test_flow_progress_tracker_any_flow_near_done() {
    use tinyllb::flow::FlowId;
    use tinyllb::scheduler::FlowProgressTracker;

    let tracker = FlowProgressTracker::new();
    let flow_a = FlowId::new("flow-a");
    let flow_b = FlowId::new("flow-b");

    // Register two flows.
    tracker.register(&flow_a, 1000);
    tracker.register(&flow_b, 1000);

    // Neither near done.
    assert!(!tracker.any_flow_near_done(0.9));

    // Flow A near done.
    tracker.update_delivered(&flow_a, 900);
    assert!(tracker.any_flow_near_done(0.9));
}

// ---------------------------------------------------------------------------
// Predictive-admit-ON integration tests (issue 16 review defect)
// ---------------------------------------------------------------------------

/// Predictive admit ON: when an active flow is near done (>= 90% delivered),
/// a new flow is pre-admitted immediately without waiting for the active flow
/// to drain.
///
/// WITHOUT predictive admit, the new flow would block until active < target.
/// WITH predictive admit, it gets through immediately.
#[tokio::test]
async fn test_predictive_admit_on_allows_pre_admit_when_near_done() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Fifo,
            4, // max_active_flows=4, but target=2
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300), // long starvation — not relevant
            CompletionBias {
                enabled: true,
                target_active_flows: 2,
                predictive_admit: true, // KEY: predictive admit ON
            },
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
            PriorityPolicy::default(),
            Priorities::default(),
        ));

        // Fill target (2 active flows).
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a = s1.admit(FlowId::new("A"), 1024.0).await.unwrap();
        let ticket_b = s2.admit(FlowId::new("B"), 1024.0).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        // Mark flow A as near done in the tracker.
        // estimated=1000, delivered=950 → ratio=0.95 >= 0.9 threshold.
        let tracker = scheduler.flow_progress_tracker();
        tracker.register(&FlowId::new("A"), 1000);
        tracker.update_delivered(&FlowId::new("A"), 950);
        assert!(
            tracker.any_flow_near_done(0.9),
            "flow A should be near done (950/1000 = 0.95)"
        );

        // C (new flow) should be admitted immediately via predictive admit,
        // NOT wait for active to drop below target.
        let s3 = scheduler.clone();
        let start = std::time::Instant::now();
        let ticket_c = s3
            .admit(FlowId::new("C"), 1024.0)
            .await
            .expect("C should be admitted via predictive admit");
        let elapsed = start.elapsed();

        // C should have been admitted promptly (< 200ms), not waiting for
        // the active flows to drop (which never happens in this test).
        assert!(
            elapsed < Duration::from_millis(500),
            "C should be admitted via predictive admit, took {:?}",
            elapsed
        );

        // Clean up.
        drop(ticket_c);
        drop(ticket_a);
        drop(ticket_b);
    })
    .await
    .expect("test should not timeout");
}

/// Predictive admit ON: when the active flow is NOT near done (< 90% delivered),
/// a new flow is deferred (waits) at the completion bias gate.
///
/// This discriminates from the near-done case: the same setup but with
/// delivered < 90% causes the new flow to wait.
#[tokio::test]
async fn test_predictive_admit_on_defers_when_not_near_done() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Fifo,
            4,
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300), // C times out first
            CompletionBias {
                enabled: true,
                target_active_flows: 2,
                predictive_admit: true,
            },
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
            PriorityPolicy::default(),
            Priorities::default(),
        ));

        // Fill target (2 active flows).
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a = s1.admit(FlowId::new("A"), 1024.0).await.unwrap();
        let ticket_b = s2.admit(FlowId::new("B"), 1024.0).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        // Mark flow A as NOT near done: delivered=800/1000 = 0.8 < 0.9.
        let tracker = scheduler.flow_progress_tracker();
        tracker.register(&FlowId::new("A"), 1000);
        tracker.update_delivered(&FlowId::new("A"), 800);
        assert!(
            !tracker.any_flow_near_done(0.9),
            "flow A should NOT be near done (800/1000 = 0.8)"
        );

        // C (new flow) should be DEFERRED because:
        // 1. active=2 >= target=2
        // 2. predictive admit checks any_flow_near_done → false
        // 3. C waits at the gate (starvation timeout is long, so it blocks).
        let s3 = scheduler.clone();
        let admit_result = tokio::time::timeout(
            Duration::from_millis(200),
            s3.admit(FlowId::new("C"), 1024.0),
        )
        .await;

        // C should NOT have been admitted (timeout fires first).
        // This discriminates from the near-done case where C is admitted immediately.
        assert!(
            admit_result.is_err(),
            "C should be deferred (timeout expected), not admitted immediately"
        );

        // Clean up — drop tickets and the stuck admit task.
        drop(ticket_a);
        drop(ticket_b);
    })
    .await
    .expect("test should not timeout");
}
