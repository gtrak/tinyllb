use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures::Stream;
use std::net::SocketAddr;
use std::pin::Pin;
use tower::ServiceExt;

/// Start a stub backend server on an ephemeral port and return its address.
async fn start_stub_backend() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/v1/completions", post(completions_handler))
        .route("/v1/models", get(models_handler))
        .route("/echo", post(echo_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

async fn chat_completions_handler(req: Request<Body>) -> Response<Body> {
    // Collect body to check for stream flag and error trigger.
    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();

    // If body contains "trigger_error", return a 500 to test error forwarding.
    if serde_json::from_slice::<serde_json::Value>(&body_bytes).is_ok() {
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        if json.get("trigger_error").and_then(|v| v.as_bool()) == Some(true) {
            let body = r#"{"error":{"message":"simulated backend error","code":500}}"#;
            let mut resp = Response::new(Body::from(body));
            *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
            resp.headers_mut().insert(
                axum::http::HeaderName::from_static("content-type"),
                axum::http::HeaderValue::from_static("application/json"),
            );
            resp.headers_mut().insert(
                axum::http::HeaderName::from_static("x-error-code"),
                axum::http::HeaderValue::from_static("kv-full"),
            );
            return resp;
        }
        if json.get("stream").and_then(|v| v.as_bool()) == Some(true) {
            // Return streaming SSE response.
            let chunks: Vec<Bytes> = vec![
                Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
                Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n"),
                Bytes::from("data: [DONE]\n\n"),
            ];
            let stream = SseStream::new(chunks);
            let body = Body::from_stream(stream);
            let mut resp = Response::new(body);
            resp.headers_mut().insert(
                axum::http::HeaderName::from_static("content-type"),
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            return resp;
        }

        // Valid JSON, no special triggers — return standard response.
        let json = r#"{"choices":[{"message":{"content":"hello world"},"index":0}}]"#;
        let mut resp = Response::new(Body::from(json));
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static("content-type"),
            axum::http::HeaderValue::from_static("application/json"),
        );
        return resp;
    }

    // Body is not valid JSON — echo raw bytes back for byte-preservation tests.
    let mut resp = Response::new(Body::from(body_bytes.to_vec()));
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-type"),
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    resp
}

async fn completions_handler(req: Request<Body>) -> Response<Body> {
    let _ = req;
    let json = r#"{"choices":[{"text":"hello world","index":0}}]"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-type"),
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

async fn models_handler(req: Request<Body>) -> Response<Body> {
    let json = r#"{"data":[{"id":"llama-2-7b","object":"model"}]}"#;
    let mut resp = Response::new(Body::from(json));
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-type"),
        axum::http::HeaderValue::from_static("application/json"),
    );
    // Echo back the query string so the test can verify it was forwarded.
    if let Some(query) = req.uri().query() {
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static("x-forwarded-query"),
            axum::http::HeaderValue::from_str(query).expect("valid query header value"),
        );
    }
    resp
}

/// Echo handler: returns the raw request body bytes verbatim.
async fn echo_handler(req: Request<Body>) -> Response<Body> {
    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let mut resp = Response::new(Body::from(body_bytes.to_vec()));
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-type"),
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    resp
}

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

/// Build the proxy app pointing at the given backend URL.
fn build_proxy_app(backend_url: &str) -> Router {
    use llm_qdisc_proxy::gateway;

    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: std::sync::Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());

    Router::new()
        .merge(health_router)
        .merge(gateway_router)
        .with_state(state)
}

/// Collect a response body into bytes.
async fn collect_body_bytes(resp: Response<Body>) -> Bytes {
    axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap()
}

/// Collect a streaming response body into individual chunks (Bytes items).
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

#[tokio::test]
async fn test_nonstream_chat_completions() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

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

    // Assert content-type header (must check before consuming body).
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type should be present")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "application/json");
    // Assert byte-identical body (not just contains).
    let body_bytes = collect_body_bytes(resp).await;
    let expected: Vec<u8> =
        Vec::from(br#"{"choices":[{"message":{"content":"hello world"},"index":0}}]"#);
    assert_eq!(
        body_bytes.as_ref(),
        &expected,
        "response body must be byte-identical to stub output"
    );
}

