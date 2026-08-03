//! Tests for the admin API endpoints (issue 09).
//!
//! - `POST /flows` — register and update flows
//! - `GET /queue` — queue status with positions

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use llm_qdisc_proxy::config::Algorithm;
use llm_qdisc_proxy::config::BackpressureMode;
use llm_qdisc_proxy::flow::FlowRegistry;
use llm_qdisc_proxy::gateway;
use llm_qdisc_proxy::metrics;
use llm_qdisc_proxy::scheduler::Scheduler;

/// Shared atomic counter tracking concurrent in-flight requests at the stub.
struct LoadTestState {
    current: AtomicU32,
    peak: AtomicU32,
}

/// Stub handler that holds requests for 200ms.
async fn hold_handler(
    state: axum::extract::State<Arc<LoadTestState>>,
    _req: Request<Body>,
) -> Response<Body> {
    let prev = state.current.fetch_add(1, Ordering::SeqCst);
    let new_val = prev + 1;
    let mut peak = state.peak.load(Ordering::SeqCst);
    while new_val > peak {
        match state
            .peak
            .compare_exchange_weak(peak, new_val, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => break,
            Err(got) => peak = got,
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    state.current.fetch_sub(1, Ordering::SeqCst);

    let json = r#"{"choices":[{"message":{"content":"ok"},"index":0}]}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Build a proxy app with configurable max_active_flows, returning the router
/// and handles for the registry/metrics/load_state.
fn build_admin_test_app(
    _backend_url: &str,
    max_active_flows: u32,
) -> (
    Router,
    Arc<FlowRegistry>,
    Arc<metrics::Metrics>,
    Arc<LoadTestState>,
) {
    let load_state = Arc::new(LoadTestState {
        current: AtomicU32::new(0),
        peak: AtomicU32::new(0),
    });

    // Build the stub backend.
    let backend_app = Router::new()
        .route("/v1/chat/completions", post(hold_handler))
        .with_state(load_state.clone());

    let listener = futures::executor::block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind should succeed");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap() });

    let backend_url_str = format!("http://{}/", addr);

    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Fifo,
        max_active_flows,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(&backend_url_str).expect("valid URL")),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry: flow_registry.clone(),
        backpressure: llm_qdisc_proxy::config::Backpressure::default(),
        request_timeout: None,
        context: None,
    };

    let _ = _backend_url;
    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(llm_qdisc_proxy::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());
    let admin_router = llm_qdisc_proxy::api::create_router().with_state(state);

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router);

    (app, flow_registry, metrics, load_state)
}

/// Collect a response body into a String.
async fn collect_body_string(resp: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// POST /flows tests
// ---------------------------------------------------------------------------

/// Test: register a new flow returns 201 Created.
#[tokio::test]
async fn test_register_new_flow_returns_201() {
    let (app, registry, _metrics, _load_state) = build_admin_test_app("http://localhost/", 4);

    let body = r#"{"id":"new-agent","weight":5.0,"priority":50}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flows")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let text = collect_body_string(resp).await;
    assert!(text.contains(r#""status":"created""#));

    // Verify the flow is in the registry with correct values.
    let flow = registry.get_or_create(llm_qdisc_proxy::flow::FlowId::new("new-agent"));
    assert_eq!(flow.weight(), 5.0);
    assert_eq!(flow.priority(), 50);
}

/// Test: updating an existing flow returns 200 OK and reflects new values.
#[tokio::test]
async fn test_update_existing_flow_returns_200() {
    let (app, registry, _metrics, _load_state) = build_admin_test_app("http://localhost/", 4);

    // Pre-create a flow via get_or_create.
    registry.get_or_create(llm_qdisc_proxy::flow::FlowId::new("existing-agent"));

    let body = r#"{"id":"existing-agent","weight":10.0,"priority":80}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flows")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let text = collect_body_string(resp).await;
    assert!(text.contains(r#""status":"updated""#));

    // Verify the flow has the updated values.
    let flow = registry.get_or_create(llm_qdisc_proxy::flow::FlowId::new("existing-agent"));
    assert_eq!(flow.weight(), 10.0);
    assert_eq!(flow.priority(), 80);
}

/// Test: invalid weight (<=0) returns 400.
#[tokio::test]
async fn test_register_invalid_weight_returns_400() {
    let (app, _registry, _metrics, _load_state) = build_admin_test_app("http://localhost/", 4);

    let body = r#"{"id":"bad-flow","weight":0,"priority":50}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flows")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let text = collect_body_string(resp).await;
    assert!(text.contains("weight"));
}

/// Test: invalid priority (>100) returns 400.
#[tokio::test]
async fn test_register_invalid_priority_returns_400() {
    let (app, _registry, _metrics, _load_state) = build_admin_test_app("http://localhost/", 4);

    let body = r#"{"id":"bad-flow","weight":1,"priority":101}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flows")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let text = collect_body_string(resp).await;
    assert!(text.contains("priority"));
}

// ---------------------------------------------------------------------------
// GET /queue tests
// ---------------------------------------------------------------------------

/// Test: GET /queue returns empty response when no requests are in flight.
#[tokio::test]
async fn test_queue_empty() {
    let (app, _registry, _metrics, _load_state) = build_admin_test_app("http://localhost/", 4);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/queue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let text = collect_body_string(resp).await;
    assert!(text.contains(r#""active":0"#));
    assert!(text.contains(r#""waiting":0"#));
    assert!(text.contains(r#""flows":[]"#));
}

/// Test: GET /queue reflects active count and waiting flows under load.
///
/// With max_active_flows=2, fire 3 requests concurrently. The third will
/// be waiting. GET /queue should show active=2, waiting=1.
#[tokio::test]
async fn test_queue_under_load() {
    let (app, _registry, _metrics, load_state) = build_admin_test_app("http://localhost/", 2);

    // Reset counters.
    load_state.current.store(0, Ordering::SeqCst);
    load_state.peak.store(0, Ordering::SeqCst);

    // Fire 3 concurrent requests. The first 2 get active slots; the 3rd waits.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;

    let mut handles = Vec::new();
    for i in 0..3 {
        let app = app.clone();
        let handle = tokio::spawn(async move {
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
            let _ = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap();
            i
        });
        handles.push(handle);
    }

    // Give the requests time to start (the stub backend holds for 200ms).
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Query the queue endpoint.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/queue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let text = collect_body_string(resp).await;

    // active should be 2 (max_active_flows), waiting should be >= 1.
    assert!(
        text.contains(r#""active":2"#),
        "active should be 2, got: {}",
        text
    );
    assert!(
        text.contains(r#""waiting":1"#)
            || text.contains(r#""waiting":2"#)
            || text.contains(r#""waiting":3"#),
        "waiting should be > 0, got: {}",
        text
    );

    // Flows list should be non-empty.
    assert!(
        !text.contains(r#""flows":[]"#),
        "flows should not be empty under load, got: {}",
        text
    );

    // Wait for all requests to complete.
    let _results: Vec<_> = futures::future::join_all(handles).await;

    // After all complete, the queue should be empty again.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/queue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let text = collect_body_string(resp).await;
    assert!(text.contains(r#""waiting":0"#));
}
