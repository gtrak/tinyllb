# 01 — Backend metrics: llama.cpp support

- **Complexity:** S
- **Timebox:** 45 min
- **Depends on:** nothing (foundational)

## Objective

Make `src/backend/mod.rs` parse llama.cpp `llamacpp:*` Prometheus metrics
into the same `BackendSnapshot`, fix the publish-gate that currently
suppresses all llama.cpp publishing, and extend the stall watchdog's
progress check so cache-heavy workloads don't false-positive.

## Files

| File | Change |
|------|--------|
| `src/backend/mod.rs` | Add `llamacpp:*` metric-name constants; add `cached_prompt_tokens` + `decode_calls` fields to `BackendSnapshot` (default 0); add `found_llamacpp` flag to `ParseSnapshotResult`; parse both families into the same fields (last-parsed-wins); change poll-loop publish gate to `found_usage \|\| found_llamacpp`; extend watchdog `progressed` check to include `cached_prompt_tokens` and `decode_calls`; `tracing::info!` flavor change (vllm ↔ llama.cpp) — keep a `last_flavor: Option<Flavor>` in the poll loop. |

No other files. `lat.md` is task 07.

## Context (verified facts — do not re-derive)

- Live `/metrics` scrape on `localhost:8000` (llama-server with `--metrics`)
  emits exactly (copied verbatim): `llamacpp:prompt_tokens_total`,
  `llamacpp:prompt_tokens_cached_total`, `llamacpp:prompt_seconds_total`,
  `llamacpp:tokens_predicted_total`, `llamacpp:tokens_predicted_seconds_total`,
  `llamacpp:n_decode_total`, `llamacpp:n_tokens_max`,
  `llamacpp:spec_decode_*`, `llamacpp:prompt_tokens_seconds`,
  `llamacpp:predicted_tokens_seconds`, `llamacpp:requests_processing`,
  `llamacpp:requests_deferred`, `llamacpp:n_busy_slots_per_decode`.
- **No `llamacpp:kv_cache_usage_ratio` exists** (removed upstream in
  #13660; re-add PR #24010 unmerged). So `found_usage` is always false on
  llama.cpp — this is exactly why the current `found_usage`-only gate
  suppresses everything.
- `llamacpp:prompt_tokens_total` is documented "excluding cached tokens";
  the live `/slots` showed 74180/75869 cached prefixes, so cache activity
  won't move `prompt_tokens_total`. `llamacpp:prompt_tokens_cached_total`
  and `llamacpp:n_decode_total` (every `llama_decode()` call, including
  prefill batches) are the missing progress signals.
- The parser's `parse_prometheus_line` already handles colon-containing
  metric names and labels — no change needed there.

## Steps

1. Add constants (alongside the existing vLLM ones), each with a comment:
   - `METRIC_LLAMACPP_REQUESTS_PROCESSING = "llamacpp:requests_processing"`
   - `METRIC_LLAMACPP_REQUESTS_DEFERRED  = "llamacpp:requests_deferred"`
   - `METRIC_LLAMACPP_PROMPT_TOKENS     = "llamacpp:prompt_tokens_total"`
   - `METRIC_LLAMACPP_PREDICTED_TOKENS  = "llamacpp:tokens_predicted_total"`
   - `METRIC_LLAMACPP_CACHED_TOKENS     = "llamacpp:prompt_tokens_cached_total"`
   - `METRIC_LLAMACPP_DECODE_CALLS      = "llamacpp:n_decode_total"`
2. Add fields to `BackendSnapshot`: `cached_prompt_tokens: f64`,
   `decode_calls: f64` (both default 0.0 in `Default`). Document that these
   are llama.cpp progress-only signals (no vLLM analog).
3. Add `pub found_llamacpp: bool` to `ParseSnapshotResult` (default false
   via `#[derive(Default)]`).
4. In `parse_snapshot`, extend the `match name { … }` arms: the existing
   vLLM arms stay; add arms mapping each `llamacpp:*` constant to the
   corresponding snapshot field, and set `found_llamacpp = true` when any
   llamacpp metric is parsed. (Do this either as additional match arms or
   with a separate `is_llamacpp` check — match arms are cleaner. Note the
   `|` alternation pattern already used for v0/v1 usage.)
5. In `poll_loop`, change the publish gate:
   ```rust
   if result.found_usage || result.found_llamacpp {
       // ... existing publish + gauge writes + watchdog ...
   }
   ```
   Inside the watchdog, extend `progressed`:
   ```rust
   let progressed = snapshot.prompt_tokens != last_prompt_tokens
       || snapshot.generation_tokens != last_generation_tokens
       || snapshot.cached_prompt_tokens != last_cached_tokens
       || snapshot.decode_calls != last_decode_calls;
   ```
   Add `last_cached_tokens` and `last_decode_calls` tracking vars (init 0.0,
   updated alongside the existing `last_prompt_tokens`/`last_generation_tokens`).
6. Add a small flavor log in the poll loop: keep
   `last_flavor: Option<&str>` (or an enum). On a published scrape, compute
   `flavor = if result.found_usage { "vllm" } else if result.found_llamacpp
   { "llama-cpp" } else { "unknown" }`. If it differs from `last_flavor`,
   `tracing::info!(flavor, "detected backend metrics flavor")` and update
   it. (No metric gauge required; a log line is enough for task 01.)
7. The `vllm_kv_cache_usage`/`vllm_kv_cache_free` gauge writes stay gated
   on `result.found_usage` (only written for vLLM) — they're inside the
   `if result.found_usage` block today; keep them there (move them before
   the `||` restructure so they don't run on llama.cpp). Concretely:
   structure the block as `if found_usage { write gauges }` then
   `if found_usage || found_llamacpp { publish snapshot; watchdog }`.

## Tests (add to `#[cfg(test)] mod tests` in the same file)

- `parse_snapshot_realistic_llamacpp_metrics`: use a body copied from the
  live scrape (the seven relevant lines). Assert `found_llamacpp == true`,
  `found_usage == false`, and that `requests_running`, `requests_waiting`,
  `prompt_tokens`, `generation_tokens`, `cached_prompt_tokens`,
  `decode_calls` are populated correctly; `kv_usage == 0.0`, `kv_free ==
  1.0`.
- `parse_snapshot_mixed_families_last_wins`: a body with both a vLLM
  usage line and llamacpp usage-like lines — assert `found_usage == true`
  and `found_llamacpp == true`, and that field precedence is
  last-parsed-wins (document the behavior).
- `parse_snapshot_llamacpp_no_kv_metric`: confirm `found_usage` stays false
  and `kv_free` derives to 1.0 (default) — i.e., no spurious KV-pressure
  on llama.cpp.
- Keep all existing vLLM tests passing unchanged (the proof of no
  regression).

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
All must pass, including the existing vLLM parse tests. Run a quick live
sanity check (informational, not gating): `curl -s localhost:8000/metrics |
head` should still show the `llamacpp:` family.
