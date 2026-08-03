//! Admin endpoints for inspecting transcript state, force-triggering compression,
//! and clearing transcripts.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use crate::context::segment::{Segment, SegmentKind};
use crate::context::CompressionJob;
use crate::gateway::AppState;

// ── Response types ────────────────────────────────────────────────────────────

/// Snapshot of a single flow's transcript metadata.
#[derive(Debug, Serialize)]
pub struct TranscriptMetaSnapshot {
    pub flow_id: String,
    pub head_turns: i32,
    pub live_turns: i32,
    pub compressed_count: i32,
    pub total_est_tokens: i32,
    pub total_raw_est_tokens: i32,
    pub savings_tokens: i32,
    pub updated_at: String,
}

/// Detail about one segment within a flow's context.
#[derive(Debug, Serialize)]
pub struct SegmentDetail {
    pub segment_idx: i32,
    pub kind: String,
    pub msg_range: [i32; 2],
    pub est_tokens: i32,
    pub raw_est_tokens: i32,
    pub message_count: i32,
    pub preview: String,
    pub summary_preview: Option<String>,
    pub created_at: String,
}

/// Full context detail for a single flow.
#[derive(Debug, Serialize)]
pub struct FlowContextDetail {
    pub flow_id: String,
    pub segments: Vec<SegmentDetail>,
    pub total_est_tokens: i32,
    pub total_raw_est_tokens: i32,
    pub savings_tokens: i32,
    pub compress_threshold: usize,
    pub needs_compression: bool,
}

/// Response from a force-compress request.
#[derive(Debug, Serialize)]
pub struct CompressResponse {
    pub flow_id: String,
    pub turn_range: [usize; 2],
    pub status: String,
}

/// Response from a delete request.
#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    pub flow_id: String,
    pub deleted: bool,
}

/// Query parameters for listing flows.
#[derive(Debug, serde::Deserialize)]
pub struct ListFlowsQuery {
    pub over_threshold: Option<bool>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn disabled_error() -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "context compression is disabled".to_string(),
    )
}

fn segment_kind_to_str(kind: &SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Head => "head",
        SegmentKind::Compressed => "compressed",
        SegmentKind::Live => "live",
    }
}

/// Extract the first 200 characters of a message's content string.
fn message_preview(content: &serde_json::Value) -> String {
    let s = content
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let preview: String = s.chars().take(200).collect();
    preview
}

/// Build a `SegmentDetail` from a `Segment`.
fn segment_to_detail(seg: &Segment) -> SegmentDetail {
    let preview = if !seg.raw_messages.is_empty() {
        message_preview(&seg.raw_messages[0])
    } else {
        String::new()
    };

    let summary_preview = seg
        .summary_message
        .as_ref()
        .map(message_preview);

    SegmentDetail {
        segment_idx: seg.segment_idx,
        kind: segment_kind_to_str(&seg.kind).to_string(),
        msg_range: [seg.msg_start_idx, seg.msg_end_idx],
        est_tokens: seg.est_tokens,
        raw_est_tokens: seg.raw_est_tokens,
        message_count: seg.raw_messages.len() as i32,
        preview,
        summary_preview,
        created_at: seg.created_at.to_rfc3339(),
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// GET /admin/context — list all flows or only those over threshold.
pub async fn list_flows(
    State(state): State<AppState>,
    Query(query): Query<ListFlowsQuery>,
) -> Result<Json<Vec<TranscriptMetaSnapshot>>, (StatusCode, String)> {
    let ctx = match &state.context {
        Some(ctx) => ctx,
        None => return Err(disabled_error()),
    };

    let metas = match query.over_threshold {
        Some(true) => {
            let flow_ids = ctx
                .store
                .list_flows_over_threshold(ctx.config.compress_threshold)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let mut metas = Vec::new();
            for fid in &flow_ids {
                if let Some(meta) = ctx.store.get_meta(fid).await.map_err(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                })? {
                    metas.push(meta);
                }
            }
            metas
        }
        _ => {
            ctx.store
                .list_all_meta()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
    };

    let snapshots: Vec<TranscriptMetaSnapshot> = metas
        .iter()
        .map(|m| TranscriptMetaSnapshot {
            flow_id: m.flow_id.clone(),
            head_turns: m.head_turns,
            live_turns: m.live_turns,
            compressed_count: m.compressed_count,
            total_est_tokens: m.total_est_tokens,
            total_raw_est_tokens: m.total_raw_est_tokens,
            savings_tokens: m.total_raw_est_tokens.saturating_sub(m.total_est_tokens),
            updated_at: m.updated_at.clone(),
        })
        .collect();

    Ok(Json(snapshots))
}

/// GET /admin/context/:flow_id — full context detail for a flow.
pub async fn get_flow_context(
    State(state): State<AppState>,
    Path(flow_id): Path<String>,
) -> Result<Json<FlowContextDetail>, (StatusCode, String)> {
    let ctx = match &state.context {
        Some(ctx) => ctx,
        None => return Err(disabled_error()),
    };

    let transcript = ctx
        .store
        .load_transcript(&flow_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if transcript.segments.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("transcript not found for flow: {}", flow_id),
        ));
    }

    let segments: Vec<SegmentDetail> = transcript
        .segments
        .iter()
        .map(segment_to_detail)
        .collect();

    let total_est = transcript.total_est_tokens();
    let total_raw = transcript.total_raw_est_tokens();
    let savings = total_raw.saturating_sub(total_est);

    Ok(Json(FlowContextDetail {
        flow_id: transcript.flow_id,
        segments,
        total_est_tokens: total_est as i32,
        total_raw_est_tokens: total_raw as i32,
        savings_tokens: savings as i32,
        compress_threshold: ctx.config.compress_threshold,
        needs_compression: total_est > ctx.config.compress_threshold,
    }))
}

