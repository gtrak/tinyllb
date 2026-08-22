pub mod error;
pub mod proxy;
pub mod retry;
pub mod stream;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use url::Url;

use self::proxy::proxy_handler;
use crate::config::{Backpressure, Priorities, RetryPolicy, TransientRetry};
use crate::flow::FlowRegistry;
use crate::metrics::Metrics;
use crate::scheduler::Scheduler;

// @lat: [[gateway#Gateway Application State]]
/// Application state shared across all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub backend_url: Arc<Url>,
    pub metrics: Arc<Metrics>,
    pub scheduler: Arc<Scheduler>,
    pub flow_registry: Arc<FlowRegistry>,
    pub backpressure: Backpressure,
    pub priorities: Priorities,
    /// Optional request-level timeout. When set, cancels forwarded requests
    /// (both streaming and non-streaming) that exceed this duration.
    pub request_timeout: Option<std::time::Duration>,
    /// Inference-watchdog signal from the backend monitor (`true` = engine
    /// deadlocked). Streaming handlers select on this to abort in-flight
    /// backend streams and retry on fresh connections.
    pub stall_rx: tokio::sync::watch::Receiver<bool>,
    pub retry_policy: RetryPolicy,
    /// Proxy-side re-forward of transient backend errors (llama.cpp
    /// context-exceed + network errors from backend restart).
    /// `max_attempts: 0` disables. See plan 007.
    pub transient_retry: TransientRetry,
    /// llama.cpp slot count for `id_slot` session pinning (mirrors
    /// `--parallel`). `None` disables pinning. See plan 009.
    pub llamacpp_slots: Option<u32>,
}

impl AppState {
    /// Construct a test `AppState` with the given core fields, filling in
    /// defaults for backpressure, priorities, request_timeout, stall_rx,
    /// and retry_policy. Tests can override individual fields with struct
    /// update syntax:
    ///   `AppState { retry_policy: custom, ..AppState::test_default(...) }`
    pub fn test_default(
        client: reqwest::Client,
        backend_url: Arc<Url>,
        metrics: Arc<Metrics>,
        scheduler: Arc<Scheduler>,
        flow_registry: Arc<FlowRegistry>,
    ) -> Self {
        AppState {
            client,
            backend_url,
            metrics,
            scheduler,
            flow_registry,
            backpressure: Backpressure::default(),
            priorities: Priorities::default(),
            request_timeout: None,
            stall_rx: crate::backend::BackendMonitor::empty().stall_receiver(),
            retry_policy: RetryPolicy::default(),
            transient_retry: TransientRetry::default(),
            llamacpp_slots: None,
        }
    }
}

// @lat: [[gateway#Reverse Proxy Request Handling]]
/// Create the gateway router that handles OpenAI-compatible routes.
///
/// Mounts:
/// - `POST /v1/chat/completions`
/// - `POST /v1/completions`
/// - `GET /v1/models`
///
/// All requests are proxied to the vLLM backend.
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(proxy_handler))
        .route("/v1/completions", post(proxy_handler))
        .route("/v1/models", get(proxy_handler))
}

/// Build the reqwest HTTP client with sensible defaults.
///
/// A `pool_idle_timeout` of 30s evicts pooled connections before they go stale
/// behind proxies (e.g. pasta) that silently drop idle TCP. `tcp_keepalive`
/// keeps live connections probed. Without these, long-running instances
/// accumulate dead pooled connections that add latency or cause failures.
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client should build with default TLS")
}

/// Build a short-timeout reqwest client for backend `/metrics` polling.
///
/// The BackendMonitor polls every second; a hung scrape should fail fast
/// (not hold the watch channel's last snapshot for minutes). This client
/// uses a 3s timeout so the monitor always reflects recent state.
pub fn build_monitor_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .pool_idle_timeout(std::time::Duration::from_secs(10))
        .tcp_keepalive(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest monitor client should build")
}
