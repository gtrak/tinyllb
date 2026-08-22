//! Integration tests for `id_slot` session pinning (plan 009, task 02):
//! a named (non-ephemeral) inference request carries a deterministic
//! `id_slot` integer in the forwarded body when `llamacpp_slots` is set;
//! ephemeral requests, non-inference routes, and the disabled (`None`)
//! config never get it. The stub backend records every forwarded request
//! body so tests assert on the actual bytes the proxy sent.

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

use tinyllb::config::{BackpressureMode, TransientRetry};
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler;

const CHAT_BODY: &str =
    r#"{"model":"llama-2","messages":[{"role":"user","content":"hi"}]}"#;

// ---------------------------------------------------------------------------
// Harness: stub backend that RECORDS every forwarded request body
// ---------------------------------------------------------------------------

/// Stub backend state: records the raw body of each received request and
/// optionally returns one transient llama.cpp context-exceed error before
/// succeeding (for the retry-survival test).
struct StubState {
    /// Raw request bodies in arrival order.
    bodies: Mutex<Vec<Bytes>>,
    /// When > 0, the next response is a transient `exceed_context_size_error`
    /// (400); decrements on use.
    transient_errors_remaining: AtomicU32,
}

/// Record the request body, then return a transient error or a normal
/// chat-completion response.
async fn record_and_respond(
    state: axum::extract::State<Arc<StubState>>,
    req: Request<Body>,
) -> Response<Body> {
    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    state.bodies.lock().unwrap().push(body_bytes);

    // Atomically claim one transient error if any remain (no wrap-around
    // when the counter is 0).
    let emit_error = {
        let mut n = state.transient_errors_remaining.load(Ordering::SeqCst);
        loop {
            if n == 0 {
                break false;
            }
            match state
                .transient_errors_remaining
                .compare_exchange_weak(n, n - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break true,
                Err(x) => n = x,
            }
        }
    };
    if emit_error {
        // llama.cpp transient context-exceed (n_prompt_tokens < n_ctx).
        let error_b = serde_json::json!({
            "error": {
                "code": 400,
                "type": "exceed_context_size_error",
                "message": "prompt is too long for the context",
                "n_prompt_tokens": 100,
                "n_ctx": 262_144
            }
        })
        .to_string();
        let mut resp = Response::new(Body::from(error_b));
        *resp.status_mut() = axum::http::StatusCode::BAD_REQUEST;
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return resp;
    }

    let json = r#"{"choices":[{"message":{"content":"ok"},"index":0}]}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Start the recording stub backend on an ephemeral port.
async fn start_stub_backend() -> (std::net::SocketAddr, Arc<StubState>) {
    let state = Arc::new(StubState {
        bodies: Mutex::new(Vec::new()),
        transient_errors_remaining: AtomicU32::new(0),
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(record_and_respond))
        .route("/v1/completions", post(record_and_respond))
        .route("/v1/models", get(record_and_respond))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, state)
}

/// Build the proxy app pointing at the given backend URL, with the given
/// `llamacpp_slots` config. Transient retry uses the default (disabled-fast)
/// policy; the retry test overrides it separately.
fn build_proxy_app(backend_url: &str, llamacpp_slots: Option<u32>) -> Router {
    build_proxy_app_with_retry(
        backend_url,
        llamacpp_slots,
        TransientRetry::default(),
    )
}

/// Build the proxy app with an explicit transient-retry policy.
fn build_proxy_app_with_retry(
    backend_url: &str,
    llamacpp_slots: Option<u32>,
    transient_retry: TransientRetry,
) -> Router {
    let metrics = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = scheduler::Scheduler::new_with_defaults(
        4,
        metrics.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let state = gateway::AppState {
        llamacpp_slots,
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
    Router::new().merge(gateway_router).with_state(state)
}

/// Collect a response body into bytes.
async fn collect_body_bytes(resp: Response<Body>) -> Bytes {
    axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap()
}

/// Parse the recorded request bodies (all must be valid JSON, except empty
/// bodies which are returned as `Null`).
fn recorded_json_bodies(stub: &Arc<StubState>) -> Vec<serde_json::Value> {
    let bodies = stub.bodies.lock().unwrap();
    bodies
        .iter()
        .map(|b| {
            if b.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(b).expect("recorded body must be valid JSON")
            }
        })
        .collect()
}

/// POST a chat-completions request to the proxy.
async fn post_chat(app: &Router, body: &str, session: Option<&str>) -> Response<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(s) = session {
        builder = builder.header("x-session-id", s);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    app.clone().oneshot(req).await.unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1. Named session + enabled config → forwarded body carries `id_slot`
/// equal to `slot_id_for_flow("ses_a", 4)`.
#[tokio::test]
async fn named_session_injects_id_slot() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, Some(4));

    let resp = post_chat(&app, CHAT_BODY, Some("ses_a")).await;
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(bodies.len(), 1, "exactly one backend call expected");
    let v = &bodies[0];
    let expected = tinyllb::flow::slot_id_for_flow("ses_a", 4);
    assert_eq!(
        v["id_slot"],
        serde_json::json!(expected),
        "forwarded body must carry the deterministic slot for the session"
    );
    // Original fields must survive injection.
    assert_eq!(v["model"], "llama-2");
    assert_eq!(v["messages"][0]["role"], "user");
}

/// 2. Two requests with the same session → identical `id_slot` in both
/// forwarded bodies (session stickiness across requests).
#[tokio::test]
async fn same_session_same_slot() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, Some(4));

    let resp1 = post_chat(&app, CHAT_BODY, Some("ses_dup")).await;
    assert_eq!(resp1.status(), 200);
    let _ = collect_body_bytes(resp1).await;
    let resp2 = post_chat(&app, CHAT_BODY, Some("ses_dup")).await;
    assert_eq!(resp2.status(), 200);
    let _ = collect_body_bytes(resp2).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(bodies.len(), 2);
    let slot1 = bodies[0].get("id_slot").cloned();
    let slot2 = bodies[1].get("id_slot").cloned();
    assert!(slot1.is_some(), "first request must carry id_slot");
    assert!(slot2.is_some(), "second request must carry id_slot");
    assert_eq!(
        slot1, slot2,
        "the same session must map to the same slot on every request"
    );
}

