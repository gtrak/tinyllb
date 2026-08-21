# Metrics Registry

A single central collection point for all operational telemetry of the LLM scheduling proxy. Every gauge, counter, and histogram converges into one scrapeable surface accessible via a shared metrics value and a public exposition endpoint.

## Purpose

This concept provides a unified collection point for scheduling observability metrics.

**Operational scope.**

- Collects per-flow queue depth, wait-time distribution, and active-flow counts for scheduling observability.
- Tracks throughput as cumulative token count and approximate tokens-per-second rate.
- Measures backend health via active-request depth and server-error occurrences (4xx client errors excluded).
- Records backpressure rejections categorized by rejection mode.
- Exposes per-flow scheduling credit and starvation-wait gauges for fair-queuing diagnostics.
- Counts request lifecycle events for end-to-end tracking.
- Monitors KV-cache utilization and admission decisions for capacity planning.

## Non-goals

This concept does not perform logging, alerting, data transformation, or arbitrary metric creation.

**Out of scope.**

- Not a logging subsystem; metrics are counters and gauges, not event narratives.
- Not configurable at runtime; the metric set is determined at build time and fixed upon creation.
- Not a general-purpose metrics SDK; it covers a predefined domain-specific inventory, not arbitrary user-defined metrics.
- Not designed for user-facing dashboards; the exposition surface serves Prometheus-format scrapers, not rendered views.
- Does not deduplicate or aggregate across multiple instances; each instance is the sole truth for its own collectors.

## Interface

The public contract consists of a metrics value with exposed collectors, multiple construction paths, a shareable handle factory, and an HTTP exposition endpoint.

**Metrics value and registry.**

- Each exposed collector is a live metric whose name and label dimensions are fixed for the lifetime of the value.
- The metrics value exposes its own registry as a public surface; external consumers read directly from it.
- The collector set includes queue, throughput, backend, backpressure, scheduling, starvation, lifecycle, KV-cache, priority-heuristic, premature-stop retry, backend retry, and backend-stall families.

**Construction paths.**

- A zero-argument constructor produces a fully assembled metrics value with all collectors registered into the value's registry.
- A default-constructed value yields the same result as the zero-argument constructor; the two paths are equivalent.
- A factory produces a shareable handle to a new metrics value, intended for concurrent access across async tasks without additional synchronization.

**Metric families.**

