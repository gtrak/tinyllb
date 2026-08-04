# Plan 005 — Premature-Stop Retry for Chat Completions

## Why

The quantized Qwen3.6-27B PrismaAURA model occasionally emits `<|im_end|>`
(token 248046, an EOS token) mid-reasoning. vLLM's `check_stop`
(`vllm/v1/engine/scheduler.py`) hard-stops on any `eos_token_id`, so the
turn ends immediately with `finish_reason: "stop"`, `reasoning_content`
present, but **empty `content` and no `tool_calls`** — a degenerate turn
that kills the agentic thread (the agent sees an empty assistant message
and cannot continue).

Root cause is weight-quant logit degradation: `<|im_end|>` ranks as
argmax inside the think block. This is not an MTP artifact — disabling
MTP only delays the EOS by 1-3 tokens (`~/opt/vllm/WORKLOG.md`
2026-07-27). The qwen3 reasoning parser is post-hoc and cannot prevent
the stop.

A vLLM-side fix was attempted: `ReasoningEosSuppressor` logits processor
(`~/dev/vllm` commit `ab2afe9f1e`, reverted by `1aa80443a2`) masked all
stop-token logits to `-inf` during thinking blocks. It worked but broke
MTP spec-decode interactions and was reverted. The fix belongs in the
proxy layer, which sees the completed response and can retry without
touching vLLM's sampler — so MTP stays unaffected.

## What

### Detection: "premature stop"

A `/v1/chat/completions` response is **premature** iff all of:

1. `finish_reason == "stop"` (excludes `length`, `tool_calls`, `error`,
   `abort` — those are legitimate or unrelated signals).
2. The assistant turn produced **no `content`** (absent, null, or empty
   string).
3. The assistant turn produced **no `tool_calls`** (absent or empty
   array).

A legitimate Qwen3 reasoning turn always ends with either content (a
final answer) or a tool call. A turn that ends with `finish_reason:
"stop"` and neither is the degenerate im_end case — the agent thread
dies. This predicate is model-agnostic, requires no token-id knowledge,
and works for both streaming and non-streaming responses.

### Retry: bumped temperature

On premature stop, the proxy re-sends the **exact forwarded body** (the
body already sent to the backend, post-`rewrite_messages` +
post-`include_usage` injection) with `temperature` bumped:

```
retry_temperature = min(base + attempt * temperature_step, max_temperature)
```

where `base` = the request body's `temperature` field (or
`default_temperature` if absent). The bumped temperature helps the model
escape the degenerate logits state that produced the premature im_end.

The retry body is the same in every other respect — same messages, same
`stream` mode, same `stream_options.include_usage`. Only `temperature`
changes.

### Streaming: forward-live, retry-appends

Coding agents (Claude Code, opencode) use `stream: true`. The proxy
forwards reasoning deltas live as they arrive. When the terminal frame
(the SSE chunk carrying `finish_reason`) arrives:

- **Good turn** (saw content or tool_calls) → forward the terminal frame
  + usage frame + `[DONE]` normally. Stream ends.
- **Premature + retries remain** → **discard** the terminal frame, drop
  the inner backend stream, issue a retry request, and continue
  forwarding the retry's deltas as a seamless continuation. The client
  sees: `[reasoning attempt 1] [reasoning attempt 2] [content]` — the
  failed reasoning shows as extra "thinking" but the thread survives.
- **Premature + retries exhausted** → **fail-open**: forward the
  terminal frame + usage + `[DONE]` from the last attempt. The client
  gets a degenerate turn, but the stream terminates cleanly.

Key protocol details:

- The failed attempt's terminal frames (`finish_reason`, `usage`,
  `[DONE]`) are **never forwarded** — they are discarded when the stream
  is dropped for retry. Only the accepted (or exhausted) attempt's
  terminal frames reach the client.
- Each vLLM response has a unique SSE `id` (e.g. `chatcmpl-xxx`). Retry
  attempts carry a different `id`. Claude Code / opencode key on the
  conversation, not the completion `id`, so this is cosmetic. Id
  normalization is noted as future work.
- Token accounting: only the **accepted** attempt's `usage` chunk counts
  toward `tokens_generated_total` and lifecycle delivered tokens.
  Failed-attempt reasoning tokens are real GPU work but are not charged
  to the client's credit.

### Non-streaming: collect, classify, retry

The non-streaming path collects the full JSON body before responding to
the client. After collection:

1. Parse JSON; check `is_premature_stop`.
2. If premature and retries remain → re-send to backend with bumped
   temperature. Collect the retry response body.
3. Repeat up to `max_retries`.
4. Forward the final body (good or last degenerate) to the client.

