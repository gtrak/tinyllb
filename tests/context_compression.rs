use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// Recorded request from the mock backend.
#[derive(Clone, Debug)]
struct RecordedRequest {
    messages: serde_json::Value,
    is_compressor: bool,
    flow_id_header: Option<String>,
}

/// Shared state for the mock backend to record incoming requests.
struct MockBackendState {
    requests: Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
}

impl MockBackendState {
    fn new() -> Self {
        Self {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// Mock backend handler that records requests and responds.
async fn mock_backend_handler(
    state: Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
    req: Request<Body>,
) -> axum::response::Json<serde_json::Value> {
    let is_compressor = req
        .headers()
        .get("x-llm-internal")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "compressor")
        .unwrap_or(false);

    let flow_id = req
        .headers()
        .get("x-llm-flow-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();

    let json_body: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or_default();
    let messages = json_body.get("messages").cloned().unwrap_or_default();

    state.lock().unwrap().push(RecordedRequest {
        messages,
        is_compressor,
        flow_id_header: flow_id,
    });

    if is_compressor {
        axum::response::Json(json!({
            "choices": [{
                "message": { "content": "Summary of turns: user discussed testing." },
                "index": 0
            }]
        }))
    } else {
        // Normal assistant response.
        axum::response::Json(json!({
            "choices": [{
                "message": { "content": "assistant response" },
                "index": 0
            }]
        }))
    }
}

/// Models endpoint handler.
async fn mock_models_handler() -> axum::response::Json<serde_json::Value> {
    axum::response::Json(json!({ "data": [] }))
}

/// Start a mock backend server on an ephemeral port.
/// Returns the base URL and the shared state for recording requests.
async fn start_mock_backend() -> (String, MockBackendState) {
    let state = MockBackendState::new();
    let requests = state.requests.clone();

    let requests1 = requests.clone();
    let chat_handler = move |req: Request<Body>| {
        let state = requests1.clone();
        async move { mock_backend_handler(state, req).await }
    };

    let requests2 = requests.clone();
    let completions_handler = move |req: Request<Body>| {
        let state = requests2.clone();
        async move { mock_backend_handler(state, req).await }
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/completions", post(completions_handler))
        .route("/v1/models", get(mock_models_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start.
    tokio::time::sleep(Duration::from_millis(100)).await;

    (base_url, state)
}

/// Build a proxy app with context compression enabled.
/// The proxy and compression worker both use the SAME backend URL.
async fn build_compression_test_app(
    threshold: usize,
    enabled: bool,
) -> (Router, String, MockBackendState) {
    let (backend_url, mock_state) = start_mock_backend().await;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let policy = tinyllb::config::ContextPolicy {
        enabled,
        compress_threshold: threshold,
        head_keep_turns: 2,
        live_keep_turns: 2,
        compress_chunk_turns: 2,
        store_path: format!(
            "{}/test_compress_{}.db",
            std::env::temp_dir().to_string_lossy(),
            uuid::Uuid::new_v4()
        ),
        ..Default::default()
    };

    let metrics = Arc::new(tinyllb::metrics::Metrics::new());
    let ctx = if enabled {
        let ctx = tinyllb::context::ContextState::new(
            policy.clone(),
            tx,
            metrics.clone(),
        )
        .await
        .expect("create context state");
        Some(Arc::new(ctx))
    } else {
        None
    };

    // Spawn the compression worker if enabled.
    if let Some(ref ctx) = ctx {
        let worker = tinyllb::context::compressor::CompressionWorker::new(
            rx,
            Arc::clone(ctx),
            url::Url::parse(&backend_url).unwrap(),
            reqwest::Client::new(),
        );
        tokio::spawn(async move { worker.run().await });
    } else {
        // Drain the channel to prevent leaks.
        tokio::spawn(async move {
            let mut rx = rx;
            while rx.recv().await.is_some() {}
        });
    }

    // Build scheduler and app state.
    let flow_registry = Arc::new(
        tinyllb::flow::FlowRegistry::new(1.0, 50),
    );
    let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Fifo,
        4,
        metrics.clone(),
        flow_registry.clone(),
        tinyllb::config::BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );

    let app_state = tinyllb::gateway::AppState {
        client: tinyllb::gateway::build_client(),
        backend_url: Arc::new(
            url::Url::parse(&backend_url).expect("valid backend URL"),
        ),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: ctx,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = tinyllb::gateway::create_router()
        .with_state(app_state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(tinyllb::metrics::endpoint::metrics_handler),
        )
        .with_state(app_state.clone());
    let admin_router = tinyllb::api::create_router().with_state(app_state);

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router);

    (app, backend_url, mock_state)
}

/// Send a chat completion request to the proxy.
async fn send_chat_request(
    router: &Router,
    flow_id: &str,
    messages: Vec<serde_json::Value>,
) -> Response<Body> {
    let body = json!({
        "model": "test",
        "messages": messages,
        "stream": false,
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("X-LLM-Flow-ID", flow_id)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

/// Wait for compression to complete by polling the admin endpoint.
/// Returns true if compression was observed, false on timeout.
async fn wait_for_compression(
    router: &Router,
    flow_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        let request = Request::builder()
            .method("GET")
            .uri(format!("/admin/context/{}", flow_id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(request).await.unwrap();
        if resp.status() == 200 {
            let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let detail: serde_json::Value =
                serde_json::from_slice(&body).unwrap();
            let compressed_count = detail["segments"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["kind"] == "compressed")
                .count();
            if compressed_count > 0 {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait for the mock backend to receive a compressor request.
async fn wait_for_compressor_request(
    mock_state: &MockBackendState,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        {
            let requests = mock_state.requests.lock().unwrap();
            if requests.iter().any(|r| r.is_compressor) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn test_disabled_no_compression() {
    let (app, _backend_url, _state) = build_compression_test_app(1, false).await;

    let flow_id = "flow-disabled";

    // Send many requests — well over any threshold.
    for i in 0..10 {
        let messages = vec![
            json!({ "role": "user", "content": format!("question {}", i) }),
            json!({ "role": "assistant", "content": format!("answer {}", i) }),
        ];
        let resp = send_chat_request(&app, flow_id, messages).await;
        assert_eq!(
            resp.status(),
            200,
            "proxy should return 200 even without context",
        );
    }

    // Admin endpoints should return 503 when context is None.
    let request = Request::builder()
        .method("GET")
        .uri(format!("/admin/context/{}", flow_id))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        resp.status(),
        503,
        "admin endpoint should return 503 when context is disabled",
    );

    // Also check the list endpoint.
    let request = Request::builder()
        .method("GET")
        .uri("/admin/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        resp.status(),
        503,
        "admin list endpoint should return 503 when context is disabled",
    );
}

#[tokio::test]
async fn test_admin_get_context_detail() {
    let (app, _backend_url, _state) =
        build_compression_test_app(1_000_000, true).await;

    let flow_id = "flow-admin-detail";

    // Send 3 requests to build up a transcript.
    for i in 0..3 {
        let messages = vec![
            json!({ "role": "user", "content": format!("question {}", i) }),
            json!({ "role": "assistant", "content": format!("answer {}", i) }),
        ];
        let resp = send_chat_request(&app, flow_id, messages).await;
        assert_eq!(resp.status(), 200);
    }

    // Small delay for store writes to complete.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // GET /admin/context/{flow_id} should return 200 with segments.
    let request = Request::builder()
        .method("GET")
        .uri(format!("/admin/context/{}", flow_id))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "admin context detail should return 200",
    );

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Verify structure.
    assert_eq!(detail["flow_id"], flow_id);
    let segments = detail["segments"].as_array().expect("segments is array");
    assert!(!segments.is_empty(), "should have at least one segment");

    // Should have at least head + live segments.
    let has_head = segments
        .iter()
        .any(|s| s["kind"] == "head");
    let has_live = segments
        .iter()
        .any(|s| s["kind"] == "live");
    assert!(has_head, "should have a head segment");
    assert!(has_live, "should have a live segment");

    // total_est_tokens should be > 0.
    let total_est = detail["total_est_tokens"]
        .as_i64()
        .expect("total_est_tokens is i64");
    assert!(total_est > 0, "total_est_tokens should be positive");
}

#[tokio::test]
async fn test_metrics_exposed() {
    let (app, _backend_url, _state) =
        build_compression_test_app(1_000_000, true).await;

    let flow_id = "flow-metrics";

    // Send a request to trigger reconciliation (which sets the gauge).
    let messages = vec![
        json!({ "role": "user", "content": "hello" }),
        json!({ "role": "assistant", "content": "world" }),
    ];
    send_chat_request(&app, flow_id, messages).await;

    // Small delay for gauge update.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // GET /metrics.
    let request = Request::builder()
        .method("GET")
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let metrics_text = String::from_utf8(body.to_vec()).unwrap();

    // Verify compression metrics are present.
    assert!(
        metrics_text.contains("tinyllb_context_compression_events_total"),
        "metrics should contain compression_events_total",
    );
    assert!(
        metrics_text.contains("tinyllb_context_estimated_tokens"),
        "metrics should contain estimated_tokens",
    );
    assert!(
        metrics_text.contains("tinyllb_context_compression_errors_total"),
        "metrics should contain compression_errors_total",
    );
}

#[tokio::test]
async fn test_fail_open_on_error() {
    let (app, _backend_url, _state) = build_compression_test_app(1, false).await;

    let flow_id = "flow-fail-open";
    let messages = vec![
        json!({ "role": "user", "content": "test message" }),
    ];
    let resp = send_chat_request(&app, flow_id, messages).await;

    // Proxy should still return 200 (fail-open).
    assert_eq!(
        resp.status(),
        200,
        "proxy should return 200 even when context is unavailable",
    );
}

#[tokio::test]
async fn test_full_compression_flow() {
    let (app, _backend_url, mock_state) =
        build_compression_test_app(10, true).await;

    let flow_id = "flow-full-compression";

    // Send enough requests to trigger compression.
    for i in 0..12 {
        let messages: Vec<serde_json::Value> = (0..=i)
            .map(|j| {
                json!({
                    "role": if j % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("turn {} message content for testing compression threshold exceeded", j)
                })
            })
            .collect();
        let resp = send_chat_request(&app, flow_id, messages).await;
        assert_eq!(resp.status(), 200);
    }

    // Wait for compression to complete (both on the admin endpoint and the backend).
    let compression_done =
        wait_for_compression(&app, flow_id, Duration::from_secs(5)).await;
    assert!(
        compression_done,
        "compression should complete within timeout",
    );

    // Also verify the compressor request was received by the mock backend.
    let compressor_seen =
        wait_for_compressor_request(&mock_state, Duration::from_secs(2)).await;
    assert!(
        compressor_seen,
        "mock backend should have received a compressor request",
    );

    // Verify the compressor request has the right header recorded.
    {
        let requests = mock_state.requests.lock().unwrap();
        let compressor_requests: Vec<_> =
            requests.iter().filter(|r| r.is_compressor).collect();
        assert!(
            !compressor_requests.is_empty(),
            "should have at least one compressor request",
        );
        for cr in &compressor_requests {
            assert!(
                cr.flow_id_header.is_some(),
                "compressor request should have X-LLM-Flow-ID header",
            );
        }
    }

    // Verify admin endpoint shows compressed segments.
    let request = Request::builder()
        .method("GET")
        .uri(format!("/admin/context/{}", flow_id))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let segments = detail["segments"].as_array().expect("segments is array");
    let has_compressed = segments
        .iter()
        .any(|s| s["kind"] == "compressed");
    assert!(
        has_compressed,
        "should have at least one compressed segment after compression",
    );

    // Verify there's still a head segment.
    let has_head = segments.iter().any(|s| s["kind"] == "head");
    assert!(has_head, "should still have a head segment");
}

#[tokio::test]
async fn test_sidecar_skips_compression() {
    let (app, _backend_url, mock_state) =
        build_compression_test_app(10, true).await;

    let flow_id = "flow-sidecar";

    // Send enough requests to trigger compression.
    for i in 0..12 {
        let messages: Vec<serde_json::Value> = (0..=i)
            .map(|j| {
                json!({
                    "role": if j % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("turn {} content for sidecar test", j)
                })
            })
            .collect();
        let resp = send_chat_request(&app, flow_id, messages).await;
        assert_eq!(resp.status(), 200);
    }

    // Wait for compression to complete.
    let compression_done =
        wait_for_compression(&app, flow_id, Duration::from_secs(5)).await;
    assert!(compression_done, "compression should complete");

    // Check that the compressor requests were properly marked.
    let compressor_seen =
        wait_for_compressor_request(&mock_state, Duration::from_secs(2)).await;
    assert!(compressor_seen, "should have compressor requests");

    {
        let requests = mock_state.requests.lock().unwrap();
        for req in requests.iter() {
            if req.is_compressor {
                // Verify the compressor request has the flow_id header set.
                assert!(
                    req.flow_id_header.is_some(),
                    "compressor request should have X-LLM-Flow-ID",
                );
                // The messages in the compressor request should be the
                // summarization prompt, not a normal conversation.
                let msgs = &req.messages;
                if let Some(arr) = msgs.as_array() {
                    assert!(
                        arr.len() >= 2,
                        "compressor prompt should have system + user messages",
                    );
                    // First message should be a system prompt.
                    assert_eq!(
                        arr[0].get("role").and_then(|r| r.as_str()),
                        Some("system"),
                        "compressor first message should be system",
                    );
                }
            }
        }
    }
}
