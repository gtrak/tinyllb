# Issue 07 — Body Rewriting in proxy_handler

## Objective

Integrate context compression into the main request proxy path. After flow
ID resolution and before forwarding to vLLM, intercept the `messages` array,
reconcile it against the stored transcript, and substitute the proxy's
`[Head + Compressed + Live]` version. If compression is needed, enqueue a
background job.

This mirrors the existing `inject_include_usage` pattern: parse JSON body,
modify the `messages` field, serialize back to `Bytes`.

## Files

| File | Change |
|------|--------|
| `src/gateway/proxy.rs` | Add compression step in `proxy_handler`, add `rewrite_messages()` helper |

## Prerequisites

- Issue 05 (reconciliation — `reconcile()` + `ReconcileResult`)
- Issue 06 (context state + `AppState` integration)

## Steps

1. **Insertion point** — in `proxy_handler()`, after line 253 (flow ID
   resolution) and before line 271 (header/body forwarding setup):

   ```rust
   // --- Context compression (issues 06/07) ---
   let body_bytes = if let Some(ref ctx) = state.context {
       match rewrite_messages(ctx, &body_bytes, &flow_id).await {
           Ok(new_body) => new_body,
           Err(e) => {
               // Fail open: log and forward original body uncompressed.
               tracing::warn!(
                   flow_id = %flow_id,
                   error = %e,
                   "context compression failed, forwarding original body"
               );
               body_bytes
           }
       }
   } else {
       body_bytes
   };
   ```

2. **`rewrite_messages()` function**:
   ```rust
   async fn rewrite_messages(
       ctx: &ContextState,
       body: &Bytes,
       flow_id: &FlowId,
   ) -> Result<Bytes, anyhow::Error>
   ```

   a. **Skip internal requests**: check for header
      `X-LLM-Internal: compressor`. If present, return body unchanged.
      (This prevents the compression sidecar request from being compressed
      itself — infinite recursion.)

   b. **Parse body JSON**: `serde_json::from_slice::<serde_json::Value>(body)`
      - If not JSON or no `messages` field: return body unchanged
      (e.g., `/v1/models` or `/v1/completions` without messages)

   c. **Extract messages array**: `value["messages"].as_array()`
      - If missing or not an array: return body unchanged

   d. **Reconcile under flow lock**:
      ```rust
      let result = ctx.with_flow_lock(&flow_id.to_string(), async {
          reconcile(flow_id, &messages, ctx.store.as_ref(),
                    ctx.estimator.as_ref(), &ctx.config).await
      }).await?;
      ```

   e. **Rewrite messages in the JSON body**:
      ```rust
      value["messages"] = serde_json::Value::Array(result.forwarded_messages);
      ```
      - Only modify `messages` — leave all other fields (`temperature`,
        `max_tokens`, `stream`, `tools`, etc.) untouched.

   f. **Serialize back**: `serde_json::to_vec(&value)?` → `Bytes`

   g. **Trigger compression if needed**:
      ```rust
      if result.needs_compression {
          if let Some((start, end)) = result.compress_turn_range {
              let job = CompressionJob {
                  flow_id: flow_id.to_string(),
                  turn_range_start: start,
                  turn_range_end: end,
                  enqueued_at: Instant::now(),
              };
              let _ = ctx.trigger_compression(job);
          }
      }
      ```

   h. Return the new body bytes.

3. **Compose with `inject_include_usage`**:
   The existing `inject_include_usage` runs AFTER `rewrite_messages`:
   ```rust
   // body_bytes is now the compressed version (or original if skipped)
   if let Some(injected) = inject_include_usage(&body_bytes) {
       headers.remove(CONTENT_LENGTH);
       builder = builder.body(injected);
   } else {
       builder = builder.body(body_bytes);
   }
   ```
   This is already the existing logic — no change needed. The variable
   `body_bytes` now points to the (possibly rewritten) body, and
   `inject_include_usage` operates on it normally.

4. **Skip non-chat routes**: `rewrite_messages` should only run for
   `/v1/chat/completions`. For `/v1/completions` (text completions, no
   messages array) or `/v1/models` (GET), skip. The function handles this
   internally (no `messages` field → return unchanged), but adding an
   explicit path check in `proxy_handler` avoids unnecessary JSON parsing.

5. **Header passthrough for internal requests**: when the compression worker
   (issue 09) makes a sidecar request, it includes `X-LLM-Internal: compressor`.
   This request flows through `proxy_handler`. The `rewrite_messages`
   function detects this header and returns the body unchanged. The header
   is stripped before forwarding to vLLM (filter it in `filter_headers` or
   in `rewrite_messages`).

6. **Logging**: add a tracing span for the compression step:
   ```rust
   tracing::debug!(
       flow_id = %flow_id,
       forwarded_tokens = result.total_est_tokens,
       raw_tokens = result.total_raw_est_tokens,
       needs_compression = result.needs_compression,
       transcript_reset = result.transcript_reset,
       "context compression applied"
   );
   ```

7. **Fail-open guarantees**: every error path in `rewrite_messages` returns
   the original body. The proxy never rejects a request due to compression
   failure. The only effect of a compression failure is that the original
   (uncompressed) messages are forwarded — vLLM may reject if over context
   limit, but that's vLLM's problem, not the proxy's.

## Tests

- `test_rewrite_substitutes_messages` — stored transcript has compressed
  segments → forwarded body contains summary messages, not original
- `test_rewrite_skips_internal_requests` — body with
  `X-LLM-Internal: compressor` header → body unchanged
- `test_rewrite_no_messages_field` — body without `messages` → unchanged
- `test_rewrite_fail_open` — store error → original body forwarded
- `test_rewrite_preserves_other_fields` — `temperature`, `max_tokens`,
  `tools` preserved after rewrite
- `test_rewrite_composes_with_include_usage` — streaming request → both
  message rewriting and `stream_options.include_usage` applied

## Verification

```bash
cargo test --lib proxy_handler 2>&1 | tail -10
cargo test --lib test_rewrite 2>&1 | tail -10
```
