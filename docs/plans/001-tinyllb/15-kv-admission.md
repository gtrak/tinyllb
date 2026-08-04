# 15 — KV-Cache-Aware Admission (vLLM Metrics Integration)

**Phase:** 3 (vLLM Integration)
**Depends on:** `02`, `04`, `08`, `11`, `12`, `14`.
**Blocks:** `16`.

## Objective

Implement the optional KV-cache-aware admission from PRD §6.3:

> Input: `current KV usage`, `available blocks`.
> Decision: `accept | delay | reject`.

The proxy queries vLLM's existing metrics endpoint (vLLM exposes
`/metrics` in Prometheus format) for KV-cache pressure, then folds that into
admission decisions alongside the flow-aware scheduler from Phase 2.

## Files

| File | Change |
| --- | --- |
| `src/backend/monitor.rs` | New: periodic poll of vLLM `/metrics`; parse KV gauges. |
| `src/scheduler/kv_admission.rs` | New: `KvPolicy` deciding accept/delay/reject from usage. |
| `src/scheduler/mod.rs` | Edit: compose `KvPolicy` into the admit path before flow scheduler. |
| `src/metrics/backend.rs` | Edit: add `vllm_kv_cache_usage`, `vllm_kv_cache_free`. |
| `tests/kv_admission.rs` | New: stub vLLM metrics server drives policy decisions. |

## Steps

1. Discover actual vLLM metric names for KV cache (e.g. `vllm:gpu_cache_usage_perc`,
    `vllm:num_preemption`).  Confirm against the installed vLLM version's
    `/metrics` output; record the names in `src/backend/metrics.rs` as
    constants so a version bump is a single edit.
2. `BackendMonitor` tokio task: every `backend.metrics_interval` (default
   1s), `reqwest::get(backend.url + "/metrics")`, parse via `prometheus`
     parser into a typed `BackendSnapshot { kv_usage, kv_free, preemptions }`.
   Store behind `ArcSwap` for lock-free reads.
3. `KvPolicy::decide(snapshot, request_cost) -> Decision`:
   * `kv_usage > reject_threshold` (default 0.95): `Reject` with `Retry-After`,
   * `kv_usage > delay_threshold` (default 0.80): `Delay` (enqueue but don't
     admit until usage drops),
   * else: `Accept` -> flow into existing scheduler admit (`11`/`12`).
4. Compose into the admit path: KV decision runs **first**; `Reject`
   triggers `06`'s backpressure 429 path; `Delay` parks the request in the
   queue; `Accept` proceeds.
5. Add `vllm_kv_cache_usage` / `vllm_kv_cache_free` gauges plus
   `llm_kv_admission_decisions_total{decision="accept|delay|reject"}` counter.
6. Tests against a stub `/metrics` Prometheus server emitting the real vLLM
   metric names; assert accept / delay / reject transitions at the configured
   thresholds.

## Verification

* `cargo test --test kv_admission` green.
* With a stub reporting `vllm:gpu_cache_usage_perc = 0.85`, new requests are
   delayed (queued, not rejected).
* With `= 0.96`, new requests rejected with `429 + Retry-After`.
* Below 0.80, requests flow through unchanged from Phase 2 behavior.
* `/metrics` exposes `vllm_kv_cache_usage` reflecting the stub's value, and
  `llm_kv_admission_decisions_total{decision=...}`.
* Live vLLM sanity check (manual; non-blocking) confirms metric names parse
  without error on a real `/metrics`.
