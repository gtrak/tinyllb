use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::flow::{FlowId, FlowRegistration};
use crate::gateway::AppState;

/// Request body for `POST /flows`.
#[derive(Debug, Deserialize)]
pub struct RegisterFlowRequest {
    /// Flow identifier.
    pub id: String,
    /// Scheduling weight (must be > 0).
    pub weight: f64,
    /// Priority class (must be in [0, 100]).
    pub priority: u32,
}

/// Response body for `POST /flows`.
#[derive(Debug, Serialize)]
pub struct RegisterFlowResponse {
    pub id: String,
    pub weight: f64,
    pub priority: u32,
    /// `"created"` for new flows, `"updated"` for existing flows.
    pub status: String,
    /// Source of the priority value: 0=heuristic, 1=header, 2=admin.
    pub priority_source: u8,
}

/// Handler for `POST /flows`.
///
/// Upserts a flow into the registry with explicit weight/priority.
/// Returns `201 Created` for new flows, `200 OK` for updates.
/// Returns `400 Bad Request` if validation fails.
// @lat: [[api#Flow Registration Endpoint]]
pub async fn register_handler(
    State(state): State<AppState>,
    Json(req): Json<RegisterFlowRequest>,
) -> Result<(StatusCode, Json<RegisterFlowResponse>), (StatusCode, String)> {
    // Validate weight.
    if req.weight <= 0.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "weight must be greater than 0".to_string(),
        ));
    }

    // Validate priority.
    if req.priority > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "priority must be between 0 and 100".to_string(),
        ));
    }

    let flow_id = FlowId::new(req.id.clone());
    let metric_label = flow_id.metric_label().to_string();
    let is_new = state.flow_registry.register(FlowRegistration {
        id: flow_id,
        weight: req.weight,
        priority: req.priority,
    });

    state
        .metrics
        .flow_priority_source_total
        .with_label_values(&[&metric_label, "admin"])
        .inc();

    Ok((
        if is_new {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(RegisterFlowResponse {
            id: req.id,
            weight: req.weight,
            priority: req.priority,
            status: if is_new {
                "created".to_string()
            } else {
                "updated".to_string()
            },
            priority_source: 2,
        }),
    ))
}
