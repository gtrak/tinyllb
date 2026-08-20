use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

use tinyllb::config::{BackpressureMode, RetryPolicy};
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn build_proxy_app_with_retry(
    backend_url: &str,
    retry_policy: RetryPolicy,
) -> (Router, Arc<tinyllb::metrics::Metrics>) {
    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = scheduler::Scheduler::new_with_defaults(
        tinyllb::config::Algorithm::Drr,
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    );
    let state = gateway::AppState {
        retry_policy,
        ..gateway::AppState::test_default(
            gateway::build_client(),
            Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
            metrics.clone(),
            Arc::new(scheduler),
            flow_registry,
        )
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

    (app, metrics)
}

async fn collect_body_bytes(resp: Response<Body>) -> Bytes {
    axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap()
}

/// Collect body bytes from a response that may terminate with a stream
/// error (abrupt body termination). Returns the bytes received before the
/// error, if any.
async fn try_collect_body_bytes(resp: Response<Body>) -> Result<Bytes, axum::Error> {
    axum::body::to_bytes(resp.into_body(), 1024 * 1024).await
}

// ---------------------------------------------------------------------------
// Helper: response body builders (return owned Vec<u8>)
// ---------------------------------------------------------------------------

fn premature_response_bytes() -> Vec<u8> {
    r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant"}}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#
        .as_bytes()
        .to_vec()
}

fn good_response_bytes(content: &str) -> Vec<u8> {
    serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": content
            }
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })
    .to_string()
    .into_bytes()
}

fn length_response_bytes() -> Vec<u8> {
    r#"{"choices":[{"finish_reason":"length","message":{"role":"assistant"}}],"usage":{"prompt_tokens":5,"completion_tokens":0,"total_tokens":5}}"#
        .as_bytes()
        .to_vec()
}

fn tool_calls_response_bytes() -> Vec<u8> {
    serde_json::json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "search"},
                    "type": "function"
                }]
            }
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    })
    .to_string()
    .into_bytes()
}

fn completions_stop_response_bytes() -> Vec<u8> {
    r#"{"choices":[{"finish_reason":"stop","text":""}],"usage":{"prompt_tokens":5,"completion_tokens":0,"total_tokens":5}}"#
        .as_bytes()
        .to_vec()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test: premature stop triggers retry, good response returned on call 2.
#[tokio::test]
async fn premature_stop_triggers_retry_returns_good_body() {
    let call_count = Arc::new(AtomicU32::new(0));
    let premature_b = premature_response_bytes();
    let good_b = good_response_bytes("hello");

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        let good_b = good_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 { premature_b } else { good_b };
            let mut resp = Response::new(Body::from(body));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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

    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        response_json["choices"][0]["message"]["content"],
        "hello"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "backend should be called twice (initial + 1 retry)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: non-premature response (content present) does NOT trigger retry.
#[tokio::test]
async fn non_premature_response_no_retry() {
    let call_count = Arc::new(AtomicU32::new(0));
    let good_b = good_response_bytes("hello world");

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let good_b = good_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(good_b));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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

    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["message"]["content"], "hello world");

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once (no retry)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: finish_reason "length" with empty content does NOT trigger retry.
#[tokio::test]
async fn finish_reason_length_no_retry() {
    let call_count = Arc::new(AtomicU32::new(0));
    let length_b = length_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let length_b = length_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(length_b));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, _) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["finish_reason"], "length");
    // finish_reason "length" with no content/tool_calls is degenerate
    // (token-capped mid-thinking): retried up to max_retries (2), then
    // fail-open forwards the last response.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "length finish should trigger retries (initial + 2), then fail-open"
    );
}

/// Test: finish_reason "tool_calls" with tool_calls present does NOT trigger retry.
#[tokio::test]
async fn finish_reason_tool_calls_no_retry() {
    let call_count = Arc::new(AtomicU32::new(0));
    let tool_b = tool_calls_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let tool_b = tool_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(tool_b));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, _) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

