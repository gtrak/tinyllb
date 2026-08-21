# Plan 007 — llama.cpp Backend Support

## Why

A hardware change replaced vLLM with `llama-server` (llama.cpp) on `:8000`
behind tinyllb (`:1234`). The deployment is Qwen3.8-27B with `-kvu` (unified
KV cache), `--parallel 4`, `--cache-ram 24000`, `--metrics` (see
`~/opt/llama.cpp/start-server.sh`). tinyllb's backend monitor only parses
`vllm:*` metrics; llama.cpp exposes a different metric family with **no
KV-usage metric**, so today the monitor publishes nothing, the stall watchdog
never runs, and `is_busy()` is permanently false on this backend.

Worse, llama.cpp's context-full behavior differs fundamentally from vLLM:
it never pages out (preempts) other requests to make room. When the unified
KV cache is exhausted it errors in-flight requests rather than evicting
residents — so tinyllb must recover itself instead of relying on the backend
to page out.

## What

### 1. llama.cpp metrics support (backend monitor)

llama-server `--metrics` exposes (verified against the live `/metrics` scrape
and `~/dev/llama.cpp/tools/server/server-context.cpp`):

| llama.cpp metric | maps to snapshot field | vLLM equivalent |
|---|---|---|
| `llamacpp:requests_processing` | `requests_running` | `vllm:num_requests_running` |
| `llamacpp:requests_deferred` | `requests_waiting` | `vllm:num_requests_waiting` |
| `llamacpp:prompt_tokens_total` | `prompt_tokens` | `vllm:prompt_tokens_total` |
| `llamacpp:tokens_predicted_total` | `generation_tokens` | `vllm:generation_tokens_total` |
| `llamacpp:prompt_tokens_cached_total` | `cached_prompt_tokens` (new) | — (no vLLM analog) |
| `llamacpp:n_decode_total` | `decode_calls` (new) | — |

The parser accepts both metric families unconditionally into the same fields
(last-parsed-wins per field, matching the existing v0/v1 precedent). Each
`/metrics` scrape self-identifies its flavor by metric-name prefix — no
config flag, no startup probe, no restart on backend swap. The detected
flavor is logged when it changes.

**Publish gate change (correctness fix):** today the poll loop only publishes
a snapshot when `found_usage` is true. For llama.cpp `found_usage` is *never*
true (no KV metric), so nothing is ever published. The gate becomes
flavor-aware: publish if `found_usage` (vLLM) **or** `found_llamacpp`.

**Stall watchdog:** `is_busy()` derives from `requests_processing` +
`requests_deferred`; token progress derives from the counters. One wrinkle:
`llamacpp:prompt_tokens_total` **excludes cached tokens** and this workload
is cache-heavy (74k/75k-token cached prefixes seen on `/slots`). Without the
cached + decode-call counters as additional progress signals, the watchdog
could false-positive during pure cache restores. The watchdog's progress
check is extended to all four counters.

**KV-dependent features degrade gracefully:** `kv_usage` stays at its 0.0
default on llama.cpp (no metric), so `kv_policy` (admission gate) accepts
and `kv_bias` (selection bias) falls back to pure DRR fairness. The
`vllm_kv_cache_usage`/`vllm_kv_cache_free` gauges are only written when
`found_usage` is true (vLLM). This inertness is documented, not silently
broken. No preemption counter exists on llama.cpp; `preemptions` stays 0
(already documented as best-effort).

### 2. Transient-failure re-forward (gateway)

Two transient failure kinds, both handled by one bounded re-forward loop:

- **llama.cpp intake context-exceed** (`ERROR_TYPE_EXCEED_CONTEXT_SIZE`):
  fires when `prompt_tokens >= slot.n_ctx`. Returns HTTP 400 with structured
  JSON `{"error":{"code":400,"type":"exceed_context_size_error","message":"...",
  "n_prompt_tokens":N,"n_ctx":M}}`. Delivered **before any SSE bytes** (the
  server waits for the first task result before committing to a 200 stream),
  so it is safe to re-forward — the request body is untouched and no client
  bytes have been sent. Per the source this is **permanent** when
  `n_prompt_tokens >= n_ctx` (slot capacity is static; retrying cannot help)
  and **transient** when `n_prompt_tokens < n_ctx` (defensive — covers future
  llama.cpp behavior changes and unified-KV crowding).

