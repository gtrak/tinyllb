use axum::{routing::get, Router};
use std::sync::Arc;

use llm_qdisc_proxy::config;
use llm_qdisc_proxy::flow::FlowRegistry;
use llm_qdisc_proxy::gateway;
use llm_qdisc_proxy::metrics;

async fn healthz() -> &'static str {
    "ok"
}

pub fn create_router(state: gateway::AppState) -> Router {
    let health_router = Router::new().route("/healthz", get(healthz));
    let metrics_router = Router::new()
        .route("/metrics", get(metrics::endpoint::metrics_handler))
        .with_state(state.clone());
    let gateway_router = gateway::create_router().with_state(state.clone());
    let admin_router = llm_qdisc_proxy::api::create_router().with_state(state);

    Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router)
}

/// Spawn a background task that periodically recomputes
/// `llm_tokens_per_second` from the counter's rate window.
fn spawn_token_rate_task(metrics: Arc<llm_qdisc_proxy::metrics::Metrics>) {
    let tokens_total = metrics.tokens_generated_total.clone();
    let tokens_per_second = metrics.tokens_per_second.clone();

    tokio::spawn(async move {
        let mut previous_count: f64 = 0.0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let current_count = tokens_total.get();
            let delta = if current_count >= previous_count {
                current_count - previous_count
            } else {
                0.0
            };
            tokens_per_second.set(delta);
            previous_count = current_count;
        }
    });
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let cfg = config::load().expect("failed to load configuration");
    tracing::info!(?cfg, "config loaded");

    let addr = if std::env::var("LLM_QDISC__SERVER__BIND").is_ok() {
        cfg.server.bind
    } else if let Ok(port_str) = std::env::var("PORT") {
        let port: u16 = port_str.parse().expect("PORT must be a valid port number");
        format!("0.0.0.0:{port}")
            .parse::<std::net::SocketAddr>()
            .unwrap()
    } else {
        cfg.server.bind
    };

    let metrics = metrics::create_metrics();

    let flow_registry = Arc::new(FlowRegistry::new(
        cfg.flows.default_weight,
        cfg.flows.default_priority,
    ));

    let scheduler = llm_qdisc_proxy::scheduler::FifoScheduler::new(
        cfg.scheduler.max_active_flows,
        metrics.clone(),
        flow_registry.clone(),
        cfg.backpressure.mode,
        cfg.backpressure.max_queue_depth,
        cfg.backpressure.max_wait,
        cfg.backpressure.retry_after_base,
    );

    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(cfg.backend.url),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: cfg.backpressure,
    };

    let app = create_router(state);

    // Spawn background task for tokens-per-second gauge.
    spawn_token_rate_task(metrics);

    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr.to_string())
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use llm_qdisc_proxy::config::BackpressureMode;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_healthz_returns_ok() {
        let metrics = metrics::create_metrics();
        let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = llm_qdisc_proxy::scheduler::FifoScheduler::new(
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
            backend_url: Arc::new(url::Url::parse("http://localhost:8000").unwrap()),
            metrics: metrics.clone(),
            scheduler: Arc::new(scheduler),
            flow_registry,
            backpressure: config::Backpressure::default(),
        };
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(body, "ok");
    }
}