/// Test: retries exhausted -> last degenerate forwarded, metrics correct.
#[tokio::test]
async fn retries_exhausted_forwards_last_degenerate() {
    let call_count = Arc::new(AtomicU32::new(0));
    let premature_b = premature_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(premature_b));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["finish_reason"], "stop");
    assert!(response_json["choices"][0]["message"].get("content").is_none());

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "backend should be called 3 times (initial + 2 retries)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 2.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 2.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 1.0);
}

/// Test: retry policy disabled -> zero behavioral change, passthrough.
#[tokio::test]
async fn disabled_no_retry_passthrough() {
    let call_count = Arc::new(AtomicU32::new(0));
    let premature_b = premature_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(premature_b));
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
    let retry_policy = RetryPolicy {
        enabled: false,
        ..Default::default()
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["finish_reason"], "stop");
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "backend should be called once");
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: internal compressor requests are skipped even when retry is enabled.
#[tokio::test]
async fn internal_compressor_skipped() {
    let call_count = Arc::new(AtomicU32::new(0));
    let premature_b = premature_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(premature_b));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-llm-internal", "compressor")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(response_json["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once (internal compressor skipped)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: non-chat-completions routes are not retried.
#[tokio::test]
async fn only_chat_completions_retries() {
    let call_count = Arc::new(AtomicU32::new(0));
    let stop_b = completions_stop_response_bytes();

    let cc = call_count.clone();
    let completions_handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let stop_b = stop_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(stop_b));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp
        }
    };

    let chat_handler = move |_req: Request<Body>| async {
        let json = r#"{"choices":[{}]}"#;
        let mut resp = Response::new(Body::from(json.as_bytes()));
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        resp
    };

    let backend_app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/completions", post(completions_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","prompt":"hi"}"#;
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
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once (completions route not retried)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: retry HTTP failure (500 on retry) -> fail-open with initial body.
#[tokio::test]
async fn retry_http_failure_fail_open() {
    let call_count = Arc::new(AtomicU32::new(0));
    let premature_b = premature_response_bytes();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let mut resp = Response::new(Body::from(premature_b));
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                resp
            } else {
                let mut resp =
                    Response::new(Body::from("internal server error".to_string()));
                *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/plain"),
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        response_json["choices"][0]["finish_reason"],
        "stop",
        "client should receive the initial premature body (fail-open)"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "backend should be called twice (initial + 1 failed retry)"
    );
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 1.0);
    assert_eq!(
        state_metrics.premature_stop_exhausted_total.get(),
        0.0,
        "exhausted should be 0 (retry failed, not all retries exhausted)"
    );
}

/// Test: retry body arrives intact (not truncated by stale Content-Length).
/// Regression for: bump_temperature changes body length, but stale Content-Length
/// header on retry send truncates the body to the original length.
#[tokio::test]
async fn retry_body_not_truncated_by_stale_content_length() {
    let call_count = Arc::new(AtomicU32::new(0));
    // Shared cell to capture the request body on the retry (call #2).
    let captured_retry_body: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let good_b = good_response_bytes("retry-success");
    let premature_b = premature_response_bytes();

    let cc = call_count.clone();
    let captured = captured_retry_body.clone();
    let handler = move |req: Request<Body>| {
        let cc = cc.clone();
        let captured = captured.clone();
        let good_b = good_b.clone();
        let premature_b = premature_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            // Capture the request body on call #2 (the retry).
            if n == 1 {
                let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let mut captured = captured.lock().unwrap();
                *captured = Some(body_bytes.to_vec());
            } else {
                // Drain body on other calls to avoid leaked resources.
                let _ = axum::body::to_bytes(req.into_body(), 1024 * 1024).await;
            }
            let body = if n == 0 { premature_b } else { good_b };
            let mut resp = Response::new(Body::from(body));
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, _) = build_proxy_app_with_retry(&backend_url, retry_policy);

    // Send a non-streaming request with NO temperature field — bump_temperature
    // will add one, changing the body length (this is the truncation trigger).
    // Explicitly set Content-Length to the ORIGINAL body length so the retry
    // send (with a longer body) would be truncated by the stale header if the
    // proxy did not strip it. Without this header, `oneshot` never adds one
    // (the transport normally does, over the wire) and the bug cannot reproduce.
    let body = r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("content-length", body.len().to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body_bytes = collect_body_bytes(resp).await;
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        response_json["choices"][0]["message"]["content"],
        "retry-success",
        "client should get the good response from the retry"
    );

    // Assert the captured retry body is the full bumped JSON.
    let captured = captured_retry_body.lock().unwrap();
    let captured_body = captured.as_ref().expect("retry body should have been captured");

    // Parse as JSON — a truncated body would fail this.
    let retry_json: serde_json::Value =
        serde_json::from_slice(captured_body).expect("retry body should be valid JSON");

    // Verify temperature was bumped to default_temperature + 1*step = 0.3.
    let temp = retry_json
        .get("temperature")
        .and_then(|v| v.as_f64())
        .expect("retry body should have temperature field");
    assert_eq!(temp, 0.3, "retry body temperature should be 0.3");

    // Verify messages are preserved.
    let messages = retry_json.get("messages").expect("retry body should have messages");
    assert!(messages.is_array(), "messages should be an array");
    assert_eq!(
        messages.as_array().unwrap().len(),
        1,
        "messages array should be preserved"
    );
    assert_eq!(
        messages[0]["role"],
        "user",
        "first message role should be preserved"
    );
    assert_eq!(
        messages[0]["content"],
        "hi",
        "first message content should be preserved"
    );
}

