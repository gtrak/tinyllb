pub mod error;
pub mod proxy;
pub mod stream;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use url::Url;

use self::proxy::proxy_handler;
use crate::config::Backpressure;
use crate::metrics::Metrics;
use crate::scheduler::FifoScheduler;

/// Application state shared across all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub backend_url: Arc<Url>,
    pub metrics: Arc<Metrics>,
    pub scheduler: Arc<FifoScheduler>,
    pub backpressure: Backpressure,
}

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
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client should build with default TLS")
}
