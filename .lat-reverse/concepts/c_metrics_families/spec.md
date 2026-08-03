# Metrics Families

Observable metrics are organized into logical families, each tracking a distinct dimension of system behavior: backend health, queue dynamics, and generation throughput.

## Purpose

Metrics families provide a structured, cross-task view of system health and performance. Each family groups semantically related measurements so any consumer can query backend liveness, queue pressure, or token throughput independently.

- Backend family tracks in-flight request depth and server-side error rate.
- Queue family tracks per-flow wait depth, latency, and active concurrency.
- Throughput family tracks cumulative token output and approximate instantaneous rate.
- All collectors share one registry, making every metric family available to every task without additional wiring.
- Additional metric families beyond the three primary groups reside in the same registry (backpressure, scheduling, starvation protection, request lifecycle, KV cache).

## Non-goals

This concept does not address consumption, alerting, or naming policy.

- Metric scraping, aggregation, or alert rule design belong to the observability pipeline [[?c_observability_pipeline]].
- Client-facing error metrics (4xx responses) are explicitly excluded from the error counter.
- Histogram bucket configuration is not a contract for consumers; bucket boundaries may shift without breaking callers.
- Per-label breakdown beyond `flow_id` on queue depth is not part of the current scope.
- Metrics do not guarantee sub-second freshness for rate-approximated gauges.

## Interface

The interface provides construction surfaces, a scrape endpoint, and metric families accessible by public field name.

### Construction

- `create_metrics()` — Factory returning a shared reference-counted handle wrapping all metric collectors, safe to share across async tasks.
- `Metrics::new()` — Creates a standalone `Metrics` instance with all collectors registered. Caller manages sharing and ownership.
- `Metrics::default()` — Equivalent to `Metrics::new()` via the `Default` trait. Provides the standard `Default::default()` construction idiom.

### Scrape endpoint

- `GET /metrics` — HTTP endpoint returning Prometheus-format metric data with content type `text/plain; version=0.0.4`. Returns `200 OK` on success; encoding failures yield `500`.

### Field-access surface

- Every collector is exposed as a public struct field (e.g., `requests_active`, `errors_total`, `queue_depth`, `active_flows`). Callers read and update metrics through direct field access rather than a higher-level API.

### Backend family

- `vllm_requests_active` — Gauge reporting the current number of in-flight requests to the vLLM backend. Value increases when a request is dispatched and decreases when it completes (success or error).
- `vllm_errors_total` — Monotonically increasing counter of backend failures. Only server errors (5xx) and network-level errors are counted; client errors (4xx) are excluded.

### Queue family

- `llm_queue_depth` — Per-flow gauge of requests awaiting admission. Labeled by `flow_id`; ephemeral flows are aggregated to a fixed label value.
- `llm_queue_wait_seconds` — Histogram of wall-clock wait times, measured from queue entry to admission. Buckets span from 10 ms to 30 s.
- `llm_active_flows` — Gauge of currently admitted flows. Increments on admission, decrements when a flow terminates.

### Throughput family

- `llm_tokens_generated_total` — Monotonically increasing counter of tokens produced by the backend.
- `llm_tokens_per_second` — Gauge approximating instantaneous token throughput, derived from the cumulative counter at regular intervals.

## Invariants

The following statements hold regardless of implementation details.

- `vllm_requests_active` equals dispatched requests minus completed requests; its value reflects in-flight backend requests.
- `vllm_errors_total` excludes client errors (4xx); only server errors and network failures contribute.
- `llm_queue_depth` equals requests admitted to the scheduler minus requests granted admission; it reflects pending demand per flow.
- `llm_active_flows` is bounded above by the scheduler's admission capacity, because each active flow consumes one admission slot.
- `llm_tokens_per_second` is derived from `llm_tokens_generated_total` at periodic intervals; it approximates instantaneous throughput, not an exact rate.

## Constraints

The following limitations are intrinsic to the design.

- If collector creation fails during initialization, construction aborts fatally with no runtime recovery.
- Gauge values can become negative if decrement operations outnumber increments (e.g., double-completion), silently corrupting the measurement.
- Queue wait times exceeding the largest histogram bucket (30 s) lose granularity, recorded only in the overflow bucket.
- The throughput-rate gauge reflects the most recent computation interval; if background refresh stops, the value becomes stale without explicit indication.
- Labeled metrics on `llm_queue_depth` risk unbounded cardinality if ephemeral flows are not properly aggregated to the fixed label.

## Rationale

Organizing metrics into families clarifies the observation surface and constrains query patterns.

- Family grouping aligns metrics with their consumer: backend health dashboards, queue capacity planning, and throughput monitoring each map to one family.
- A single shared handle keeps every metric accessible to every task, avoiding the need to pass metric references through call chains.
- Excluding 4xx from the error counter keeps the error rate meaningful for backend health; client misuse is an expected operational signal, not a backend fault.
- The rate gauge is a derived convenience rather than a primary source; keeping it approximate avoids coupling the metric surface to timer precision guarantees.
- Automatic cleanup when a flow scope ends keeps the active-flows count accurate even when flows terminate unexpectedly.

## Related

- [[?c_fifo_scheduler]] — Admits requests into the queue; source of queue depth and wait-time observations.
- [[?c_queue_ticket]] — Flow-scoped permit whose lifetime drives `llm_active_flows`.
- [[?c_prometheus_registry]] — Underlying registration mechanism for all collectors.
- [[?c_backpressure]] — Backpressure rejection metrics housed in the same Metrics struct.
- [[?c_drr_scheduling]] — DRR flow credit metrics housed in the same Metrics struct.
- [[?c_starvation_protection]] — Starvation protection metrics housed in the same Metrics struct.
- [[?c_request_lifecycle]] — Request lifecycle event metrics housed in the same Metrics struct.
- [[?c_kv_cache]] — KV cache metrics housed in the same Metrics struct.
- [[src/metrics/mod.rs#Metrics]] — Central metrics struct carrying all families.
- [[src/metrics/mod.rs#create_metrics]] — Shared handle factory.
- [[src/metrics/endpoint.rs#metrics_handler]] — Scrape endpoint implementation.
- [[src/metrics/backend.rs]] — Backend family metric definitions.
- [[src/metrics/queue.rs]] — Queue family metric definitions.
- [[src/metrics/throughput.rs]] — Throughput family metric definitions.
