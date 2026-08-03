# Issue 10 — Admin API

## Objective

Add admin endpoints for inspecting transcript state, force-triggering
compression, and clearing transcripts. These are essential for debugging
and operational visibility.

## Files

| File | Change |
|------|--------|
| `src/api/mod.rs` | Add routes for `/admin/context/*` |
| `src/api/context.rs` | New — handlers for context admin endpoints |
| `src/gateway/mod.rs` | Wire admin routes into the router |

## Prerequisites

- Issue 04 (store — `TranscriptStore`, `TranscriptMeta`)
- Issue 06 (context state)
- Issue 09 (compression worker — for force-trigger via channel)

## Steps

1. **Routes** (added to the router in `src/gateway/mod.rs` or `src/api/mod.rs`):
   ```
   GET    /admin/context                    — list all flows with summaries
   GET    /admin/context/:flow_id           — segment breakdown for one flow
   POST   /admin/context/:flow_id/compress  — force-trigger compression
   DELETE /admin/context/:flow_id           — clear transcript for a flow
   ```

2. **`GET /admin/context`** — list all flows:
   - Query `transcript_meta` for all rows
   - Return JSON array of `TranscriptMeta`:
     ```json
     [
       {
         "flow_id": "session-abc",
         "head_turns": 3,
         "live_turns": 6,
         "compressed_count": 2,
         "last_compressed_turn": 12,
         "total_est_tokens": 45000,
         "total_raw_est_tokens": 120000,
         "savings_tokens": 75000,
         "updated_at": "2026-08-03T14:22:01Z"
       },
       ...
     ]
     ```
   - Add `?over_threshold=true` query param to filter flows over
     `compress_threshold` (useful for finding flows needing attention)

3. **`GET /admin/context/:flow_id`** — detailed segment breakdown:
   - Load full transcript (all segments)
   - Return JSON:
     ```json
     {
       "flow_id": "session-abc",
       "segments": [
         {
           "segment_idx": 0,
           "kind": "head",
           "msg_range": [0, 6],
           "est_tokens": 3200,
           "raw_est_tokens": 3200,
           "message_count": 6,
           "preview": "System: You are... | User: ...",
           "created_at": "..."
         },
         {
           "segment_idx": 1,
           "kind": "compressed",
           "msg_range": [6, 18],
           "est_tokens": 1800,
           "raw_est_tokens": 12000,
           "summary_preview": "Summary of turns 3-8: The user asked about...",
           "created_at": "..."
         },
         {
           "segment_idx": 2,
           "kind": "live",
           "msg_range": [18, 24],
           "est_tokens": 5200,
           "raw_est_tokens": 5200,
           "message_count": 6,
           "preview": "User: ... | Assistant: ...",
           "created_at": "..."
         }
       ],
       "total_est_tokens": 10200,
       "total_raw_est_tokens": 20400,
       "savings_tokens": 10200,
       "compress_threshold": 100000,
       "needs_compression": false
     }
     ```
   - `preview` = first 200 chars of the first message content (for
     debugging — not the full message)
   - `summary_preview` = first 200 chars of the summary message

4. **`POST /admin/context/:flow_id/compress`** — force-trigger:
   - Load transcript, find the oldest uncompacted turns in Live
   - If no turns available to compress: return 409 Conflict with
     `{"error": "no compressible turns available"}`
   - Enqueue a `CompressionJob` directly to the channel
   - Return 202 Accepted:
     ```json
     {
       "flow_id": "session-abc",
       "turn_range": [6, 14],
       "status": "enqueued"
     }
     ```

5. **`DELETE /admin/context/:flow_id`** — clear transcript:
   - `store.delete_transcript(flow_id)`
   - Return 200 OK:
     ```json
     {"flow_id": "session-abc", "deleted": true}
     ```
   - If flow doesn't exist: return 404

6. **Handler signatures** in `src/api/context.rs`:
   ```rust
   pub async fn list_flows(
       State(state): State<AppState>,
       Query(params): Query<ListParams>,
   ) -> Result<Json<Vec<TranscriptMetaSnapshot>>, ApiError>

   pub async fn get_flow_context(
       State(state): State<AppState>,
       Path(flow_id): Path<String>,
   ) -> Result<Json<FlowContextDetail>, ApiError>

   pub async fn force_compress(
       State(state): State<AppState>,
       Path(flow_id): Path<String>,
   ) -> Result<(StatusCode, Json<CompressResponse>), ApiError>

   pub async fn delete_flow_context(
       State(state): State<AppState>,
       Path(flow_id): Path<String>,
   ) -> Result<Json<DeleteResponse>, ApiError>
   ```

7. **Error handling**:
   - If `context_policy.enabled = false`: all endpoints return 503 with
     `{"error": "context compression is disabled"}`
   - If store error: return 500 with error message
   - If flow not found: return 404

8. **Router wiring** in `src/gateway/mod.rs`:
   ```rust
   let app = Router::new()
       .route("/healthz", get(health_handler))
       .route("/metrics", get(metrics_handler))
       // ... existing routes ...
       .route("/admin/context", get(api::context::list_flows))
       .route("/admin/context/:flow_id", get(api::context::get_flow_context))
       .route("/admin/context/:flow_id/compress", post(api::context::force_compress))
       .route("/admin/context/:flow_id", delete(api::context::delete_flow_context))
       .with_state(state);
   ```

9. **Response types** — define in `src/api/context.rs`:
   ```rust
   #[derive(serde::Serialize)]
   struct TranscriptMetaSnapshot {
       flow_id: String,
       head_turns: i32,
       live_turns: i32,
       compressed_count: i32,
       total_est_tokens: i32,
       total_raw_est_tokens: i32,
       savings_tokens: i32,
       updated_at: String,
   }

   #[derive(serde::Serialize)]
   struct SegmentDetail {
       segment_idx: i32,
       kind: String,
       msg_range: [i32; 2],
       est_tokens: i32,
       raw_est_tokens: i32,
       message_count: i32,
       preview: String,
       summary_preview: Option<String>,
       created_at: String,
   }

   #[derive(serde::Serialize)]
   struct FlowContextDetail {
       flow_id: String,
       segments: Vec<SegmentDetail>,
       total_est_tokens: i32,
       total_raw_est_tokens: i32,
       savings_tokens: i32,
       compress_threshold: usize,
       needs_compression: bool,
   }
   ```

## Tests

- `test_list_flows_empty` — no transcripts → empty array
- `test_list_flows_over_threshold` — 3 flows, 1 over threshold,
  `?over_threshold=true` → 1 result
- `test_get_flow_detail` — transcript with 3 segments → correct breakdown
   with previews
- `test_get_flow_not_found` — 404
- `test_force_compress` — enqueue job → 202 Accepted with turn_range
- `test_force_compress_no_compressible` — Live too short → 409
- `test_delete_flow` — transcript deleted → subsequent GET returns 404
- `test_endpoints_disabled` — `context_policy.enabled = false` → 503

## Verification

```bash
cargo test --lib api_context 2>&1 | tail -10
# Manual check once running:
curl http://localhost:1234/admin/context | jq
curl http://localhost:1234/admin/context/session-abc | jq
curl -X POST http://localhost:1234/admin/context/session-abc/compress | jq
curl -X DELETE http://localhost:1234/admin/context/session-abc | jq
```
