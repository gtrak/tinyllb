## Purpose

This concept provides a single, central collection point for all operational telemetry of the LLM scheduling proxy. Every gauge, counter, and histogram across queue depth, throughput, backpressure, resource utilization, and request lifecycle converges into one scrapeable surface, accessible via a shared metrics value and a public exposition endpoint.

### Operational scope

- Collects per-flow queue depth, wait-time distribution, and active-flow counts for scheduling observability.
- Tracks throughput as cumulative token count and approximate tokens-per-second rate.
- Measures backend health via active-request depth and server-error occurrences (4xx client errors excluded).
- Records backpressure rejections categorized by rejection mode.
- Exposes per-flow scheduling credit and starvation-wait gauges for fair-queuing diagnostics.
- Counts request lifecycle events for end-to-end tracking.
- Monitors KV-cache utilization and admission decisions for capacity planning.

## Non-goals

This concept does not perform logging, alerting, data transformation, or arbitrary metric creation.

### Out of scope

- Not a logging subsystem; metrics are counters and gauges, not event narratives.
- Not configurable at runtime; the metric set is determined at build time and fixed upon creation.
- Not a general-purpose metrics SDK; it covers a predefined domain-specific inventory, not arbitrary user-defined metrics.
- Not designed for user-facing dashboards; the exposition surface serves Prometheus-format scrapers, not rendered views.
- Does not deduplicate or aggregate across multiple instances; each instance is the sole truth for its own collectors.

## Interface

The public contract consists of a metrics value with exposed collectors and an accessible registry, multiple construction paths, a shareable handle factory, and an HTTP exposition endpoint.

### Metrics value and registry

- Each exposed collector is a live metric whose name and label dimensions are fixed for the lifetime of the value.
- The metrics value exposes its own registry as a public surface; external consumers read directly from it.
- The collector set includes queue, throughput, backend, backpressure, scheduling, starvation, lifecycle, and KV-cache families.

### Construction paths

- A zero-argument constructor produces a fully assembled metrics value in which all collectors are registered into the value's registry.
- A default-constructed value yields the same result as the zero-argument constructor; the two paths are equivalent.
- A factory produces a shareable handle to a new metrics value, intended for concurrent access across async tasks without additional synchronization.
- Public submodules (`backend`, `endpoint`, `queue`, `throughput`) constitute part of the crate's interface surface.

### Metric families

- **Queue family**: per-flow queue depth (labeled by flow identity), wait-time histogram (buckets 0.01–30.0 seconds), and count of active flows; ephemeral flows aggregate under the label value `ephemeral`.
- **Throughput family**: cumulative token generation count and approximate tokens-per-second rate.
- **Backend family**: active request depth and server-error count (5xx and network errors only; 4xx client errors excluded).
- **Backpressure family**: rejection counts categorized by backpressure mode.
- **Scheduling family**: per-flow scheduling credit (labeled by flow identity).
- **Starvation family**: per-flow starvation-wait observation (labeled by flow identity) and total forced-admission count.
- **Lifecycle family**: request-event counter (labeled by event type: request_started, token_received, request_completed, request_cancelled).
- **KV-cache family**: cache usage percentage, free percentage, and admission-decision count (labeled by decision: accept, delay, reject).

### Exposition endpoint

- An HTTP handler serves all registered metrics in Prometheus text format at `GET /metrics` with `200 OK` and content type `text/plain; version=0.0.4`.
- If metric encoding fails, the endpoint returns `500 Internal Server Error` with an error log.

## Invariants

Construction guarantees that every collector is fully integrated into the value's own registry and that all metric names are distinct.

### Registration completeness

- After construction, the set of exposed collectors exactly matches the set registered in the value's registry.

### Name stability

- Each collector has a fixed Prometheus metric name for the lifetime of the value; no two collectors share the same name.

### Single registry

- All collectors write into one shared registry; no collector is orphaned or writes to a separate registry.

### Concurrent observability

- The metrics value supports concurrent observation across tasks without requiring exclusive ownership; collectors are observable through shared handles.

## Constraints

Construction is infallible and the metric inventory is fixed.

### Infallible construction

- Construction never returns an error to the caller; if any collector cannot be created or registered, construction panics instead of providing a graceful failure path.

### Fixed metric set

- The metric families and their label dimensions are determined at build time; runtime extension or removal of collectors is not supported.

### Prometheus binding

- The collector inventory is specific to the Prometheus ecosystem; definitions are not portable to other observability systems without redefinition.

### Single-instance assumption

- The design assumes one instance per deployment; co-located instances with overlapping metric names would conflict at the registry level.

## Rationale

A centralized, infallible registry simplifies correctness reasoning and ensures that metrics are always available when the system runs.

### Centralization

- A single collection point eliminates the risk of unregistered or orphaned collectors and makes the full metric surface discoverable from one location.

### Infallibility as a design signal

- Panicking on construction failure communicates that a valid metric registry is a prerequisite for system operation; a partial or misconfigured registry would mask operational failures downstream.

### Fixed inventory

- A static metric set prevents configuration drift and ensures that dashboards and alerts referencing these metrics remain valid across deployments.

### Shared handle design

- Shareable handles avoid synchronization overhead on every metric observation while guaranteeing that all tasks observe the same registry.

### Domain-specific scope

- Encoding the metric set as a domain-specific collection rather than a generic SDK keeps the surface tightly coupled to the proxy's scheduling and capacity concerns, reducing the chance of irrelevant or unused metrics.

## Related

- [[src/metrics/mod.rs#Metrics]] — metrics value definition and registry
- [[src/metrics/mod.rs#create_metrics]] — shareable handle factory
- [[src/metrics/mod.rs#new]] — zero-argument constructor
- [[src/metrics/mod.rs#default]] — default trait implementation
- [[src/metrics/endpoint.rs#metrics_handler]] — HTTP exposition endpoint
- [[src/metrics/backend.rs]] — backend-family module documentation
- [[?c_app_state]] — application state container that holds the shareable metrics handle
- [[?c_request_scheduling]] — scheduling logic whose fairness is measured by queue, starvation, and credit metrics
- [[?c_backpressure]] — backpressure policy whose rejection counts are recorded here
- [[?c_kv_cache]] — cache management whose utilization and admission decisions are exposed here
- [[?c_flow_management]] — flow identity tracking whose queue depth and wait-time are measured here
