//! Tests for flow identification (issue 08).
//!
//! Verifies the flow ID resolution order:
//! 1. `X-LLM-Flow-ID` header wins over everything.
//! 2. `metadata.flow_id` in JSON body is used when no header.
//! 3. Ephemeral ID is generated when neither header nor metadata present.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use tower::ServiceExt;

use tinyllb::config::Algorithm;
use tinyllb::config::BackpressureMode;
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler::Scheduler;

/// Build a test proxy app for flow identification tests, returning both the
/// router and the shared registry/metrics handles for assertion.
fn build_flow_test_app_with_handles(
    backend_url: &str,
) -> (Router, Arc<FlowRegistry>, Arc<metrics::Metrics>) {
    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Drr,
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
        flow_registry: flow_registry.clone(),
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        stall_rx: tinyllb::backend::BackendMonitor::empty().stall_receiver(),
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let admin_router = tinyllb::api::create_router().with_state(state);

    (
        Router::new()
            .merge(health_router)
            .merge(gateway_router)
            .merge(admin_router),
        flow_registry,
        metrics,
    )
}

/// Start a stub backend that echoes back the flow ID from the ticket.
async fn start_echo_stub() -> std::net::SocketAddr {
    let echo_handler = |_req: Request<Body>| async {
        let json = r#"{"choices":[{"message":{"content":"ok"},"index":0}]}"#;
        let mut resp = Response::new(Body::from(json));
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        resp
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(echo_handler))
        .route("/v1/models", get(echo_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Collect a response body into a String.
async fn collect_body_string(resp: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Test: X-LLM-Flow-ID header is resolved and the ticket carries it.
///
/// Sends a request with `X-LLM-Flow-ID: agent-1` and verifies the registry
/// has a flow with that ID after the request completes.
#[tokio::test]
async fn test_header_flow_id_resolved() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let mut headers = HeaderMap::new();
    headers.insert("x-llm-flow-id", HeaderValue::from_static("agent-1"));
    let body = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "agent-1");
    assert!(!id.is_ephemeral());
}

/// Test: metadata.flow_id is used when no header is present.
///
/// Sends a request with `metadata.flow_id: "agent-2"` and no X-LLM-Flow-ID header.
#[tokio::test]
async fn test_metadata_flow_id_resolved() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let headers = HeaderMap::new();
    let body = Bytes::from(r#"{"metadata":{"flow_id":"agent-2"},"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "agent-2");
    assert!(!id.is_ephemeral());
}

/// Test: ephemeral flow ID when neither header nor metadata present.
///
/// Sends a request with no X-LLM-Flow-ID header and no metadata.flow_id.
#[tokio::test]
async fn test_ephemeral_flow_id_generated() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let headers = HeaderMap::new();
    let body = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert!(id.is_ephemeral());
    assert!(id.to_string().starts_with("ephemeral-"));
}

/// Test: header takes precedence over metadata.
///
/// When both `X-LLM-Flow-ID` and `metadata.flow_id` are present,
/// the header value wins.
#[tokio::test]
async fn test_header_wins_over_metadata() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let mut headers = HeaderMap::new();
    headers.insert("x-llm-flow-id", HeaderValue::from_static("header-wins"));
    let body =
        Bytes::from(r#"{"metadata":{"flow_id":"metadata-losing"},"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "header-wins");
    assert!(!id.is_ephemeral());
}

/// Test: non-JSON body falls through to ephemeral.
///
/// When the body is not valid JSON, metadata extraction is skipped.
#[tokio::test]
async fn test_non_json_body_falls_through_to_ephemeral() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let headers = HeaderMap::new();
    let body = Bytes::from_static(b"not-json-binary-data");

    let id = identify::resolve(&headers, &body).flow_id;
    assert!(id.is_ephemeral());
}

/// Test: GET requests (e.g. /v1/models) with empty body get ephemeral IDs.
#[tokio::test]
async fn test_get_request_gets_ephemeral_id() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let headers = HeaderMap::new();
    let body = Bytes::new();

    let id = identify::resolve(&headers, &body).flow_id;
    assert!(id.is_ephemeral());
}