On retry HTTP failure (network error, non-200): fail-open with the last
successful body.

### Retry request mechanics

- **Reuses the exact forwarded body** — does NOT re-run
  `rewrite_messages`. This avoids double-reconciling the compression
  transcript (which would double-append turns via the
  longest-common-prefix diff).
- **Bypasses the scheduler** — the client request's admission slot
  (`QueueTicket`) is already held for the stream's entire lifetime. The
  retry is a direct HTTP call to the backend using the same `client`,
  `backend_url`, and filtered headers. No re-admission, no double slot
  consumption.
- **Skips internal compressor requests** — requests with
  `X-LLM-Internal: compressor` are excluded (the compression sidecar
  has its own retry via `compression_retries`).

### Configuration

New `retry_policy` section in `config.yaml`, mirroring the
`context_policy` convention (opt-in, `enabled: false` by default):

```yaml
retry_policy:
  enabled: false                  # set true to activate premature-stop retry
  max_retries: 2                  # retry attempts after the initial (total = 3)
  temperature_step: 0.3           # temperature added per retry attempt
  max_temperature: 1.5            # cap on bumped temperature
  default_temperature: 0.0        # base temperature when request omits it
```

Environment overrides: `TINYLLB__RETRY_POLICY__ENABLED=true`, etc.

Validation (only when `enabled: true`):
- `max_retries > 0`
- `temperature_step > 0.0`
- `max_temperature >= default_temperature`
- `max_temperature <= 2.0` (OpenAI-compatible range)

### Metrics

New Prometheus counters in the `tinyllb_*` family:

| Metric | Type | Description |
|--------|------|-------------|
| `tinyllb_premature_stop_detected_total` | counter | Premature stops detected (one per failed attempt) |
| `tinyllb_premature_stop_retries_total` | counter | Retry requests issued |
| `tinyllb_premature_stop_exhausted_total` | counter | Degenerate turns forwarded after all retries exhausted |

## Scope

### In scope

- `src/config/mod.rs` — `RetryPolicy` struct + `Default` impl
- `src/config/loader.rs` — defaults, env overrides, validation
- `src/gateway/retry.rs` — **new module**: `is_premature_stop()`,
  `bump_temperature()`, SSE frame parser, streaming retry state machine
- `src/gateway/proxy.rs` — non-streaming retry loop; streaming retry
  wiring; capture `forwarded_body` for reuse; gate on route + internal
  header
- `src/gateway/stream.rs` — streaming retry stream (channel-based
  spawned task) or equivalent; token accounting for accepted attempt
  only
- `src/gateway/mod.rs` — add `retry_policy: RetryPolicy` to `AppState`
- `src/metrics/mod.rs` — three new counters
- `src/main.rs` — wire `cfg.retry_policy` into `AppState`
- `config.example.yaml` — `retry_policy` block (commented, disabled)
- `README.md` — config table rows + short section
- `lat.md/config.md` — `retry_policy` documentation
- `tests/premature_stop_retry.rs` — **new**: integration tests
- All existing test files that construct `AppState` (~20 sites) — add
  `retry_policy: RetryPolicy::default()` field

### Out of scope

- vLLM-side logits processing (the reverted `ReasoningEosSuppressor`
  approach)
- Retry for `/v1/completions` (non-chat; no reasoning/tool-call parsing)
- Retry for `/v1/models` (GET, no generation)
- SSE `id` normalization across retry attempts (cosmetic; future work)
- Persisting retry state across proxy restarts
- Retry with request-body modifications other than temperature (e.g.,
  appending a "continue" system message)
- Quality eval for whether retries produce semantically equivalent
  output (the retry may produce different reasoning/answer — that's
  acceptable; the goal is thread survival, not determinism)

## Success criteria

- [ ] A `/v1/chat/completions` response with `finish_reason: "stop"`,
      empty content, and no tool_calls triggers a retry (up to
      `max_retries`) when `retry_policy.enabled: true`
- [ ] A response with non-empty content or tool_calls does NOT trigger a
      retry
- [ ] `finish_reason: "length"` or `finish_reason: "tool_calls"` does
      NOT trigger a retry
- [ ] Streaming: the client receives a concatenated stream (failed
      reasoning + good response) with a single terminal frame + `[DONE]`
- [ ] Non-streaming: the client receives the good response body after
      retry
- [ ] Fail-open: after all retries exhausted, the last degenerate
      response is forwarded (stream terminates cleanly)
- [ ] Disabled (`enabled: false`): zero behavioral change — no retries,
      no metrics, passthrough identical to today
