# Issue 12 — Integration Tests

## Objective

End-to-end tests that verify the full compression flow: request arrives →
proxy reconciles → body rewritten → compression triggered → worker
summarizes → segments stored → subsequent requests use compressed context.

These tests use a mock vLLM backend (stub TCP server returning canned
completions) to test without a real model.

## Files

| File | Change |
|------|--------|
| `tests/context_compression.rs` | New — end-to-end compression tests |
| `tests/context_reconcile.rs` | New — reconciliation edge case tests |
| `tests/mock_backend.rs` | New — shared mock vLLM backend helper (if not already existing in test infra) |

## Prerequisites

- All implementation issues 01–11

## Test infrastructure

### Mock backend

A stub HTTP server that:
- Accepts `POST /v1/chat/completions`
- Returns canned responses:
  - For `X-LLM-Internal: compressor` requests: returns a short summary string
  - For normal requests: returns a short assistant message
- Records all received requests (messages array, headers, body) for assertions
- Can be configured to return errors (for retry testing)

```rust
struct MockBackend {
    received_requests: Arc<Mutex<Vec<RecordedRequest>>>,
    responses: VecDeque<String>,  // queue of responses to return
}
```

If the existing test suite already has a mock backend (in
`tests/gateway.rs` or a shared helper), reuse and extend it. The existing
tests use real TCP sockets with stub handlers, so this pattern should
already exist.

### Test proxy setup

Build an `AppState` with:
- Real `ContextState` (SQLite in-memory or temp file, real token estimator
  with test tokenizer or heuristic mode)
- Real scheduler (FIFO with `max_active_flows = 4`)
- Mock backend URL
- `context_policy.enabled = true`

## Tests

### `tests/context_compression.rs`

1. **`test_full_compression_flow`**:
   - Start proxy with `compress_threshold = 500` (low for testing)
   - Send 10 chat completion requests for the same flow, each appending
     a turn with ~100 tokens
   - After request 6 (total > 500), compression should trigger
   - Verify: mock backend's 7th request onwards receives a `messages` array
     containing a `{role: "system", content: "Summary of turns..."}` in the
     middle (between head and live)
   - Verify: total forwarded tokens stays near threshold (±200)

2. **`test_prefix_cache_stability`**:
   - Send 5 requests, trigger compression after request 3
   - Verify: the messages array sent to the backend for requests 4 and 5
     share the SAME prefix (bytes 0..N) — i.e., `[Head + Compressed]` is
     identical across both requests
   - Use `RecordedRequest` to compare message prefix byte-for-byte

3. **`test_sidecar_is_background_priority`**:
   - Register flows via `POST /flows` with known priorities
   - Send a compression-triggering request
   - Verify: the sidecar request has `X-LLM-Flow-ID: compressor:{flow_id}`
   - Verify: the compressor flow was registered with `background` priority

4. **`test_sidecar_skips_compression`**:
   - Send a compression request that triggers a sidecar
   - Verify: the sidecar request's body is NOT modified by `rewrite_messages`
     (no additional compression recursion)
   - Check: sidecar request has `X-LLM-Internal: compressor` header

5. **`test_persistence_across_restart`**:
   - Send 5 requests, trigger compression
   - Stop proxy, restart with same SQLite file
   - Send another request for the same flow
   - Verify: forwarded messages include the stored compressed segments
   - Verify: `GET /admin/context/{flow_id}` shows the persisted segments

6. **`test_conversation_reset_detection`**:
   - Send 5 requests with flow_id "A" (system prompt "You are a coder")
   - Send a 6th request with flow_id "A" but system prompt "You are a poet"
   - Verify: `transcript_reset = true` (check via admin API or metrics)
   - Verify: new transcript created, old segments not forwarded

7. **`test_fail_open_on_store_error`**:
   - Point `store_path` to a read-only directory (or use a store that returns
     errors)
   - Send a request
   - Verify: proxy returns 200 (not 500), body forwarded to backend unchanged

8. **`test_disabled_no_compression`**:
   - `context_policy.enabled = false`
   - Send 20 requests (way over threshold)
   - Verify: no compressed segments in forwarded bodies
   - Verify: admin endpoints return 503

9. **`test_admin_force_compress`**:
   - Send 3 requests (under threshold, no auto-compression)
   - `POST /admin/context/{flow_id}/compress`
   - Verify: 202 Accepted, sidecar request sent, Compressed segment created

10. **`test_admin_get_context_detail`**:
    - Build up a transcript with head + 2 compressed + live
    - `GET /admin/context/{flow_id}`
    - Verify: response contains all segments with correct kinds, token counts,
      and previews

11. **`test_multi_flow_isolation`**:
    - Send requests for flow "A" and flow "B" concurrently
    - Trigger compression for flow "A"
    - Verify: flow "B"'s messages are NOT affected by flow "A"'s compression

12. **`test_metrics_exposed`**:
    - Trigger compression
    - `GET /metrics`
    - Verify: `llm_qdisc_context_compression_events_total` incremented
    - Verify: `llm_qdisc_context_compression_tokens_saved_total` > 0
    - Verify: `llm_qdisc_context_estimated_tokens{flow_id="..."}` present

### `tests/context_reconcile.rs`

Unit-level tests for reconciliation edge cases (already specified in issue 05,
duplicated here for the integration test file):

1. **`test_tool_call_grouping`** — messages with `tool_calls` and `role: "tool"`
   are grouped into the correct turn
2. **`test_multimodal_content`** — messages with array content (text + image)
   handled correctly
3. **`test_large_conversation`** — 100 turns, verify reconciliation completes
   in reasonable time (< 100ms)
4. **`test_incoming_missing_messages`** — body without `messages` field
   → proxy forwards unchanged
5. **`test_empty_conversation`** — first request with empty `messages: []` →
   no transcript created, forwarded as-is

## Test helpers

Create shared helpers for:
- `build_test_proxy(enabled: bool, threshold: usize) -> (Router, Arc<MockBackend>)`
- `send_chat_request(proxy: &Router, flow_id: &str, messages: Vec<Value>) -> Response`
- `wait_for_compression(proxy: &Router, flow_id: &str, timeout: Duration)` —
  polls admin API until compressed_segments > 0
- `recorded_requests(backend: &MockBackend) -> Vec<RecordedRequest>` —
  returns all requests received by the mock backend

## Verification

```bash
cargo test --test context_compression 2>&1 | tail -20
cargo test --test context_reconcile 2>&1 | tail -20
```