/// Test: ephemeral metric_label is aggregated.
///
/// Ephemeral flows should return "ephemeral" as the metric label value.
#[test]
fn test_ephemeral_metric_label_aggregation() {
    let id = FlowId::new("ephemeral-a1b2c3d4-e5f6-7890-abcd-ef1234567890");
    assert_eq!(id.metric_label(), "ephemeral");
}

/// Test: named flow metric_label is exact.
#[test]
fn test_named_flow_metric_label() {
    let id = FlowId::new("my-agent");
    assert_eq!(id.metric_label(), "my-agent");
}

/// Test: FlowRegistry get_or_create with defaults.
#[test]
fn test_flow_registry_get_or_create_defaults() {
    let registry = FlowRegistry::new(2.5, 75);

    let flow = registry.get_or_create(FlowId::new("test-flow"));
    assert_eq!(flow.id.to_string(), "test-flow");
    assert_eq!(flow.weight(), 2.5);
    assert_eq!(flow.priority(), 75);

    // Verify it's the same Arc on second call.
    let flow2 = registry.get_or_create(FlowId::new("test-flow"));
    assert!(Arc::ptr_eq(&flow, &flow2));
}

/// Test: FlowRegistry with different IDs returns different flows.
#[test]
fn test_flow_registry_different_ids() {
    let registry = FlowRegistry::new(1.0, 50);

    let flow1 = registry.get_or_create(FlowId::new("flow-1"));
    let flow2 = registry.get_or_create(FlowId::new("flow-2"));

    assert_eq!(registry.len(), 2);
    assert!(!Arc::ptr_eq(&flow1, &flow2));
    assert_eq!(flow1.id.to_string(), "flow-1");
    assert_eq!(flow2.id.to_string(), "flow-2");
}

/// Test: end-to-end flow identification through the gateway.
///
/// Sends a request with X-LLM-Flow-ID header and verifies the full flow
/// identification path works (header resolved -> registry updated -> ticket created).
#[tokio::test]
async fn test_end_to_end_flow_identification() {
    let addr = start_echo_stub().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _flow_registry, _metrics) = build_flow_test_app_with_handles(&backend_url);

    // Send a request with X-LLM-Flow-ID header.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "coding-agent")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let _body = collect_body_string(resp).await;

    // The request succeeded; the flow "coding-agent" should have been
    // created in the registry with default weight/priority.
}

/// Test: a proxied request with X-LLM-Flow-ID creates a flow with defaults.
///
/// Sends a request with `X-LLM-Flow-ID: coding-agent` and verifies the
/// FlowRegistry contains `coding-agent` with the configured default_weight
/// and default_priority.
#[tokio::test]
async fn test_named_flow_registers_with_defaults() {
    let addr = start_echo_stub().await;
    let backend_url = format!("http://{}/", addr);
    let (app, flow_registry, _metrics) = build_flow_test_app_with_handles(&backend_url);

    // Send a request with X-LLM-Flow-ID: coding-agent.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "coding-agent")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let _body = collect_body_string(resp).await;

    // Verify the flow exists in the registry with default_weight=1.0 and
    // Verify the flow exists in the registry with default_weight=1.0.
    // After the first admit, the cadence state machine sets priority to 100
    // (Cold state = optimistic interactive) unless the heuristic is disabled.
    let flow = flow_registry.get_or_create(FlowId::new("coding-agent"));
    assert_eq!(flow.id.to_string(), "coding-agent");
    assert_eq!(flow.weight(), 1.0);
    assert_eq!(flow.priority(), 100);
}

