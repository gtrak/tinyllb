pub mod context;
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
/// - `GET /admin/context` — list all transcript flows.
/// - `GET /admin/context/:flow_id` — full context detail for a flow.
/// - `POST /admin/context/:flow_id/compress` — force-trigger compression.
/// - `DELETE /admin/context/:flow_id` — delete transcript for a flow.
// @lat: [[api#Admin API Router Assembly]]
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/flows", post(flows::register_handler))
        .route("/queue", get(queue::queue_handler))
        .route("/admin/context", get(context::list_flows))
        .route("/admin/context/{flow_id}", get(context::get_flow_context).delete(context::delete_flow_context))
        .route("/admin/context/{flow_id}/compress", post(context::force_compress))
}
