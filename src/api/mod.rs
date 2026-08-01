pub mod flows;
pub mod queue;

use axum::routing::{get, post};
use axum::Router;

use crate::gateway::AppState;

/// Build the admin router (control-plane endpoints, not under `/v1/...`).
///
/// Mounts:
/// - `POST /flows` — register (or update) a flow's weight/priority.
/// - `GET /queue` — current queue depth, active count, per-flow positions.
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/flows", post(flows::register_handler))
        .route("/queue", get(queue::queue_handler))
}