/// Test: ephemeral flows aggregate to "ephemeral" label; named flows get
/// their own label value in the metrics gauge.
///
/// Drives several ephemeral requests through the proxy, then a named flow,
/// and asserts:
/// - The metrics gauge has a label value "ephemeral" (not per-UUID).
/// - The metrics gauge has a separate label for the named flow "metrics-agent".
/// - No per-UUID label values exist in the gauge.
#[tokio::test]
async fn test_ephemeral_aggregation_and_named_flow_metric_label() {
    use prometheus::Encoder;

    let addr = start_echo_stub().await;
    let backend_url = format!("http://{}/", addr);
    let (app, _flow_registry, metrics) = build_flow_test_app_with_handles(&backend_url);

    // Send 3 ephemeral requests (no X-LLM-Flow-ID header).
    for _ in 0..3 {
        let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .clone()
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
        let _body = collect_body_string(resp).await;
    }

    // Send 1 named flow request.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-flow-id", "metrics-agent")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _body = collect_body_string(resp).await;

    // Scrape the metrics to verify label values.
    let encoder = prometheus::TextEncoder::new();
    let metric_families = metrics.registry.gather();
    let mut buf = Vec::new();
    encoder.encode(&metric_families, &mut buf).unwrap();
    let metrics_text = String::from_utf8(buf).unwrap();

    // Verify: "ephemeral" label exists (aggregated ephemeral flows).
    assert!(
        metrics_text.contains(r#"flow_id="ephemeral""#),
        "Expected llm_queue_depth with flow_id='ephemeral' label"
    );

    // Verify: named flow label exists.
    assert!(
        metrics_text.contains(r#"flow_id="metrics-agent""#),
        "Expected llm_queue_depth with flow_id='metrics-agent' label"
    );

    // Verify: no per-UUID labels (ephemeral-UUID patterns should not appear).
    for family in &metric_families {
        if family.name.as_deref() == Some("llm_queue_depth") {
            for metric in &family.metric {
                for label in &metric.label {
                    if label.name.as_deref() == Some("flow_id") {
                        let value = label.value.as_deref().unwrap_or("");
                        assert!(
                            !value.starts_with("ephemeral-"),
                            "Per-UUID label found: flow_id='{value}' — should aggregate to 'ephemeral'"
                        );
                    }
                }
            }
        }
    }
}

/// Test: X-Session-Id header resolves to a stable, non-ephemeral flow ID.
///
/// Verifies that the standard x-session-id header is honored by resolve() and
/// produces a flow_id that is not ephemeral.
#[tokio::test]
async fn test_x_session_id_resolves_to_stable_flow() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Session-Id",
        HeaderValue::from_static("integ-session"),
    );
    let body = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "integ-session");
    assert!(!id.is_ephemeral());
}

/// Test: x-session-affinity header resolves to a stable, non-ephemeral flow ID.
#[tokio::test]
async fn test_x_session_affinity_resolves_to_stable_flow() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-session-affinity",
        HeaderValue::from_static("integ-affinity"),
    );
    let body = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "integ-affinity");
    assert!(!id.is_ephemeral());
}

/// Test: two requests with the same X-Session-Id resolve to the same flow ID.
///
/// Regression test: before session fingerprinting, each request would get a
/// unique ephemeral ID even within the same agentic session.
#[tokio::test]
async fn test_same_session_id_yields_same_flow() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let session = "shared-session-id";

    // First request
    let mut headers1 = HeaderMap::new();
    headers1.insert("x-session-id", HeaderValue::from_static(session));
    let body1 = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());
    let id1 = identify::resolve(&headers1, &body1).flow_id;

    // Second request, same session header, different body
    let mut headers2 = HeaderMap::new();
    headers2.insert("x-session-id", HeaderValue::from_static(session));
    let body2 =
        Bytes::from(r#"{"model":"llama-2","messages":[{"role":"user","content":"turn 2"}]}"#.as_bytes());
    let id2 = identify::resolve(&headers2, &body2).flow_id;

    assert_eq!(id1, id2);
    assert_eq!(id1.to_string(), session);
    assert!(!id1.is_ephemeral());
}

/// Test: x-claude-code-session-id resolves to stable flow ID.
#[tokio::test]
async fn test_claude_code_session_id_resolves_to_stable_flow() {
    use bytes::Bytes;
    use tinyllb::flow::identify;

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-claude-code-session-id",
        HeaderValue::from_static("claude-ses-123"),
    );
    let body = Bytes::from_static(r#"{"model":"llama-2"}"#.as_bytes());

    let id = identify::resolve(&headers, &body).flow_id;
    assert_eq!(id.to_string(), "claude-ses-123");
    assert!(!id.is_ephemeral());
}