/// POST /admin/context/:flow_id/compress — force-trigger compression.
pub async fn force_compress(
    State(state): State<AppState>,
    Path(flow_id): Path<String>,
) -> Result<(StatusCode, Json<CompressResponse>), (StatusCode, String)> {
    let ctx = match &state.context {
        Some(ctx) => ctx,
        None => return Err(disabled_error()),
    };

    let transcript = ctx
        .store
        .load_transcript(&flow_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Find the Live segment to determine compressible turns.
    let live = transcript
        .live_segment()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("no live segment for flow: {}", flow_id),
            )
        })?;

    // Find turn boundaries within the live segment's messages.
    let boundaries =
        crate::context::segment::find_turn_boundaries(&live.raw_messages);

    // We need at least 2 turns in the live segment to compress
    // (compress the oldest, keep the newest as live).
    if boundaries.len() < 2 {
        return Err((
            StatusCode::CONFLICT,
            "no compressible turns available".to_string(),
        ));
    }

    // The oldest uncompacted turn starts at boundary[0] and ends at boundary[1].
    // We compress everything up to the second-to-last turn boundary.
    let compress_end = boundaries[boundaries.len() - 1];

    let job = CompressionJob {
        flow_id: flow_id.clone(),
        turn_range_start: 0,
        turn_range_end: compress_end,
        enqueued_at: std::time::Instant::now(),
    };

    ctx.trigger_compression(job)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((
        StatusCode::ACCEPTED,
        Json(CompressResponse {
            flow_id,
            turn_range: [0, compress_end],
            status: "enqueued".to_string(),
        }),
    ))
}

/// DELETE /admin/context/:flow_id — delete all segments and metadata.
pub async fn delete_flow_context(
    State(state): State<AppState>,
    Path(flow_id): Path<String>,
) -> Result<Json<DeleteResponse>, (StatusCode, String)> {
    let ctx = match &state.context {
        Some(ctx) => ctx,
        None => return Err(disabled_error()),
    };

    ctx.store
        .delete_transcript(&flow_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DeleteResponse {
        flow_id,
        deleted: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcript_meta_snapshot_serialization() {
        let snapshot = TranscriptMetaSnapshot {
            flow_id: "test-flow".to_string(),
            head_turns: 5,
            live_turns: 3,
            compressed_count: 2,
            total_est_tokens: 500,
            total_raw_est_tokens: 1200,
            savings_tokens: 700,
            updated_at: "2025-06-01T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("deserialize snapshot");

        assert_eq!(parsed["flow_id"], "test-flow");
        assert_eq!(parsed["savings_tokens"], 700);
        assert_eq!(parsed["head_turns"], 5);
    }

    #[test]
    fn test_compress_response_serialization() {
        let resp = CompressResponse {
            flow_id: "test-flow".to_string(),
            turn_range: [5, 20],
            status: "enqueued".to_string(),
        };

        let json = serde_json::to_string(&resp).expect("serialize compress response");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("deserialize compress response");

        assert_eq!(parsed["flow_id"], "test-flow");
        assert_eq!(parsed["turn_range"][0], 5);
        assert_eq!(parsed["turn_range"][1], 20);
        assert_eq!(parsed["status"], "enqueued");
    }

    #[test]
    fn test_delete_response_serialization() {
        let resp = DeleteResponse {
            flow_id: "test-flow".to_string(),
            deleted: true,
        };

        let json = serde_json::to_string(&resp).expect("serialize delete response");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("deserialize delete response");

        assert_eq!(parsed["flow_id"], "test-flow");
        assert_eq!(parsed["deleted"], true);
    }

    #[test]
    fn test_flow_context_detail_serialization() {
        let detail = FlowContextDetail {
            flow_id: "test-flow".to_string(),
            segments: vec![SegmentDetail {
                segment_idx: 0,
                kind: "head".to_string(),
                msg_range: [0, 10],
                est_tokens: 100,
                raw_est_tokens: 100,
                message_count: 5,
                preview: "hello world".to_string(),
                summary_preview: None,
                created_at: "2025-01-01T00:00:00Z".to_string(),
            }],
            total_est_tokens: 500,
            total_raw_est_tokens: 1000,
            savings_tokens: 500,
            compress_threshold: 800,
            needs_compression: false,
        };

        let json = serde_json::to_string(&detail).expect("serialize flow context detail");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("deserialize flow context detail");

        assert_eq!(parsed["flow_id"], "test-flow");
        assert_eq!(parsed["segments"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["savings_tokens"], 500);
        assert!(!parsed["needs_compression"].as_bool().unwrap());
    }

    #[test]
    fn test_message_preview_truncation() {
        let long_content = "x".repeat(300);
        let msg = serde_json::json!({ "content": long_content });
        let preview = message_preview(&msg);
        assert_eq!(preview.len(), 200);
    }

    #[test]
    fn test_message_preview_no_content_field() {
        let msg = serde_json::json!({ "role": "user" });
        let preview = message_preview(&msg);
        assert!(preview.is_empty());
    }

    #[test]
    fn test_disabled_error() {
        let (status, msg) = disabled_error();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(msg, "context compression is disabled");
    }
}
