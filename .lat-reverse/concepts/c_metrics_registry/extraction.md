## Concept: c_metrics_registry

Role: Extractor. Scope: `src/metrics/mod.rs`. Observable behavior and evidence only.

## Responsibilities

- Acts as the single central collection point holding every Prometheus gauge, counter, and histogram for the LLM QDisc Proxy. Evidence: doc comment `src/metrics/mod.rs:10-15`; struct fields `src/metrics/mod.rs:16-58`.
- Creates a fresh Prometheus `Registry` and registers all defined collectors into it. Evidence: `Registry::new()` and sequential `registry.register(...)` calls `src/metrics/mod.rs:70`, `src/metrics/mod.rs:182-226`.
- Provides a handle designed to be shared across async tasks via `Arc`. Evidence: `create_metrics()` wraps a new instance in `Arc` `src/metrics/mod.rs:249-252`; doc comment mentions storage in `AppState` `src/metrics/mod.rs:13-15`.
- Exposes the module as a collection of domain sub-modules (backend, endpoint, queue, throughput). Evidence: `pub mod` declarations `src/metrics/mod.rs:1-4`.

## Interface Surfaces

### Surface: `Metrics` type (public struct with public collector fields)
- **Inputs/precondition:** a constructible instance already exists (via `Metrics::new()`).
- **Outputs/postcondition:** each public field is a live Prometheus collector registered in the struct's `registry` field, ready for observation.
- **Fields exposed (with their metric name and label cardinality, per `src/metrics/mod.rs:16-58`):**
  - Queue family: per-flow queue depth gauge (labeled by `flow_id`); wait-time histogram; count of active flows.
  - Throughput family: total tokens generated counter; tokens-per-second gauge.
  - Backend family: active requests gauge; backend errors counter.
  - Backpressure family: backpressure rejections counter (labeled by `mode`).
  - Scheduling family: per-flow DRR credit gauge (labeled by `flow_id`).
  - Starvation family: per-flow observed starvation-wait gauge (labeled by `flow_id`); total force-admit counter.
  - Lifecycle family: request-event counter (labeled by `event`, values started/received/completed/cancelled).
  - KV cache family: usage-percentage gauge; free-percentage gauge; admission-decision counter (labeled by `decision`: accept/delay/reject).
- **Errors:** metric-recording errors are delegated to the underlying Prometheus collectors; not surfaced by this module.

### Surface: `Metrics::new()`
- Inputs: none.
- Outputs: a fully assembled `Metrics` value in which every collector field has been created AND registered into the struct's `registry`.
- Errors: panics (fixed-message `expect`) if any collector cannot be constructed (`src/metrics/mod.rs:72-179`) or registered into the registry (`src/metrics/mod.rs:182-226`). No `Result`/graceful error path exists.

### Surface: `Default for Metrics`
- Equates default construction with `Metrics::new()`. Evidence: `src/metrics/mod.rs:60-64`.

### Surface: `create_metrics() -> Arc<Metrics>`
- Inputs: none.
- Outputs: an `Arc`-shared `Metrics` value usable concurrently across async tasks.
- Errors: inherits `Metrics::new()` panic behavior (no error return). Evidence: `src/metrics/mod.rs:249-253`.

## Invariants

- After construction, every public collector on the value is present in the value's own `registry`; the set of registered collectors matches the set of struct fields. Evidence: each field constructed `src/metrics/mod.rs:72-179` is followed by a matching registration `src/metrics/mod.rs:182-226`.
- Each collector has a fixed Prometheus metric name that is stable for the lifetime of the instance; names are distinct across all collectors. Evidence: unique `Opts::new`/metric names `src/metrics/mod.rs:72-179`.
- A single `Registry` instance is shared by all collectors in the value. Evidence: all registrations target the same `registry` binding `src/metrics/mod.rs:70,182-226`.
- The type is shareable by `Arc` and there is no attached mutable instance state required for cross-task access beyond the collectors themselves. Evidence: `create_metrics` returns `Arc` `src/metrics/mod.rs:249-252`.

## Failure Modes

- Construction aborts (panics) instead of returning an error if any metric name is invalid or if any registration into the registry conflicts (e.g. a duplicate metric name within the same registry). Evidence: panicking `expect` on creation and registration `src/metrics/mod.rs:72-179`, `src/metrics/mod.rs:182-226`.
- No graceful/`Result`-based failure path exists for construction; callers cannot recover from a construction failure at runtime. Evidence: `new()` returns the value type, not a fallible type `src/metrics/mod.rs:69-247`.
- Because names are fixed, adding a second collector with an identical name into the same registry would panic at registration; the current layout precludes this only by selecting distinct names. Evidence: distinct fixed names `src/metrics/mod.rs:72-179`.