/// 3. No session header → ephemeral flow → no `id_slot` even with the
/// config enabled.
#[tokio::test]
async fn ephemeral_omits_id_slot() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, Some(4));

    let resp = post_chat(&app, CHAT_BODY, None).await;
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].get("id_slot").is_none(),
        "ephemeral (unnamed) flows must never carry id_slot"
    );
}

/// 4. Disabled config (`llamacpp_slots: None`) → no `id_slot` for a named
/// session, and the forwarded body is byte-identical to the client's body
/// (regression gate: nothing is re-serialized).
#[tokio::test]
async fn disabled_omits_id_slot() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, None);

    let resp = post_chat(&app, CHAT_BODY, Some("ses_a")).await;
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].get("id_slot").is_none(),
        "id_slot must be absent when pinning is disabled"
    );
    // Byte-identity: with pinning disabled and no streaming, the proxy must
    // forward the client's body untouched.
    let raw = stub.bodies.lock().unwrap();
    assert_eq!(
        raw[0].as_ref(),
        CHAT_BODY.as_bytes(),
        "forwarded body must be byte-identical when nothing is injected"
    );
}

/// 5. `id_slot` is a JSON integer (number), never a string.
#[tokio::test]
async fn id_slot_is_integer() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, Some(8));

    let resp = post_chat(&app, CHAT_BODY, Some("ses_num")).await;
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(bodies.len(), 1);
    let v = &bodies[0]["id_slot"];
    assert!(v.is_number(), "id_slot must be a JSON number, got {v}");
    assert!(v.is_u64(), "id_slot must be a JSON unsigned integer, got {v}");
    assert!(!v.is_string(), "id_slot must not be a string");
}

/// 6. `GET /v1/models` is not an inference request → never pinned, even
/// with a session header and the config enabled. The stub records the
/// (empty) forwarded body: nothing is injected.
#[tokio::test]
async fn models_route_never_pinned() {
    let (addr, stub) = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url, Some(4));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("x-session-id", "ses_a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let raw = stub.bodies.lock().unwrap();
    assert_eq!(raw.len(), 1, "exactly one backend call expected");
    let v: serde_json::Value =
        serde_json::from_slice(&raw[0]).unwrap_or(serde_json::Value::Null);
    assert!(
        v.get("id_slot").is_none(),
        "non-inference routes must never carry id_slot"
    );
}

/// 7. Transient-retry survival: the stub 400s once (transient
/// `exceed_context_size_error`) then succeeds; the retried request must
/// carry the same `id_slot` as the first attempt. Guaranteed structurally by
/// baking `id_slot` into `forwarded_body` (the canonical retried body).
#[tokio::test]
async fn id_slot_survives_retry() {
    let (addr, stub) = start_stub_backend().await;
    // Arm one transient error before success.
    stub.transient_errors_remaining
        .store(1, Ordering::SeqCst);
    let backend_url = format!("http://{}/", addr);
    let fast_policy = TransientRetry {
        max_attempts: 3,
        backoff_start: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
    };
    let app = build_proxy_app_with_retry(&backend_url, Some(4), fast_policy);

    let resp = post_chat(&app, CHAT_BODY, Some("ses_retry")).await;
    assert_eq!(resp.status(), 200);
    let _ = collect_body_bytes(resp).await;

    let bodies = recorded_json_bodies(&stub);
    assert_eq!(
        bodies.len(),
        2,
        "backend should be called twice (initial + 1 re-forward)"
    );
    let expected = tinyllb::flow::slot_id_for_flow("ses_retry", 4);
    let slot1 = bodies[0].get("id_slot").cloned();
    let slot2 = bodies[1].get("id_slot").cloned();
    assert!(
        slot1 == Some(serde_json::json!(expected)),
        "initial attempt must carry id_slot, got {slot1:?}"
    );
    assert!(
        slot2 == Some(serde_json::json!(expected)),
        "retried attempt must carry the same id_slot, got {slot2:?}"
    );
}
