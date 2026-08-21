# 05 — Gateway: mid-stream error re-forward

- **Complexity:** S
- **Timebox:** 50 min
- **Depends on:** 02 (config), 03 (counters), 04 (classify helper)

## Objective

Handle the rarer transient case: a mid-stream SSE error event from
llama.cpp (unified-KV exhaustion while all slots are busy) when **no content
frames have been forwarded to the client yet**. Re-forward the original
request with bounded backoff. If content was already forwarded, keep the
current abort-body behavior (re-forwarding would duplicate tokens — the
client's retry remains the recovery).

## Files

| File | Change |
|------|--------|
| `src/gateway/stream.rs` | In `spawn_retry_stream` (the channel-based spawned task from plan 005), detect a backend SSE error frame that is a llama.cpp transient error before any content/tool_calls frame was forwarded, and re-send via the channel's retry path. Re-uses `classify_llamacpp_error` from `src/gateway/retry.rs` (task 04). |
| `src/gateway/retry.rs` | Possibly expose a small helper to classify an SSE error frame's `data:` JSON body — or re-use `classify_llamacpp_error` on the parsed frame bytes. |

## Context (verified facts — do not re-derive)

- `src/gateway/stream.rs` already has a channel-based spawned task
  (`spawn_retry_stream`, plan 005) that owns the `QueueTicket` +
  `LifecycleGuard`, feeds backend chunks through `SseFrameParser`, calls
  `classify_frame`, and handles the premature-stop retry by re-sending via
  `send_retry_request`. Read that implementation before changing anything —
  the transient re-forward slots into the same retry state machine.
- llama.cpp sends mid-stream errors as an SSE error event (the server
  formats the error JSON into the stream — see
  `server-context.cpp:4406` "error received during streaming, terminating
  stream"). The `classify_frame` already extracts fields from `data:` JSON;
  an error frame carries `error.type == "exceed_context_size_error"` (or
  another transient type). The stream then terminates (no `[DONE]`).
- Today, a stream that "ends without a terminal frame is treated as a
  transport failure" (transport-retry behavior) — the body is aborted so
  the client retries on fresh connections. That path already handles the
  "content was forwarded" case acceptably. This task adds the *better*
  path for the no-content-yet case: proxy-side re-forward, which is
  transparent to the client.

## Steps

1. Read `src/gateway/stream.rs` fully — locate `spawn_retry_stream` and
   the per-frame `classify_frame` loop. Note where `saw_content` /
   `saw_tool_calls` flags are tracked (the premature-stop path uses them).
2. Add detection of an SSE error frame: when a frame's parsed JSON has an
   `error` object, classify it with `classify_llamacpp_error` (applied to
   the frame's `data:` payload bytes — the helper from task 04 already
   takes `&[u8]` and tolerates non-error JSON by returning
   `NotLlamacpp`).
3. Decision logic, integrated into the existing retry loop:
   - **Transient error frame AND `!saw_content && !saw_tool_calls` AND
     attempts remain** → discard the error frame, drop the inner backend
     stream, increment `backend_retries_total`, sleep backoff, re-send via
     `send_retry_request` (same body — no temperature bump), swap the inner
     stream to the retry response, reset the per-attempt frame flags,
     `attempt += 1`. Continue. (This mirrors the premature-stop retry's
     discard-and-retry exactly.)
   - **Transient error frame AND content/tool_calls already forwarded** →
     do NOT re-forward (would duplicate). Fall through to the existing
     abort-body / transport-failure behavior (end the stream so the client
     retries). Increment `backend_retry_exhausted_total` since the proxy
     could not recover transparently.
   - **Permanent error frame (or non-llama.cpp error)** → forward the
     error frame to the client (or end the stream per current behavior).
     Do not retry.
   - **Transient + attempts exhausted** → forward the error frame and end
     the stream; increment `backend_retry_exhausted_total`.
4. Reuse the existing `attempt` counter and `max_attempts` gating — but
   note the premature-stop retry and the transient retry share the same
   retry budget? **Decision:** they are independent budgets
   (`retry_policy.max_retries` for premature-stop,
   `transient_retry.max_attempts` for transient errors). Track a separate
   `transient_attempt` counter, OR a combined budget — keep it simple and
   correct: separate counters, each bounded by its own policy. Document
   the choice inline.
5. The `QueueTicket`/`LifecycleGuard` stay held across both retry kinds
   (they're owned by the spawned task for the stream's whole lifetime),
   so re-forward needs no re-admission — exactly like premature-stop
   retry.

## Tests

- Add to `tests/transient_retry.rs` (created in task 04) or a focused
  `tests/transient_retry_stream.rs`:
  - **Mid-stream transient, no content yet → re-forward → success:** stub
    streams one error frame (transient) before any content, then a full
    normal SSE stream on retry. Assert the client receives a clean,
    content-bearing stream with a single terminal `[DONE]`, and
    `backend_retries_total == 1`.
  - **Mid-stream transient after content → abort (no duplication):** stub
    streams content deltas, then a transient error frame. Assert the
    stream terminates (client gets the partial content and must retry),
    and `backend_retry_exhausted_total == 1`, `backend_retries_total ==
    0` (no re-forward attempted because content was already sent).
  - **Mid-stream permanent → forward error, no retry:** stub streams a
    permanent `exceed_context_size_error` frame. Assert no retry, the
    error reaches the client.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
All existing streaming/premature-stop tests must pass unchanged — the
transient path only adds a new trigger to the existing retry state
machine; with `transient_retry.max_attempts == 0` (the disabled case) the
behavior is byte-identical to today.
