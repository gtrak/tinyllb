use axum::{routing::get, Router};
use std::sync::Arc;

use tinyllb::config;
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;

async fn healthz() -> &'static str {
    "ok"
}

// @lat: [[app#Application Composition and Startup]]
pub fn create_router(state: gateway::AppState) -> Router {
    let health_router = Router::new().route("/healthz", get(healthz));
    let metrics_router = Router::new()
        .route("/metrics", get(metrics::endpoint::metrics_handler))
        .with_state(state.clone());
    let gateway_router = gateway::create_router().with_state(state.clone());
    let admin_router = tinyllb::api::create_router().with_state(state);

    Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .merge(admin_router)
}

/// Spawn a background task that periodically recomputes
/// `llm_tokens_per_second` as a rolling average of the counter's per-second
/// deltas over `window_secs`. This smooths the lumpy updates that occur when
/// tokens are credited in a batch at request completion.
// @lat: [[app#Token Rate Gauge Task]]
pub fn spawn_token_rate_task(metrics: &Arc<tinyllb::metrics::Metrics>, window_secs: u64) {
    let tokens_total = metrics.tokens_generated_total.clone();
    let tokens_per_second = metrics.tokens_per_second.clone();
    let window_secs = window_secs.max(1);

    tokio::spawn(async move {
        let mut samples: Vec<f64> = Vec::with_capacity(window_secs as usize);
        let mut previous_count: f64 = 0.0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let current_count = tokens_total.get();
            let delta = if current_count >= previous_count {
                current_count - previous_count
            } else {
                0.0
            };
            previous_count = current_count;

            samples.push(delta);
            if samples.len() > window_secs as usize {
                samples.remove(0);
            }
            let sum: f64 = samples.iter().sum();
            tokens_per_second.set(sum / samples.len() as f64);
        }
    });
}

#[tokio::main]
async fn main() {
    tinyllb::telemetry::init();

    let cfg = config::load().expect("failed to load configuration");
    tracing::info!(?cfg, "config loaded");

    let addr = if std::env::var("TINYLLB__SERVER__BIND").is_ok() {
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

    // Create the backend monitor. Always start the polling task so that
    // backend gauges (kv_cache_usage, kv_cache_free) are populated for
    // observability regardless of whether KV admission policy is enabled.
    // The KvPolicy itself short-circuits to Accept when disabled.
    let client = gateway::build_client();
    let (monitor, monitor_task) = tinyllb::backend::BackendMonitor::new(
        &cfg.backend,
        metrics.clone(),
        client.clone(),
    );
    if let Some(task) = monitor_task {
        tokio::spawn(task);
    }
    let monitor = Arc::new(monitor);

    let scheduler = tinyllb::scheduler::Scheduler::new(
        cfg.scheduler.algorithm,
        cfg.scheduler.max_active_flows,
        metrics.clone(),
        flow_registry.clone(),
        cfg.backpressure.mode,
        cfg.backpressure.max_queue_depth,
        cfg.backpressure.max_wait,
        cfg.backpressure.retry_after_base,
        cfg.scheduler.starvation_timeout,
        cfg.scheduler.completion_bias.clone(),
        cfg.kv_policy.clone(),
        monitor.clone(),
        cfg.priority_policy.clone(),
        cfg.priorities.clone(),
    );

    // Initialize context-compression state when enabled.
    let context_state: Option<Arc<tinyllb::context::ContextState>> = if cfg.context_policy.enabled {
        match async {
            let (tx, rx) = tokio::sync::mpsc::channel::<tinyllb::context::CompressionJob>(64);
            let state = Arc::new(tinyllb::context::ContextState::new(cfg.context_policy.clone(), tx, metrics.clone()).await?);
            let n = state.find_flows_needing_compression().await?;
            if n > 0 { tracing::info!(n, "enqueued compression jobs for over-threshold flows at startup"); }
            // Spawn the compression worker (consumes jobs from the channel).
            // Sidecar requests go directly to the vLLM backend (not through the
            // proxy) to avoid self-referential HTTP.
            let worker = tinyllb::context::compressor::CompressionWorker::new(
                rx,
                Arc::clone(&state),
                cfg.backend.url.clone(),
                client.clone(),
            );
            tokio::spawn(async move { worker.run().await });
            anyhow::Ok(Some(state))
        }.await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "failed to initialize context state, continuing without compression");
                None
            }
        }
    } else {
        None
    };

    let state = gateway::AppState {
        client,
        backend_url: Arc::new(cfg.backend.url),
        metrics: metrics.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: cfg.backpressure,
        priorities: cfg.priorities.clone(),
        request_timeout: cfg.request_timeout,
        context: context_state,
        retry_policy: cfg.retry_policy.clone(),
    };

    let app = create_router(state);

    // Spawn background task for tokens-per-second gauge (rolling average).
    spawn_token_rate_task(&metrics, cfg.server.tps_window_secs);

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
    use tinyllb::config::BackpressureMode;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_healthz_returns_ok() {
        let metrics = metrics::create_metrics();
        let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = tinyllb::scheduler::Scheduler::new_with_defaults(
            tinyllb::config::Algorithm::Fifo,
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
            priorities: config::Priorities::default(),
            request_timeout: None,
            context: None,
            retry_policy: config::RetryPolicy::default(),
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