// ---------------------------------------------------------------------------
// Helper: parse SSE data payloads from client response body
// ---------------------------------------------------------------------------

/// Parse a client SSE response body into a Vec of `data:` payloads.
/// Each SSE event is delimited by `\n\n`.  For each event, find lines
/// starting with `data:` and collect the payload (everything after `data:`).
fn parse_sse_data_payloads(body_bytes: &Bytes) -> Vec<String> {
    let body_str = String::from_utf8_lossy(body_bytes);
    let mut payloads = Vec::new();
    // SSE events are delimited by double newlines.
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
// Streaming tests
// ---------------------------------------------------------------------------

/// Test: premature stop triggers retry, concatenated stream contains reasoning + content.
#[tokio::test]
async fn streaming_premature_triggers_retry_concatenated() {
    let call_count = Arc::new(AtomicU32::new(0));

    // Call 1: reasoning delta (no content) + premature terminal (no content/tool_calls).
    // These frames get forwarded live (reasoning delta) and the terminal triggers retry.
    // [DONE] and usage from call 1 are NOT forwarded (accepted == false).
    let premature_b: String = [
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
    ]
    .concat();

    // Call 2: content delta + terminal with content + usage + [DONE].
    // All frames forwarded (accepted == true after terminal).
    let good_b: String = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{\"content\":\"\"}}]}\n\n",
        "data: {\"usage\":{\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        let good_b = good_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 { premature_b } else { good_b };
            let mut resp = Response::new(Body::from(body));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let payloads = parse_sse_data_payloads(&body_bytes);

    // Client should see reasoning from call 1 AND content from call 2.
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains("thinking..."),
        "client should receive reasoning delta from call 1"
    );
    assert!(
        body_str.contains("final answer"),
        "client should receive content delta from call 2"
    );

    // Exactly ONE finish_reason frame and ONE [DONE].
    let finish_reason_count = payloads
        .iter()
        .filter(|p| p.contains("finish_reason"))
        .count();
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(
        finish_reason_count, 1,
        "exactly one finish_reason frame in concatenated stream"
    );
    assert_eq!(done_count, 1, "exactly one [DONE] in concatenated stream");

    // Metrics: one premature detection, one retry, no exhaustion.
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
    // Only the accepted attempt's usage is counted.
    assert_eq!(
        state_metrics.tokens_generated_total.get(),
        7.0,
        "tokens from accepted attempt only"
    );
}

/// Test: normal streaming response (content present) does NOT trigger retry.
#[tokio::test]
async fn streaming_non_premature_no_retry() {
    let call_count = Arc::new(AtomicU32::new(0));

    let good_b: String = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"usage\":{\"completion_tokens\":5}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let good_b = good_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(good_b));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let payloads = parse_sse_data_payloads(&body_bytes);

    // Backend called ONCE (no retry).
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once"
    );

    // Client got the content.
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("hello"), "client should receive content");

    // Exactly one finish_reason and one [DONE].
    let finish_reason_count = payloads
        .iter()
        .filter(|p| p.contains("finish_reason"))
        .count();
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(finish_reason_count, 1);
    assert_eq!(done_count, 1);

    // No premature-stop activity.
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
    assert_eq!(state_metrics.tokens_generated_total.get(), 5.0);
}

