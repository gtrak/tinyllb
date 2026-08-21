# 04 — Gateway: intake-error / network-error transient re-forward

- **Complexity:** S
- **Timebox:** 60 min
- **Depends on:** 02 (config), 03 (counters)

## Objective

Re-forward the original request body with bounded exponential backoff for
**transient** failures of two kinds, both of which occur before any client
bytes are sent (so re-forwarding is safe):

1. **llama.cpp intake context-exceed** — HTTP 400 with
   `type == "exceed_context_size_error"` and `n_prompt_tokens < n_ctx`
   (transient; `>= n_ctx` is permanent and passes through unchanged).
2. **Network errors** — "connection reset by server", connection refused,
   broken pipe, etc. observed when llama.cpp is restarted (or briefly
   unreachable) under live traffic. Today these become `ProxyError::Network`
   → 502 Bad Gateway. With transient retry, the proxy waits out the backend
   restart and re-forwards, returning 502 only after the budget is
   exhausted.

This covers both streaming and non-streaming intake errors because they
both arrive as an initial non-2xx status (or a send error) in the same code
path.

## Files

| File | Change |
|------|--------|
| `src/gateway/proxy.rs` | In the error block (~:504-522, the `if status.is_client_error() || status.is_server_error()` branch) AND the send-error path (~:470-487, the `Err(...)` arms of the initial `send`), add transient detection + bounded re-forward before the verbatim-return / 502. On a successful retry (2xx), re-dispatch the retry response through the normal streaming/non-streaming paths instead of returning. |
| `src/gateway/retry.rs` | Add a `classify_llamacpp_error(body: &[u8]) -> LlamacppErrorClass` helper + a small `LlamacppErrorClass` enum (`Permanent`, `Transient`, `NotLlamacpp`) used by 04 and 05. Also add a `is_transient_network_error(&reqwest::Error) -> bool` predicate (connection reset / connection refused / broken pipe / connect errors) used by 04's send-error path. Reuses nothing else. |

## Context (verified facts — do not re-derive)

- `src/gateway/proxy.rs:504-522` is the error block: it collects the body
  (`collect_response_body`), marks lifecycle completed, and returns the
  response verbatim with filtered headers. This runs for *both* streaming
  and non-streaming requests because it precedes the `is_sse ||
  wants_streaming` dispatch at `:529`.
- llama.cpp intake context-exceed returns HTTP 400 JSON:
  `{"error":{"code":400,"type":"exceed_context_size_error","message":"...",
  "n_prompt_tokens":N,"n_ctx":M}}` (source: `server-common.cpp:51`,
  `server-context.cpp:3137`, `server-task.cpp:1505`). The `type` field is
  the reliable discriminator — do **not** match on the message string.
  `n_prompt_tokens` and `n_ctx` are present only for this error type.
- The error is delivered **before any SSE bytes** (the server waits for the
  first task result before committing to a 200 stream —
  `server-context.cpp:4307`), so re-forwarding is safe: the request body is
  untouched and no client bytes have been sent.
- The request body is already buffered as `forwarded_body: Bytes` earlier
  in the handler. The premature-stop retry loop re-sends via
  `send_retry_request(&state.client, &method, &backend_url, &headers,
  body_bytes, state.request_timeout)` (see `proxy.rs:643`). Re-use the same
  helper for the transient re-forward — the body is sent unchanged (no
  temperature bump).
- `state` is `AppState`; `transient_retry` is reached via the `Backend`
  config. Decide the access path during implementation: either add
  `transient_retry: TransientRetry` directly to `AppState` (cleaner, mirrors
  `retry_policy`) and wire it in `main.rs` + `AppState::test_default`, or
  thread it from `state` via a new field. Prefer adding it to `AppState`
  for symmetry with `retry_policy`; update `src/gateway/mod.rs`
  `test_default` and `src/main.rs` construction (the
  `retry_policy` precedent shows exactly which sites to touch).

## Steps

1. In `src/gateway/retry.rs`, add:
   ```rust
   pub enum LlamacppErrorClass {
       NotLlamacpp,
       Permanent,   // n_prompt_tokens >= n_ctx
       Transient,   // n_prompt_tokens < n_ctx
   }
   pub fn classify_llamacpp_error(body: &[u8]) -> LlamacppErrorClass {
       // Parse JSON; navigate error.type; if == "exceed_context_size_error"
       // and both n_prompt_tokens and n_ctx are present ints, classify;
       // else NotLlamacpp. Tolerate malformed JSON → NotLlamacpp.
   }
   pub fn is_transient_network_error(e: &reqwest::Error) -> bool {
       // true for: connection reset / connection refused / broken pipe /
       // connect-timeout / hyper connection errors. Use reqwest's
       // is_connect(), is_timeout() (connect-phase), and the underlying
       // io::ErrorKind where reachable. Be conservative: do NOT retry
       // request-build errors or body-parse errors.
   }
   ```
   Unit-test all branches.
2. Add `transient_retry: TransientRetry` to `AppState` (`src/gateway/mod.rs`),
   wire it in `src/main.rs` (`transient_retry: cfg.backend.transient_retry.clone()`)
   and `AppState::test_default` (`transient_retry:
   TransientRetry::default()`).