- **Network errors (backend restart):** "connection reset by server",
  connection refused, broken pipe — observed when llama.cpp is restarted
  under live traffic. Today these become `ProxyError::Network` → 502. With
  transient retry, the proxy waits out the restart and re-forwards,
  returning 502 only after the budget is exhausted.

- **Mid-stream KV exhaustion / connection reset**: under `-kvu`, when decode
  can't find free KV space and all slots are busy (it only purges *idle*
  slots, never busy ones), in-flight requests error mid-stream via an SSE
  error event; a backend restart mid-generation produces a connection reset.
  If no content frames were forwarded yet, re-forwarding is safe; if content
  was already forwarded, re-forward would duplicate tokens, so the current
  abort-body + client-retry behavior is kept.

**Decisions (locked with user):**
- Permanent intake context-exceed (`n_prompt_tokens >= n_ctx`) → **pass the
  400 through unchanged** (docs only).
- Transient failures → **proxy-side re-forward** with bounded backoff,
  reusing the premature-stop retry body-buffering/re-send pattern
  (`send_retry_request` in `src/gateway/retry.rs`).
- Backend detection → **per-scrape self-identification** by metric-name
  prefix.

### 3. Configuration, metrics, tests, docs

- New `backend.transient_retry` config: `max_attempts` (default 3),
  `backoff_start` (500ms), `backoff_max` (4s). `max_attempts: 0` disables.
- New counters: `tinyllb_backend_retries_total`,
  `tinyllb_backend_retry_exhausted_total`.
- Unit tests with a real `llamacpp:` metric body (copied from the live
  scrape); gateway stub tests for permanent passthrough, transient
  re-forward, and retry exhaustion.
- README quickstart for llama.cpp; `lat.md` updates; `lat check`.

## Success criteria

- [ ] `parse_snapshot` on a real `llamacpp:` `/metrics` body populates
      `requests_running`, `requests_waiting`, `prompt_tokens`,
      `generation_tokens`, `cached_prompt_tokens`, `decode_calls`, and sets
      `found_llamacpp = true`; `kv_usage` stays 0.0, `found_usage` false.
- [ ] A vLLM-flavored scrape still sets `found_usage`/`found_free` and the
      existing vLLM unit tests pass unchanged.
- [ ] The poll loop publishes a snapshot (and runs the stall watchdog) on a
      llama.cpp scrape that lacks any KV metric — i.e. the `found_usage`
      gate no longer suppresses llama.cpp publishing.
- [ ] Stall watchdog treats frozen `prompt_tokens` + `generation_tokens` +
      `cached_prompt_tokens` + `decode_calls` under busy state as a stall.
- [ ] A permanent intake 400 (`exceed_context_size_error`,
      `n_prompt_tokens >= n_ctx`) is passed through to the client unchanged.
- [ ] A transient intake 400 (`n_prompt_tokens < n_ctx`) is re-forwarded
      with bounded backoff up to `max_attempts`; on success the (possibly
      streaming) response is dispatched normally.
- [ ] On retry exhaustion the last error response is forwarded and
      `tinyllb_backend_retry_exhausted_total` increments.
- [ ] `max_attempts: 0` disables transient retry — zero behavioral change.
- [ ] Mid-stream error with no content forwarded yet → re-forward; with
      content already forwarded → current abort-body behavior (no token
      duplication).