/// Test: tool_calls finish_reason does NOT trigger retry.
#[tokio::test]
async fn streaming_finish_reason_tool_calls_no_retry() {
    let call_count = Arc::new(AtomicU32::new(0));

    let tool_b: String = [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"function\":{\"name\":\"search\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"usage\":{\"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let tool_b = tool_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(tool_b));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let payloads = parse_sse_data_payloads(&body_bytes);

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once (tool_calls is not premature)"
    );

    // Exactly one finish_reason == "tool_calls" and one [DONE].
    let finish_reason_count = payloads
        .iter()
        .filter(|p| p.contains("finish_reason"))
        .count();
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(finish_reason_count, 1);
    assert_eq!(done_count, 1);
    assert!(
        payloads
            .iter()
            .any(|p| p.contains("\"tool_calls\"")),
        "should contain tool_calls finish_reason"
    );

    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
}

/// Test: all three attempts return premature; last attempt's terminal + usage + [DONE]
/// is forwarded (fail-open by accepting). When retries run out and the last terminal
/// is still degenerate (premature-shaped: stop + no content + no tool_calls),
/// `premature_stop_exhausted_total` is incremented to match the non-streaming path.
///
/// Tracing the retry loop (max_retries=2):
/// - attempt 0: premature (0 < 2) -> detected=1, retries=1, attempt=1, swap
/// - attempt 1: premature (1 < 2) -> detected=2, retries=2, attempt=2, swap
/// - attempt 2: accepted (2 >= 2), degenerate -> exhausted=1. Forward terminal+usage+[DONE].
/// Expected: detected=2, retries=2, exhausted=1.
#[tokio::test]
async fn streaming_exhausted_forwards_last() {
    let call_count = Arc::new(AtomicU32::new(0));

    // Calls 1 and 2: reasoning + premature terminal (no content).
    let premature_b: String = [
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
    ]
    .concat();

    // Call 3 (last): same + usage + [DONE]. The terminal is ACCEPTED
    // (attempt=2 >= max_retries=2), so usage and [DONE] are forwarded.
    let last_b: String = [
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"usage\":{\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        let last_b = last_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            let body = if n == 2 { last_b } else { premature_b };
            let mut resp = Response::new(Body::from(body));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let payloads = parse_sse_data_payloads(&body_bytes);

    let body_str = String::from_utf8_lossy(&body_bytes);

    // Backend called 3 times (initial + 2 retries).
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "backend should be called 3 times (initial + 2 retries)"
    );

    // Client receives exactly 1 finish_reason (from the last attempt) and 1 [DONE].
    let finish_reason_count = payloads
        .iter()
        .filter(|p| p.contains("finish_reason"))
        .count();
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(
        finish_reason_count, 1,
        "exactly one finish_reason from the last (accepted) attempt"
    );
    assert_eq!(done_count, 1, "exactly one [DONE] from the last attempt");

    // Reasoning from all attempts may appear (forwarded live), but no content.
    assert!(
        body_str.contains("thinking..."),
        "reasoning deltas are forwarded live"
    );

    // Metrics: 2 premature detections, 2 retries, 1 exhausted
    // (degenerate fail-open on last attempt after retries exhausted).
    assert_eq!(
        state_metrics.premature_stop_detected_total.get(),
        2.0,
        "two premature detections (calls 1 and 2)"
    );
    assert_eq!(
        state_metrics.premature_stop_retries_total.get(),
        2.0,
        "two retries attempted"
    );
    assert_eq!(
        state_metrics.premature_stop_exhausted_total.get(),
        1.0,
        "degenerate fail-open on last attempt after retries exhausted (matches non-streaming)"
    );
}

