# 04 — Prometheus Metrics + /metrics Endpoint

**Phase:** 1 (Basic Queue Proxy)
**Depends on:** `03`.
**Cross-cutting:** revisited in Phase 2 (`11`) and Phase 3 (`15`).

## Objective

Stand up the Prometheus metrics registry and the `GET /metrics` endpoint with
the **queue**, **throughput**, and **backend** families from PRD §8.  Scheduling
metrics (flow credit, starvation) are added in Phase 2; KV metrics in Phase 3.
This issue wires the infra (registry, exporter, label conventions) once so
later issues only register new collectors.

## Files

| File | Change |
| --- | --- |
| `src/metrics/mod.rs` | New: global `PrometheusRegistry`, `register!` helpers. |
| `src/metrics/queue.rs` | New: `llm_queue_depth`, `llm_queue_wait_seconds`, `llm_active_flows`. |
| `src/metrics/throughput.rs` | New: `llm_tokens_generated_total`, `llm_tokens_per_second`. |
| `src/metrics/backend.rs` | New: `vllm_requests_active`, `vllm_errors_total`. |
| `src/metrics/endpoint.rs` | New: axum `GET /metrics` -> prometheus text format. |
| `src/main.rs` | Edit: mount `/metrics`; instrument gateway with counters. |
| `tests/metrics.rs` | New: assert metrics exist with correct names/types. |

## Steps

1. Use the `prometheus` crate; create a single `Registry` on `AppState`.
2. Define gauges/counters/histograms exactly as named in PRD §8:
   * `llm_queue_depth{}` gauge
   * `llm_queue_wait_seconds{}` histogram (buckets per PRD TBD; default 0.01→30s)
   * `llm_active_flows{}` gauge
   * `llm_tokens_generated_total{}` counter
   * `llm_tokens_per_second{}` gauge (updated periodically)
   * `vllm_requests_active{}` gauge
   * `vllm_errors_total{}` counter
3. `GET /metrics`: `prometheus::TextEncoder` encode the registry; return
   `200` with `text/plain; version=0.0.4` per OpenMetrics.
4. Instrument `src/gateway` (from `03`): `vllm_requests_active` inc/dec around
   forwarded requests, `vllm_errors_total` inc on backend 5xx / network error,
   `llm_tokens_generated_total` inc per SSE chunk `usage` delta (best-effort
   parse; fall back to no-op if usage frame absent).
5. Background task: every 1s recompute `llm_tokens_per_second` from the
   counter's rate window.
6. Tests: scrape `/metrics`, assert each PRD-named metric string is present
   and typed correctly (counter vs gauge vs histogram).

## Verification

* `cargo test --test metrics`.
* `curl localhost:8080/metrics` lists every PRD §8 queue/throughput/backend
  metric with correct type.
* After a forwarded request: `vllm_requests_active` reflects in-flight count;
  `llm_tokens_generated_total` (where parseable) increments.
* `promtool check metrics /tmp/scrape.txt` passes (no malformed lines).
