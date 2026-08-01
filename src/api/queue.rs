use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::gateway::AppState;

/// Response body for `GET /queue`.
#[derive(Debug, Serialize)]
pub struct QueueResponse {
    /// Number of active flows (currently executing at the backend).
    pub active: u64,
    /// Number of requests currently waiting in the queue.
    pub waiting: u64,
    /// Per-flow waiting positions (only for flows currently queued).
    /// Ordered by queue position. `position` is 1-indexed.
    pub flows: Vec<FlowPosition>,
}

/// Per-flow entry in the queue response.
#[derive(Debug, Serialize)]
pub struct FlowPosition {
    pub id: String,
    /// 1-indexed position in the queue.
    pub position: u64,
}

/// Handler for `GET /queue`.
///
/// Returns the current queue state including active count,
/// waiting count, and per-flow positions.
pub async fn queue_handler(State(state): State<AppState>) -> Json<QueueResponse> {
    let snapshot = state.scheduler.queue_snapshot();
    Json(QueueResponse {
        active: snapshot.active,
        waiting: snapshot.waiting,
        flows: snapshot
            .flows
            .iter()
            .map(|e| FlowPosition {
                id: e.id.clone(),
                position: e.position,
            })
            .collect(),
    })
}
