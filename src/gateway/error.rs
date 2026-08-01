use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::fmt;
use std::time::Duration;

/// Errors that can occur while proxying requests to the backend.
pub enum ProxyError {
    /// The backend returned an HTTP error (4xx/5xx). Contains status, headers, body.
    BackendError {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    },
    /// Network error communicating with the backend (connection refused, DNS failure, etc.).
    Network(reqwest::Error),
    /// Internal error (not from reqwest directly).
    Internal(String),
    /// Request body exceeds the configured limit.
    TooLarge,
    /// Request was rejected by the backpressure mechanism.
    Rejected { retry_after: Duration },
}

impl fmt::Debug for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::BackendError { status, .. } => {
                write!(f, "BackendError {{ status: {:?}, .. }}", status)
            }
            ProxyError::Network(e) => write!(f, "Network({:?})", e),
            ProxyError::Internal(msg) => write!(f, "Internal({})", msg),
            ProxyError::TooLarge => write!(f, "TooLarge"),
            ProxyError::Rejected { retry_after } => {
                write!(f, "Rejected {{ retry_after: {:?} }}", retry_after)
            }
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        match self {
            ProxyError::BackendError {
                status,
                headers,
                body,
            } => {
                let mut resp = Response::new(axum::body::Body::from(body));
                *resp.status_mut() = status;
                *resp.headers_mut() = headers;
                resp
            }
            ProxyError::Network(err) => {
                tracing::error!(error = %err, "backend network error");
                (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response()
            }
            ProxyError::Internal(msg) => {
                tracing::error!(error = msg, "internal proxy error");
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
            ProxyError::TooLarge => {
                (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response()
            }
            ProxyError::Rejected { retry_after } => {
                // Retry-After must be integer seconds per RFC 7231.
                let retry_secs = (retry_after.as_secs_f64().ceil()) as u64;
                let retry_after_str = retry_secs.to_string();

                let mut resp = Response::new(axum::body::Body::from(r#"{"error":"queue full"}"#));
                *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                resp.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    axum::http::HeaderValue::from_str(&retry_after_str)
                        .expect("valid integer header value"),
                );
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                resp
            }
        }
    }
}

impl From<reqwest::Error> for ProxyError {
    fn from(err: reqwest::Error) -> Self {
        ProxyError::Network(err)
    }
}