#[tokio::test]
async fn test_streaming_chat_completions() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

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

    // Assert content-type is text/event-stream.
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type should be present")
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("text/event-stream"));

    // Collect the streaming body as individual chunks and assert byte identity.
    let chunks = collect_chunks(resp).await;
    let full: Vec<u8> = chunks
        .iter()
        .flat_map(|c| c.as_ref().iter().copied())
        .collect();

    let mut expected_full: Vec<u8> = Vec::new();
    expected_full
        .extend_from_slice(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
    expected_full
        .extend_from_slice(b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n");
    expected_full.extend_from_slice(b"data: [DONE]\n\n");

    assert_eq!(
        full, expected_full,
        "streaming response must be byte-identical to stub SSE frames"
    );

    // The stub sends 3 SSE frames. Assert we observed at least 2 distinct
    // non-empty chunks — a fully-buffered response would collapse to 1 chunk.
    // (HTTP clients may coalesce some frames, so >= 2 is the strongest
    // invariant that holds reliably across hyper's internal buffering.)
    assert!(
        chunks.len() >= 2,
        "streaming response should yield >= 2 distinct chunks (got {}), \
         proving the stream is not fully buffered",
        chunks.len()
    );
}

#[tokio::test]
async fn test_backend_500_forwarded() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

    // Send a request that triggers the stub to return 500.
    let body = r#"{"model":"llama-2","trigger_error":true}"#;
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

    assert_eq!(resp.status(), 500);

    // Assert custom header is preserved (must check before consuming body).
    let x_error_code = resp
        .headers()
        .get(axum::http::HeaderName::from_static("x-error-code"))
        .expect("x-error-code header should be forwarded");
    assert_eq!(x_error_code.to_str().unwrap(), "kv-full");

    // Assert byte-identical error body.
    let body_bytes = collect_body_bytes(resp).await;
    let expected = r#"{"error":{"message":"simulated backend error","code":500}}"#.as_bytes();
    assert_eq!(
        body_bytes.as_ref(),
        expected,
        "error body must be byte-identical to stub output"
    );
}

#[tokio::test]
async fn test_backend_unreachable_returns_502() {
    // Use a URL that points to nowhere.
    let app = build_proxy_app("http://127.0.0.1:59999/");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 502);
}

#[tokio::test]
async fn test_models_get_forwarded() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body_bytes = collect_body_bytes(resp).await;
    let expected = r#"{"data":[{"id":"llama-2-7b","object":"model"}]}"#.as_bytes();
    assert_eq!(
        body_bytes.as_ref(),
        expected,
        "models response body must be byte-identical to stub output"
    );
}

/// Test that non-UTF-8 request bodies are forwarded byte-preserving.
/// The stub's chat_completions handler echoes the raw body when it's not
/// valid JSON. We send binary data and assert the response is byte-identical.
#[tokio::test]
async fn test_request_body_byte_preservation() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

    // Non-UTF-8 bytes.
    let binary_body: Vec<u8> = vec![0xff, 0xfe, 0x01, 0x02, 0x03];
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/octet-stream")
                .body(Body::from(binary_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // The stub echoes the raw bytes back (not valid JSON → echo mode).
    // Assert byte-for-byte identity to prove the proxy preserved the binary data.
    let body_bytes = collect_body_bytes(resp).await;
    assert_eq!(
        body_bytes.as_ref(),
        binary_body.as_slice(),
        "response body must be byte-identical to the sent binary body"
    );
}

/// Test that the body size guard rejects oversized requests with 413.
#[tokio::test]
async fn test_body_size_guard_rejects_large_content_length() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

    // Send a request with Content-Length exceeding 32 MiB.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("content-length", "34359738368") // 32 MiB + 1
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 413);
}

/// Test that query strings are preserved when forwarding.
/// The stub echoes the query string back in an `x-forwarded-query` header;
/// we assert the proxy forwarded `api_key=test123` exactly.
#[tokio::test]
async fn test_query_string_preserved() {
    let addr = start_stub_backend().await;
    let backend_url = format!("http://{}/", addr);
    let app = build_proxy_app(&backend_url);

    // Send a request with a query string.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models?api_key=test123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Assert the query string was forwarded exactly.
    let forwarded_query = resp
        .headers()
        .get(axum::http::HeaderName::from_static("x-forwarded-query"))
        .expect("x-forwarded-query header should be present")
        .to_str()
        .unwrap();
    assert_eq!(
        forwarded_query, "api_key=test123",
        "query string must be forwarded exactly to the backend"
    );
}