/// Test: retry disabled -> passthrough. Premature terminal + [DONE] forwarded AS-IS.
#[tokio::test]
async fn streaming_disabled_passthrough() {
    let call_count = Arc::new(AtomicU32::new(0));

    // Premature response: reasoning + stop (no content) + usage + [DONE].
    // With retry disabled, ALL frames are forwarded as-is.
    let premature_b: String = [
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"usage\":{\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let _ = cc.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(premature_b));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            resp
        }
    };

    let backend_app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, backend_app).await.unwrap(); });

    let backend_url = format!("http://{}/", addr);
    let retry_policy = RetryPolicy {
        enabled: false,
        ..Default::default()
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    let body_bytes = collect_body_bytes(resp).await;
    let payloads = parse_sse_data_payloads(&body_bytes);

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "backend should be called once (retry disabled)"
    );

    // Client receives the premature terminal + [DONE] as-is.
    let finish_reason_count = payloads
        .iter()
        .filter(|p| p.contains("finish_reason"))
        .count();
    let done_count = payloads.iter().filter(|p| *p == "[DONE]").count();
    assert_eq!(finish_reason_count, 1);
    assert_eq!(done_count, 1);

    // No premature-stop activity (retry disabled).
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 0.0);
    assert_eq!(state_metrics.premature_stop_exhausted_total.get(), 0.0);
    // MetricStream path (retry disabled) counts tokens via TokenAccumulator.
    assert_eq!(
        state_metrics.tokens_generated_total.get(),
        7.0,
        "MetricStream counts tokens in disabled-passthrough path"
    );
}

/// Test: retry HTTP failure -> fail-open. The stream ends with whatever was
/// forwarded before the retry (reasoning delta), no terminal/[DONE].
///
/// Implementation behavior verified in src/gateway/stream.rs:
/// The retry-failure arm reads:
///   ```
///   _ => {
///       metrics.premature_stop_exhausted_total.inc();
///       return; // fail-open
///   }
///   ```
/// This means the spawned task returns immediately on retry failure.
/// The client receives the forwarded frames (reasoning from call 1) but
/// NO terminal frame, NO [DONE], and NO usage — the stream just ends.
#[tokio::test]
async fn streaming_retry_http_failure_fail_open() {
    let call_count = Arc::new(AtomicU32::new(0));

    // Call 1: reasoning + premature terminal.
    let premature_b: String = [
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
    ]
    .concat();

    let cc = call_count.clone();
    let handler = move |_req: Request<Body>| {
        let cc = cc.clone();
        let premature_b = premature_b.clone();
        async move {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let mut resp = Response::new(Body::from(premature_b));
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/event-stream"),
                );
                resp
            } else {
                // Retry: return HTTP 500 to simulate network failure.
                let mut resp =
                    Response::new(Body::from("internal server error".to_string()));
                *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/plain"),
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
    let retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        temperature_step: 0.3,
        max_temperature: 1.5,
        default_temperature: 0.0,
    };

    let (app, state_metrics) = build_proxy_app_with_retry(&backend_url, retry_policy);

    let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
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
    // The body terminates with an error (abrupt termination) rather than a
    // clean close: a stream that ends without an accepted terminal frame
    // must surface as a transport failure so clients auto-retry.
    // (Note: frames forwarded before the error do reach a streaming client,
    // but to_bytes discards partial data on error, so they can't be
    // asserted here.)
    let body_result = try_collect_body_bytes(resp).await;
    assert!(
        body_result.is_err(),
        "body should terminate with an error (no terminal frame accepted)"
    );

    // Backend called twice (initial + failed retry).
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "backend should be called twice (initial + 1 failed retry)"
    );

    // Retry failure increments exhausted.
    assert_eq!(state_metrics.premature_stop_detected_total.get(), 1.0);
    assert_eq!(state_metrics.premature_stop_retries_total.get(), 1.0);
    assert_eq!(
        state_metrics.premature_stop_exhausted_total.get(),
        1.0,
        "retry HTTP failure increments exhausted"
    );
}
