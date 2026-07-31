use axum::{routing::get, Router};

use llm_qdisc_proxy::config;

async fn healthz() -> &'static str {
    "ok"
}

pub fn create_router() -> Router {
    Router::new().route("/healthz", get(healthz))
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

    let app = create_router();

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
        let app = create_router();
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
