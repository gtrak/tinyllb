use axum::{routing::get, Router};
use std::sync::Arc;

use llm_qdisc_proxy::config;
use llm_qdisc_proxy::gateway;

async fn healthz() -> &'static str {
    "ok"
}

pub fn create_router(state: gateway::AppState) -> Router {
    let health_router = Router::new().route("/healthz", get(healthz));
    let gateway_router = gateway::create_router().with_state(state);

    Router::new().merge(health_router).merge(gateway_router)
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

    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(cfg.backend.url),
    };

    let app = create_router(state);

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
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_healthz_returns_ok() {
        let state = gateway::AppState {
            client: gateway::build_client(),
            backend_url: Arc::new(url::Url::parse("http://localhost:8000").unwrap()),
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
