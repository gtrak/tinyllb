use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use prometheus::TextEncoder;

use crate::gateway::AppState;

/// Serve Prometheus-format metrics at `GET /metrics`.
///
/// Returns `200 OK` with `text/plain; version=0.0.4` per the OpenMetrics
/// specification.
pub async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = state.metrics.registry.gather();
    let body = match encoder.encode_to_string(&metric_families) {
        Ok(encoded) => encoded,
        Err(e) => {
            tracing::error!(error = %e, "failed to encode metrics");
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/plain; version=0.0.4".parse().unwrap(),
    );

    (headers, body).into_response()
}