- [ ] Internal compressor requests (`X-LLM-Internal: compressor`) are
      skipped
- [ ] Only `/v1/chat/completions` is affected
- [ ] Temperature is bumped correctly: `min(base + n*step, max)`,
      using the request's temperature or `default_temperature`
- [ ] Retry reuses the exact forwarded body (no re-reconciliation of
      compression transcript)
- [ ] Retry bypasses the scheduler (no re-admission, no double slot)
- [ ] Metrics exposed: `premature_stop_detected_total`,
      `premature_stop_retries_total`, `premature_stop_exhausted_total`
- [ ] `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass
- [ ] All ~20 `AppState` construction sites updated with
      `retry_policy` field

## Implementation detail

### `src/gateway/retry.rs` — new module

**`is_premature_stop(body: &serde_json::Value) -> bool`**

Shared by both paths. Parses the non-streaming JSON response body:

```text
choices[0].finish_reason == "stop"
  AND (choices[0].message.content is absent, null, or empty string)
  AND (choices[0].message.tool_calls is absent or empty array)
```

**`bump_temperature(body: &serde_json::Value, attempt: u32, policy: &RetryPolicy) -> serde_json::Value`**

Clones the body, sets `temperature` to
`min(base + attempt * step, max_temperature)` where `base` = body's
`temperature` (or `default_temperature` if absent). Returns the modified
body.

**`SseFrameParser`** — accumulates raw bytes, splits on `\n\n` (SSE
event delimiter), returns complete frame byte-slices. Keeps the
incomplete tail in an internal buffer. Used by the streaming retry path
to inspect each frame before forwarding.

**`classify_frame(frame: &[u8]) -> FrameClassification`** — best-effort
parses a single SSE `data:` line as JSON. Returns:

```rust
struct FrameClassification {
    has_content: bool,      // delta.content is non-null and non-empty
    has_tool_calls: bool,   // delta.tool_calls is non-null and non-empty
    finish_reason: Option<String>,
    is_done: bool,          // literal "data: [DONE]"
    has_usage: bool,        // usage object present
}
```

Frames that fail JSON parse are classified as "pass-through" (all
fields false) — they are forwarded without inspection.

### `src/gateway/proxy.rs` — non-streaming retry

After `collect_response_body` and before constructing the client
response, when `retry_policy.enabled && is_chat_completions(path) &&
!is_internal_compressor(headers)`:

```text
attempts = 0
loop {
    if !is_premature_stop(&body) || attempts >= max_retries {
        break
    }
    metrics.premature_stop_detected_total.inc()
    metrics.premature_stop_retries_total.inc()
    attempts += 1
    retry_body = bump_temperature(&forwarded_body, attempts, &policy)
    match client.post(backend_url).headers(filtered_headers).body(retry_body).send().await {
        Ok(resp) if resp.status().is_success() => {
            body = resp.bytes().await?  // replace body
        }
        _ => break  // fail-open: use last successful body
    }
}
if attempts >= max_retries && is_premature_stop(&body) {
    metrics.premature_stop_exhausted_total.inc()
}
// forward body to client (existing path)
```

`forwarded_body` is captured before the first send: it's the body after
`rewrite_messages` + `inject_include_usage`, cloned and held for retry
reuse.

### `src/gateway/stream.rs` — streaming retry

When `retry_policy.enabled && is_chat_completions && !is_internal_compressor`:
use a **channel-based spawned task** instead of `MetricStream`.

The spawned task owns the `QueueTicket` and `LifecycleGuard` (held for
the entire retry loop, not per-attempt). It:

1. Opens the first attempt's `bytes_stream()`.
2. Feeds chunks through `SseFrameParser`.
3. For each complete frame, calls `classify_frame`:
   - Non-terminal, non-`[DONE]` → forward raw frame bytes to the channel
     (client sees live reasoning/content). Update `saw_content` /
     `saw_tool_calls` flags.
   - Terminal (`finish_reason` present):
     - If premature (`finish_reason == "stop"` && `!saw_content` &&
       `!saw_tool_calls` && `attempt < max_retries`) → increment metrics,
       **discard** the frame, **drop** the inner stream, issue retry
       request, swap inner stream to retry response, reset flags,
       `attempt += 1`. Continue.
     - Else → forward the frame + continue forwarding remaining frames
       (usage, `[DONE]`). Extract `completion_tokens` from the usage
       frame for metrics + lifecycle accounting. Mark done.
4. When done → close the channel. The spawned task completes, dropping
   the ticket + lifecycle guard (which emits `request_completed`).
5. If the channel receiver is dropped (client disconnected) →
   `tx.send()` fails, the task aborts (lifecycle guard drops as
   cancelled).

The client-facing response body is `Body::from_stream(ReceiverStream)`.
Response headers are set as today (status, filtered headers,
`X-Request-ID`).

When `retry_policy.enabled` is `false`: the existing `MetricStream` path
is used unchanged — zero regression risk.

### `src/gateway/mod.rs` — AppState

Add `retry_policy: RetryPolicy` to `AppState`. All ~20 construction sites
(main.rs + test files) are updated with `retry_policy:
RetryPolicy::default()` (or `cfg.retry_policy.clone()` in main.rs).

### `src/config/mod.rs` — RetryPolicy

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
    #[serde(default = "RetryPolicy::default_enabled")]
    pub enabled: bool,                    // false
    #[serde(default = "RetryPolicy::default_max_retries")]
    pub max_retries: u32,                 // 2
    #[serde(default = "RetryPolicy::default_temperature_step")]
    pub temperature_step: f64,            // 0.3
    #[serde(default = "RetryPolicy::default_max_temperature")]
    pub max_temperature: f64,             // 1.5
    #[serde(default = "RetryPolicy::default_default_temperature")]
    pub default_temperature: f64,         // 0.0
}
```