- [ ] `tinyllb_backend_retries_total` increments on each re-forward.
- [ ] `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass.
- [ ] `lat check` passes; `lat.md` reflects the dual-family parser, the
      publish-gate change, the new snapshot fields, and the transient
      re-forward concept.

## Scope

### In scope

- `src/backend/mod.rs` — llamacpp metric constants, dual-family parser,
  `cached_prompt_tokens`/`decode_calls` snapshot fields, `found_llamacpp`
  flag, flavor-aware publish gate, extended watchdog progress, flavor log.
- `src/config/mod.rs`, `src/config/loader.rs` — `TransientRetry` config +
  defaults/env overrides/validation; nested under `backend`.
- `src/metrics/mod.rs` — `tinyllb_backend_retries_total` +
  `tinyllb_backend_retry_exhausted_total` counters.
- `src/gateway/proxy.rs` — intake-error transient re-forward in the error
  block (covers both streaming and non-streaming intake 400s).
- `src/gateway/stream.rs` — mid-stream SSE-error re-forward when no content
  forwarded yet.
- `src/gateway/mod.rs` / `src/main.rs` — wire `transient_retry` into
  `AppState` + construction sites.
- `config.example.yaml` — `backend.transient_retry` block.
- `README.md` — llama.cpp quickstart + kv_policy inertness note.
- `lat.md/backend.md`, `lat.md/gateway.md` (or `admission.md`),
  `lat.md/metrics.md`, `lat.md/config.md` — updated contracts.
- `tests/` — backend llamacpp parsing unit tests; gateway stub tests.

### Out of scope

- Chat-history truncation / context compression (deleted 2026-08-19 in
  `d400a93`; explicitly not re-added — permanent overflow passes 400
  through per user decision).
- `/slots`-derived KV-pressure substitute (deferred; current llama.cpp has
  no KV metric upstream and the re-add PR #24010 is unmerged).
- Renaming the `vllm_*` gauges in tinyllb's own `/metrics` output (stable
  public surface; renaming would break existing dashboards).
- Retry for `/v1/models` or non-chat `/v1/completions` error translation.
- Per-flow retry budgets.

## Task order

```
01 (backend metrics: llamacpp constants + dual parser + snapshot fields
     + publish gate + watchdog progress)
 → 02 (config: backend.transient_retry + loader defaults/validation)
 → 03 (metrics: backend_retries_total + retry_exhausted_total counters)
 → 04 (gateway intake-error transient re-forward in proxy.rs)
 → 05 (gateway mid-stream error re-forward in stream.rs)
 → 06 (tests: backend unit + gateway stub)
 → 07 (docs + lat.md + lat check)
```

- 01 is foundational (the monitor is currently inert on llama.cpp).
- 02 and 03 are independent structural additions needed by 04/05.
- 04 and 05 touch different files (proxy.rs error block vs stream.rs) but
  share the `send_retry_request` helper and the transient config; done
  serially to avoid retry.rs conflicts.
- 06 depends on 01 + 04 + 05.
- 07 is last (docs reflect final behavior).

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Publish-gate change causes vLLM regressions. | Gate is `found_usage \|\| found_llamacpp`; vLLM scrapes still trip `found_usage`. Existing vLLM unit tests must pass unchanged. |
| Watchdog false-positive during cache-heavy prefills. | Progress check uses all four counters (incl. `cached_prompt_tokens` + `decode_calls`), not just `prompt_tokens_total`. |
| Re-forward duplicates streamed tokens. | Mid-stream re-forward only when no content frames forwarded yet; else current abort-body behavior. |
| Retrying a permanent error burns attempts. | Permanent (`n_prompt_tokens >= n_ctx`) is never retried — passed through immediately. |
| AppState field addition breaks construction sites. | Only `backend.transient_retry` is nested under `Backend` (already in AppState via `backend_url` etc.); `transient_retry` is read from `state.backend` or passed explicitly. Fewer sites than the top-level `retry_policy` precedent. |
| Backend swap without restart. | Per-scrape self-identification — flavor re-derives each poll, no restart needed. |

## Future work (not in this plan)

- `/slots`-derived context pressure as a KV-usage analog for `kv_policy`/
  `kv_bias`, once the value of the signal is proven (or upstream PR #24010
  ships a real KV metric).
- Per-flow transient-retry budget (back off if a flow consistently errors).
- Chat-history truncation as an opt-in recovery for permanent overflow
  (only if the "pass 400 through" decision proves insufficient in
  practice).