3. In `src/gateway/proxy.rs`, handle BOTH transient failure kinds in one
   bounded re-forward loop. Two entry points feed the same loop:
   - **Send-error path** (~:470-487): today the `Err(ProxyError::Network(e))`
     arm returns 502 immediately. When `is_transient_network_error(&e)` and
     `state.transient_retry.max_attempts > 0`, enter the re-forward loop
     instead of returning.
   - **Error-block path** (~:504-522): compute
     `class = classify_llamacpp_error(&body_bytes)` after collecting the
     error body. `Permanent` → verbatim return (no retry). `Transient` and
     `max_attempts > 0` → enter the re-forward loop. `NotLlamacpp` →
     verbatim return (existing behavior; vLLM-shaped 4xx untouched).
   The shared re-forward loop:
   ```rust
   for attempt in 1..=state.transient_retry.max_attempts {
       state.metrics.backend_retries_total.inc();
       sleep(backoff(attempt, &state.transient_retry)).await;
       let send = send_retry_request(&state.client, &method,
           &backend_url, &headers, forwarded_body.clone(),
           state.request_timeout).await;
       match send {
           Ok(resp) if resp.status().is_success() => {
               // Re-dispatch this response: re-read status/headers/
               // content_type/is_sse and fall through to the normal
               // streaming/non-streaming dispatch below. Easiest:
               // assign `response = resp`, re-derive `status`,
               // `response_headers`, `content_type`, `is_sse`, and
               // `break` out of the retry loop into the existing
               // dispatch (529+ streaming, 584+ non-streaming).
               // DO NOT return from inside the loop on success.
           }
           Ok(resp) => {
               // Another error (maybe 400 again). Reclassify; if
               // Permanent, break with this response as the final
               // verbatim error; if still Transient and attempts
               // remain, continue; if exhausted, break.
               ...
           }
           Err(e) if is_transient_network_error(&e) && attempts_remain => continue,
           Err(_) => break, // non-transient network error: keep the 502
       }
   }
   // If we exit the loop without a success, the last error response is
   // forwarded verbatim (or 502 for a send error) and
   // backend_retry_exhausted_total increments.
   ```
   - `backoff(attempt, policy)`: exponential,
     `min(backoff_start * 2^(attempt-1), backoff_max)`, capped.
   - **Critical structural note:** the error block currently `return`s. To
     re-dispatch a successful retry, the success path must NOT return — it
     must fall through to the streaming/non-streaming dispatch below. The
     cleanest implementation: pull the error block into a helper or restructure
     with a labeled block / early re-assignment of `response` so control
     reaches the `is_sse || wants_streaming` dispatch at `:529`. If a full
     refactor is too invasive, an acceptable alternative is to run the
     retry loop and on success *recursively* call into the dispatch via a
     small inner async helper — but prefer the fall-through restructure.
     Keep the premature-stop retry loop (`:607`) untouched; it runs on the
     final body after dispatch, and a transient-retried successful body is
     just a normal success body.
4. Lifecycle accounting: the `RequestActiveGuard` / `lifecycle` is held
   across the retry loop (the admission slot stays held, exactly like the
   premature-stop retry which "bypasses the scheduler"). Do **not**
   `lifecycle.mark_completed()` on the transient error attempt — only on
   the final response (success or permanent/exhausted error). Today the
   error block calls `lifecycle.mark_completed()` at `:512`; move that
   call so it only fires on the *final* verbatim-error return, not on a
   retried-then-success path (where the normal completion path at `:692`
   handles it).

## Tests

- `tests/transient_retry.rs` (new) using a stub backend (see
  `benches/stub_backend.rs` for the stub pattern, or the existing gateway
  test harness in `tests/gateway.rs`):
  - **Permanent passthrough:** stub returns 400
    `{"error":{"code":400,"type":"exceed_context_size_error",
    "n_prompt_tokens":300000,"n_ctx":262144,...}}`. Assert the client
    receives 400 with that body, and `backend_retries_total` is 0.
  - **Transient → success:** stub returns the transient 400 once, then 200
    with a normal chat-completion body. Assert the client receives the 200
    body, and `backend_retries_total == 1`.
  - **Transient exhaustion:** stub always returns the transient 400. Assert
    the client receives the 400 and
    `backend_retry_exhausted_total == 1` (and `backend_retries_total ==
    max_attempts`).
  - **Disabled:** `max_attempts: 0` → transient 400 passes through with
    `backend_retries_total == 0`.
  - **Network error (backend restart):** stub returns a connection-reset
    error on the first send, then a 200 on retry. Assert the client
    receives the 200 body and `backend_retries_total == 1`. (Use a stub
    that drops/returns an error for attempt 1.)
  - **Streaming transient:** a streaming request (`stream:true`) getting
    a transient intake 400, then a 200 SSE stream on retry. Assert the
    client receives the SSE stream and `backend_retries_total == 1`.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
All existing gateway tests must pass unchanged (the disabled-by-default
semantics and the `NotLlamacpp` classification for non-llama.cpp 4xx
errors mean vLLM-shaped 4xx passthrough is untouched — verify this in
the existing `tests/gateway.rs`).