Added to `Config` as `#[serde(default)] pub retry_policy: RetryPolicy`.

## Task order

```
01 (config: RetryPolicy + loader defaults + validation)
 → 02 (retry.rs: is_premature_stop + bump_temperature + SseFrameParser
       + classify_frame + unit tests)
 → 03 (AppState field + update all ~20 construction sites)
 → 04 (non-streaming retry loop in proxy.rs + integration tests)
 → 05 (streaming retry in stream.rs + integration tests)
 → 06 (metrics: 3 counters + wire into both paths)
 → 07 (docs: config.example.yaml, README, lat.md/config.md)
```

- 01 → 02 (retry.rs needs the policy type)
- 03 is mechanical but touches many files — do it early so everything
  compiles before wiring logic
- 04 and 05 are independent paths but share retry.rs helpers
- 06 can run after 04+05 (metrics need the call sites)
- 07 is last (docs reflect final behavior)

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| False positive: a legitimate response has empty content + no tool calls + `finish_reason: stop`. | For Qwen3 reasoning turns, this is always degenerate (the model should produce content or a tool call). Gated behind `enabled: false` by default. Operator can disable if it causes issues. |
| Retry burns 2-3x GPU time for a failed long-reasoning turn. | Bounded by `max_retries` (default 2). The failure is "occasional" per the WORKLOG. The cost of NOT retrying is a dead agentic thread (much worse). |
| Streaming client receives multiple reasoning blocks (extra "thinking"). | Accepted by user decision. The thread survives, which is the priority. The extra reasoning is displayed as additional thinking. |
| SSE `id` changes between attempts. | Cosmetic — Claude Code / opencode key on the conversation, not the completion `id`. Id normalization noted as future work. |
| Retry HTTP call fails (network error, backend 5xx). | Non-streaming: fail-open with last successful body. Streaming: end the stream (client has reasoning but no content). Increment `premature_stop_exhausted_total`. |
| Client disconnects mid-retry. | Channel sender fails → spawned task aborts → lifecycle guard drops as cancelled (same semantics as today's `MetricStream` on disconnect). |
| AppState field addition breaks ~20 test construction sites. | Mechanical update with `retry_policy: RetryPolicy::default()`. `cargo build --all-targets` catches all sites. Do this in task 03 before wiring logic. |
| Context compression double-reconciles on retry. | Retry reuses the exact `forwarded_body` (post-rewrite). Does NOT re-run `rewrite_messages`. The compression transcript is not re-diffed. |
| Scheduler double-admits on retry. | Retry bypasses the scheduler entirely — direct HTTP call to backend. The `QueueTicket` is held by the spawned task for the whole loop. |

## Future work (not in this plan)

- SSE `id` normalization: rewrite the `id` field in retry frames to
  match the first attempt's `id`, for strict client compatibility.
- Retry with request-body modifications beyond temperature (e.g.,
  appending a "continue your reasoning" system message, or setting
  `repetition_penalty`).
- Configurable detection predicate: e.g., only retry when
  `reasoning_content` is non-empty (to distinguish "model gave up" from
  "model produced nothing at all").
- Per-flow retry budget: track retry rate per flow and back off if a
  flow consistently produces premature stops (indicates a pathological
  prompt rather than occasional quant noise).
- Quality eval: measure whether retried responses are semantically
  equivalent to what a non-degenerate turn would have produced.
- Retry for `/v1/completions` if reasoning parsing is ever extended to
  that route.
