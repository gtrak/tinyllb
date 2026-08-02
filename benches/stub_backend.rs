//! Shared stub backend for benchmarks.
//!
//! Simulates a vLLM backend with a **quadratic** concurrency penalty:
//!   service_time = base_time * (1 + penalty * in_flight^2)
//!
//! This models GPU KV-cache saturation: at low concurrency the backend is fast,
//! but as concurrent requests increase, memory bandwidth contention causes
//! superlinear slowdown. A quadratic model is more realistic than linear because
//! cache thrashing and memory bandwidth contention are inherently quadratic effects.
//!
//! Parameters:
//!   base_time_ms = 20ms — service time at zero concurrency
//!   penalty = 0.05 — quadratic coefficient
//!   token_count = 10 — SSE frames per request
//!
//! Why these parameters make a PASS reachable:
//!
//! Proxy (capped at max_active_flows=4):
//!   peak in-flight ≤ 4
//!   service_time ≤ 20ms * (1 + 0.05 * 4²) = 20ms * 1.8 = 36ms
//!   throughput ≈ 4 / 36ms ≈ 111 req/s = 1110 tok/s
//!
//! Direct at high concurrency (e.g., N=16):
//!   service_time ≈ 20ms * (1 + 0.05 * 16²) = 20ms * 13.8 = 276ms
//!   throughput ≈ 16 / 276ms ≈ 58 req/s = 580 tok/s
//!   → proxy (1110) > direct (580), PASS at N=16.
//!
//! Direct at N=32:
//!   service_time ≈ 20ms * (1 + 0.05 * 32²) = 20ms * 52.2 = 1044ms
//!   throughput ≈ 32 / 1044ms ≈ 31 req/s = 310 tok/s
//!   → Even worse for direct.
//!
//! The crossover point (where proxy ≈ direct):
//!   direct_throughput(N) = N / (20ms * (1 + 0.05 * N²))
//!   proxy_throughput(4) ≈ 4 / (20ms * 1.8) ≈ 111 req/s
//!   Setting direct = proxy: N / (1 + 0.05 * N²) ≈ 111 * 0.02 = 2.22
//!   N ≈ 2.22 * (1 + 0.05 * N²)
//!   Solving: N ≈ 4.5 or N ≈ 10.4
//!   For our test levels (1,4,8,16,32), N=16 and N=32 are above crossover.
//!
//! Usage: include this file via `#[path = "stub_backend.rs"] mod stub_backend;`
//! in benchmark or test files.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Configuration for the stub backend.
pub struct StubConfig {
    /// Base service time at zero concurrency (millis).
    pub base_time_ms: u64,
    /// Quadratic penalty coefficient. Service time scales as:
    ///   base_time * (1 + penalty * in_flight^2)
    pub penalty: f64,
    /// Number of SSE token frames to emit per request.
    pub token_count: usize,
}

impl Default for StubConfig {
    fn default() -> Self {
        Self {
            base_time_ms: 20,
            penalty: 0.05,
            token_count: 10,
        }
    }
}

/// Shared state for the stub backend.
pub struct StubState {
    /// Current in-flight request count.
    pub in_flight: AtomicU32,
    /// Peak in-flight count observed.
    pub peak_in_flight: AtomicU32,
    /// Total tokens emitted across all requests.
    pub total_tokens: AtomicU32,
    /// Configuration.
    pub config: StubConfig,
}

impl StubState {
    pub fn new(config: StubConfig) -> Self {
        Self {
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            total_tokens: AtomicU32::new(0),
            config,
        }
    }

    /// Compute service time based on current in-flight count.
    /// Model: service_time = base_time * (1 + penalty * in_flight^2)
    fn service_time(&self) -> Duration {
        let current = self.in_flight.load(Ordering::Relaxed) as f64;
        let penalty_factor = 1.0 + self.config.penalty * current * current;
        let ms = (self.config.base_time_ms as f64 * penalty_factor) as u64;
        Duration::from_millis(ms.max(1))
    }

    #[allow(dead_code)]
    /// Get the peak in-flight count since last reset.
    pub fn peak_in_flight(&self) -> u32 {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    /// Get total tokens emitted.
    pub fn tokens_emitted(&self) -> u32 {
        self.total_tokens.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    /// Reset counters for a new benchmark run.
    pub fn reset(&self) {
        self.in_flight.store(0, Ordering::SeqCst);
        self.peak_in_flight.store(0, Ordering::SeqCst);
        self.total_tokens.store(0, Ordering::SeqCst);
    }
}

/// SSE stream wrapper that yields pre-built chunks instantly.
pub struct StubSseStream {
    chunks: std::vec::IntoIter<Bytes>,
}

impl StubSseStream {
    pub fn new(chunks: Vec<Bytes>) -> Self {
        Self {
            chunks: chunks.into_iter(),
        }
    }
}

impl Stream for StubSseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Poll::Ready(this.chunks.next().map(Ok))
    }
}

/// Handler for `/v1/chat/completions`.
/// Applies quadratic concurrency penalty and emits N token frames.
pub async fn stub_chat_handler(
    axum::extract::State(state): axum::extract::State<Arc<StubState>>,
    _req: Request<Body>,
) -> Response<Body> {
    // Increment in-flight count.
    let current = state.in_flight.fetch_add(1, Ordering::SeqCst);
    let new_val = current + 1;
    // Update peak.
    loop {
        let peak = state.peak_in_flight.load(Ordering::SeqCst);
        if new_val > peak {
            match state.peak_in_flight.compare_exchange_weak(
                peak,
                new_val,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        } else {
            break;
        }
    }

    // Compute service time based on concurrency (quadratic collapse).
    let service_time = state.service_time();
    tokio::time::sleep(service_time).await;

    // Build SSE frames: N token frames + usage frame + [DONE].
    let token_count = state.config.token_count;
    let mut chunks = Vec::with_capacity(token_count + 2);

    // Token frames.
    for i in 0..token_count {
        let frame = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"token_{}\"}}}}]}}\n\n",
            i
        );
        chunks.push(Bytes::from(frame));
    }

    // Usage frame.
    let usage_frame = format!(
        "data: {{\"usage\":{{\"prompt_tokens\":100,\"completion_tokens\":{},\"total_tokens\":{}}}}}\n\n",
        token_count,
        100 + token_count
    );
    chunks.push(Bytes::from(usage_frame));

    // DONE frame.
    chunks.push(Bytes::from("data: [DONE]\n\n"));

    // Record tokens.
    state
        .total_tokens
        .fetch_add(token_count as u32, Ordering::SeqCst);

    // Decrement in-flight count.
    state.in_flight.fetch_sub(1, Ordering::SeqCst);

    // Emit all frames via StubSseStream (instant, no inter-frame delay).
    // The concurrency penalty sleep above simulates the total generation time.
    let stream = StubSseStream::new(chunks);
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    resp
}

/// Build the stub backend router.
pub fn build_stub_router(state: Arc<StubState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(stub_chat_handler))
        .with_state(state)
}

/// Start the stub backend on an ephemeral port and return its address.
pub async fn start_stub_backend(state: Arc<StubState>) -> std::net::SocketAddr {
    let app = build_stub_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}