- **Queue family**: per-flow queue depth (labeled by flow identity), wait-time histogram (buckets 0.01–30.0 seconds), and count of active flows; ephemeral flows aggregate under the label value `ephemeral`.
- **Throughput family**: cumulative token generation count and approximate tokens-per-second rate.
- **Backend family**: active request depth and server-error count (5xx and network errors only; 4xx client errors excluded).
- **Backpressure family**: rejection counts categorized by backpressure mode.
- **Scheduling family**: per-flow scheduling credit (labeled by flow identity).
- **Starvation family**: per-flow starvation-wait observation (labeled by flow identity) and total forced-admission count.
- **Lifecycle family**: request-event counter (labeled by event type: request_started, token_received, request_completed, request_cancelled).
- **KV-cache family**: cache usage percentage, free percentage, and admission-decision count (labeled by decision: accept, delay, reject).
- **Backend stall family**: `llm_backend_stalled` gauge (1 while the inference watchdog considers the backend deadlocked) and `tinyllb_backend_stall_events_total` counter of watchdog-detected stalls; stall semantics documented in [[backend#Inference Stall Watchdog]].
- **Premature-stop retry family**: premature stop detection counter, retry requests issued counter, and degenerate-turn-forwarded counter. In the streaming path, the exhausted counter is also incremented when a retry HTTP failure forces fail-open.
- **Backend retry family**: `tinyllb_backend_retries_total` counter of transient backend-error re-forwards issued and `tinyllb_backend_retry_exhausted_total` counter of exhausted retry budgets; semantics documented in [[gateway#Transient Backend-Error Re-forward]].

**Exposition endpoint.**

- An HTTP handler serves all registered metrics in Prometheus text format at `GET /metrics` with `200 OK` and content type `text/plain; version=0.0.4`.
- If metric encoding fails, the endpoint returns `500 Internal Server Error` with an error log.

## Invariants

Construction guarantees that every collector is fully integrated into the value's own registry and that all metric names are distinct.

**Registration completeness.**

- After construction, the set of exposed collectors exactly matches the set registered in the value's registry.

**Name stability.**

- Each collector has a fixed Prometheus metric name for the lifetime of the value; no two collectors share the same name.

**Single registry.**

- All collectors write into one shared registry; no collector is orphaned or writes to a separate registry.

**Concurrent observability.**

- The metrics value supports concurrent observation across tasks without requiring exclusive ownership; collectors are observable through shared handles.

## Constraints

Construction is infallible and the metric inventory is fixed.

**Infallible construction.**

- Construction never returns an error to the caller; if any collector cannot be created or registered, construction panics instead of providing a graceful failure path.

**Fixed metric set.**

- The metric families and their label dimensions are determined at build time; runtime extension or removal of collectors is not supported.

**Prometheus binding.**

- The collector inventory is specific to the Prometheus ecosystem; definitions are not portable to other observability systems without redefinition.

**Single-instance assumption.**

- The design assumes one instance per deployment; co-located instances with overlapping metric names would conflict at the registry level.

## Rationale

A centralized, infallible registry simplifies correctness reasoning and ensures that metrics are always available when the system runs.

**Centralization.**

- A single collection point eliminates the risk of unregistered or orphaned collectors and makes the full metric surface discoverable from one location.

**Infallibility as a design signal.**

- Panicking on construction failure communicates that a valid metric registry is a prerequisite for system operation; a partial or misconfigured registry would mask operational failures downstream.

**Fixed inventory.**

- A static metric set prevents configuration drift and ensures that dashboards and alerts referencing these metrics remain valid across deployments.

**Shared handle design.**

- Shareable handles avoid synchronization overhead on every metric observation while guaranteeing that all tasks observe the same registry.

**Domain-specific scope.**

- Encoding the metric set as a domain-specific collection rather than a generic SDK keeps the surface tightly coupled to the proxy's scheduling and capacity concerns.

## Related

Cross-concept links and source locations for the metrics registry and its consumers.

- [[src/metrics/mod.rs#Metrics]] — metrics value definition and registry
- [[src/metrics/mod.rs#create_metrics]] — shareable handle factory

- [[src/metrics/endpoint.rs#metrics_handler]] — HTTP exposition endpoint
- [[src/metrics/backend.rs]] — backend-family module documentation
- [[gateway#Gateway Application State]] — application state container that holds the shareable metrics handle
- [[admission#Backpressure and Admission Rejection]] — backpressure policy whose rejection counts are recorded here
- [[admission#KV-Cache-Aware Admission Gate]] — cache management whose utilization and admission decisions are exposed here
- [[scheduler#Scheduler Facade and Policy Selection]] — scheduling logic whose fairness is measured by queue, starvation, and credit metrics
- [[flow#Flow Registry and State]] — flow identity tracking whose queue depth and wait-time are measured here

# Prometheus Export Endpoint

A read-only observation surface for all registered Prometheus metrics. It exposes the complete state of the metric registry in standardized exposition format, enabling external scrapers without affecting system behavior.

## Purpose

The metrics endpoint exposes all registered metric families through a single HTTP endpoint.

**Exposition contract.**

- Produces output conforming to the Prometheus text exposition format version 0.0.4.
- Requires no authentication or authorization to access.
- Accepts no client-supplied request data; response content is determined solely by current metric registry state.

## Non-goals

The metrics endpoint does not provide mutation, filtering, or streaming capabilities.

**Out of scope.**

- Does not accept, modify, or delete metrics.
- Does not filter, aggregate, or transform metric data on request.
- Does not support push-based metric ingestion.
- Does not expose internal system state beyond registered metric collectors.

## Interface

The metrics endpoint exposes a single HTTP surface with strict input and output contracts.

**HTTP GET `/metrics`.**

- Accepts no client-supplied request parameters, headers, or body; all request data is ignored.
- Returns `200 OK` with `Content-Type: text/plain; version=0.0.4` containing all metric families when encoding succeeds.
- Returns `500 Internal Server Error` with empty body only when metric serialization fails.

## Invariants

The endpoint guarantees all-or-nothing consistency between successful responses and the metric registry state.

**Response guarantees.**

- A successful response always contains valid Prometheus text exposition format; partial or malformed output never returns with `200`.
- The response body reflects the complete snapshot of all currently registered metric families at the time of the request.
- The `Content-Type` header is set to `text/plain; version=0.0.4` only on successful responses; error responses do not include a `Content-Type` header.
- Serialization failures produce an `ERROR`-level log entry before the `500` response is returned.
- Metric serialization failures produce `500 Internal Server Error`; there is no degraded fallback mode.

## Constraints

The endpoint is limited by the capabilities of the underlying metric registry and serialization layer.

**Operational limits.**

- Only metrics registered in the shared [[metrics#Metrics Registry]] are visible; unregistered collectors do not appear.
- Serialization failures are terminal for the response; there is no fallback format or degraded output mode.
- The endpoint operates asynchronously; clients receive either the complete metric set or an error response.
- No rate limiting or caching is applied at the endpoint layer.

## Rationale

The design prioritizes simplicity and observability compliance over extensibility.

**Design choices.**

- A single endpoint with no parameters eliminates ambiguity about what clients expect.
- Strict all-or-nothing output prevents partial metric data from misleading scrapers.
- The Prometheus exposition format is the standard interface for metric scraping; deviation would break ecosystem tooling.
- Unauthenticated access reflects the read-only, non-sensitive nature of aggregate metrics.
- Logging encoding failures aids operational diagnosis without masking errors from scrapers.

## Related

Cross-concept links and source locations for the exposition endpoint and its dependencies.

- [[metrics#Metrics Registry]] — Shared registry supplying metric families to the endpoint
- [[gateway#Gateway Application State]] — Application state object carrying the metric registry into request handlers
- [[src/metrics/mod.rs]] — Metric registry and collector definitions
- [[src/metrics/endpoint.rs]] — Endpoint handler implementation
- [[src/gateway/mod.rs]] — Request state carrying metrics into handlers
- [[src/main.rs]] — Route registration

# Metric Family Contracts

Observable metrics are organized into logical families, each tracking a distinct dimension of system behavior: backend health, queue dynamics, and generation throughput.

## Purpose

Metrics families provide a structured, cross-task view of system health and performance.

**Family overview.**

- Backend family tracks in-flight request depth and server-side error rate.
- Queue family tracks per-flow wait depth, latency, and active concurrency.
- Throughput family tracks cumulative token output and approximate instantaneous rate.
- All collectors share one registry, making every metric family available to every task without additional wiring.
- Additional metric families beyond the three primary groups reside in the same registry (backpressure, scheduling, starvation protection, request lifecycle, KV cache, priority heuristic, premature-stop retry, backend retry, and backend stall).
- Priority heuristic family tracks per-flow priority class, cadence state-machine state, priority source events, and inter-request gap distribution for turn-boundary classification diagnostics.
- Premature-stop retry family tracks premature-stop detections, retry attempts issued, and degenerate turns forwarded after retries are exhausted.
- Backend stall family tracks the watchdog's deadlocked-engine gauge and the count of detected stall events; see [[backend#Inference Stall Watchdog]].

## Non-goals

This concept does not address consumption, alerting, or naming policy.

**Exclusions.**

- Metric scraping, aggregation, or alert rule design belong to external observability pipelines.
- Client-facing error metrics (4xx responses) are explicitly excluded from the error counter.
- Histogram bucket configuration is not a contract for consumers; bucket boundaries may shift without breaking callers.
- Per-label breakdown beyond `flow_id` on queue depth is not part of the current scope.
- Metrics do not guarantee sub-second freshness for rate-approximated gauges.

## Interface

The interface provides construction surfaces, a scrape endpoint, and metric families accessible by public field name.

**Construction.**

- `create_metrics()` — Factory returning a shared reference-counted handle wrapping all metric collectors, safe to share across async tasks.
- `Metrics::new()` — Creates a standalone metrics instance with all collectors registered. Caller manages sharing and ownership.
- `Metrics::default()` — Equivalent to `Metrics::new()` via the `Default` trait.

**Scrape endpoint.**

- `GET /metrics` — HTTP endpoint returning Prometheus-format metric data with content type `text/plain; version=0.0.4`. Returns `200 OK` on success; encoding failures yield `500`.

**Field-access surface.**

- Every collector is exposed as a public struct field (e.g., `requests_active`, `errors_total`, `queue_depth`, `active_flows`). Callers read and update metrics through direct field access.

**Backend family.**

- `vllm_requests_active` — Gauge reporting current in-flight requests to the vLLM backend. Increases on dispatch, decreases on completion.
- `vllm_errors_total` — Monotonically increasing counter of backend failures. Only server errors (5xx) and network-level errors are counted.

**Queue family.**

- `llm_queue_depth` — Per-flow gauge of requests awaiting admission. Labeled by `flow_id`; ephemeral flows aggregate to a fixed label value.
- `llm_queue_wait_seconds` — Histogram of wall-clock wait times, measured from queue entry to admission. Buckets span 10 ms to 30 s.
- `llm_active_flows` — Gauge of currently admitted flows. Increments on admission, decrements when a flow terminates.

**Throughput family.**

- `llm_tokens_generated_total` — Monotonically increasing counter of tokens produced by the backend.
- `llm_tokens_per_second` — Gauge approximating instantaneous token throughput, derived from the cumulative counter at regular intervals.

**Backend stall family.**

- `llm_backend_stalled` — Gauge set to 1 while the inference watchdog considers the backend deadlocked (engine busy with no token progress), 0 otherwise.
- `tinyllb_backend_stall_events_total` — Monotonically increasing counter of backend inference stalls detected by the watchdog; each newly detected stall increments it once. See [[backend#Inference Stall Watchdog]] for stall semantics.

**Priority heuristic family.**

- `llm_flow_priority_class` — Per-flow gauge of the current numeric priority value (100/50/10). Updated on every admission after the turn-boundary state machine runs. Labeled by `flow_id`; ephemeral flows aggregate to `"ephemeral"`.
- `llm_flow_cadence_state` — Per-flow gauge of the state-machine state (0=cold, 1=interactive, 2=agentic_suspected, 3=agentic_confirmed). Updated on every admission. Disambiguates Cold from Interactive (both priority 100). Labeled by `flow_id`; ephemeral flows aggregate to `"ephemeral"`.
- `llm_flow_priority_source_total` — Counter of explicit priority-override events, labeled by `flow_id` and `source` (`header`, `admin`, `auto`). Incremented when a flow's priority is pinned via the `X-LLM-Priority` header, set via the admin API, or cleared via `auto`.
- `llm_flow_inter_request_seconds` — Per-flow histogram of observed inter-request gaps (seconds). Observed on every admission after the first; the first arrival for a flow produces no observation. Buckets span 0.1s to 120s. Labeled by `flow_id`.

**Premature-stop retry family.**

- `tinyllb_premature_stop_detected_total` — Premature stops detected (one per failed attempt).
- `tinyllb_premature_stop_retries_total` — Retry requests issued after a premature stop.
- `tinyllb_premature_stop_exhausted_total` — Degenerate turns forwarded after all retries exhausted. In the streaming path, this is also incremented when a retry HTTP failure forces fail-open.

**Backend retry family.**

- `tinyllb_backend_retries_total` — Monotonically increasing counter of transient backend-error re-forwards issued by the gateway; incremented once per re-forward attempt.
- `tinyllb_backend_retry_exhausted_total` — Monotonically increasing counter of requests whose transient retry budget was exhausted: the final error is still transient after all attempts, or the final network failure is transient. Permanent and non-llama.cpp errors are never counted here. See [[gateway#Transient Backend-Error Re-forward]].

## Invariants

The following statements hold regardless of implementation details.

**Metric relationships.**

- `vllm_requests_active` equals dispatched requests minus completed requests; its value reflects in-flight backend requests.
- `vllm_errors_total` excludes client errors (4xx); only server errors and network failures contribute.
- `llm_queue_depth` equals requests admitted to the scheduler minus requests granted admission; it reflects pending demand per flow.
- `llm_active_flows` is bounded above by the scheduler's admission capacity, because each active flow consumes one admission slot.
- `llm_tokens_per_second` is derived from `llm_tokens_generated_total` at periodic intervals; it approximates instantaneous throughput, not an exact rate.

## Constraints

The following limitations are intrinsic to the design.

**Operational limits.**

- If collector creation fails during initialization, construction aborts fatally with no runtime recovery.
- Gauge values can become negative if decrement operations outnumber increments (e.g., double-completion), silently corrupting the measurement.
- Queue wait times exceeding the largest histogram bucket (30 s) lose granularity, recorded only in the overflow bucket.
- The throughput-rate gauge reflects the most recent computation interval; if background refresh stops, the value becomes stale without explicit indication.
- Labeled metrics on `llm_queue_depth` risk unbounded cardinality if ephemeral flows are not properly aggregated to the fixed label.

## Rationale

Organizing metrics into families clarifies the observation surface and constrains query patterns.

**Design reasoning.**

- Family grouping aligns metrics with their consumer: backend health dashboards, queue capacity planning, and throughput monitoring each map to one family.
- A single shared handle keeps every metric accessible to every task, avoiding the need to pass metric references through call chains.
- Excluding 4xx from the error counter keeps the error rate meaningful for backend health; client misuse is an expected operational signal, not a backend fault.
- The rate gauge is a derived convenience rather than a primary source; keeping it approximate avoids coupling the metric surface to timer precision guarantees.
- Automatic cleanup when a flow scope ends keeps the active-flows count accurate even when flows terminate unexpectedly.

## Related

Cross-concept links and source locations for metric families and their consumers.

- [[scheduler#Deficit Round Robin Discipline]] — Admits requests into the queue; source of queue depth and wait-time observations.
- [[admission#Backpressure and Admission Rejection]] — Backpressure rejection metrics housed in the same metrics struct.
- [[admission#KV-Cache-Aware Admission Gate]] — KV cache metrics housed in the same metrics struct.
- [[src/metrics/mod.rs#Metrics]] — Central metrics struct carrying all families.
- [[src/metrics/mod.rs#create_metrics]] — Shared handle factory.
- [[src/metrics/endpoint.rs#metrics_handler]] — Scrape endpoint implementation.
- [[src/metrics/backend.rs]] — Backend family metric definitions.
- [[src/metrics/queue.rs]] — Queue family metric definitions.
- [[src/metrics/throughput.rs]] — Throughput family metric definitions.
- [[scheduler#Scheduler Facade and Policy Selection]] — scheduler that records priority class and inter-request gap metrics on every admission
- [[flow#Flow Identification]] — priority header override events recorded as source counter increments
