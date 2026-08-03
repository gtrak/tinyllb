# Scheduler Facade

## Purpose

The scheduler facade presents a single admission-control surface that hides algorithm-specific dispatch. It guarantees that every admission attempt passes through a shared KV-cache gate before consulting the selected scheduling policy, and that all queue metrics uniformly account for requests delayed by that gate.

- Provides a unified entry point regardless of which scheduling algorithm is configured
- Enforces a KV-cache admission gate before every admit attempt
- Ensures queue depth and snapshot metrics always include KV-delayed requests
- Exposes shared policy state — completion bias, starvation timeout, flow progress — to all algorithm variants
- Supports algorithm-specific accounting through a uniform query interface

## Non-goals

The facade deliberately does not implement scheduling logic or define policy parameters.

- Does not implement any scheduling algorithm; dispatch is delegated to an algorithm-specific backend
- Does not decide admission policy; delegates to the configured scheduler and the KV gate
- Does not manage request lifecycle beyond admission and accounting report
- Does not define backpressure policy; delegates retry-after computation to the underlying scheduler
- Does not persist state; all metrics and accounting are ephemeral to the current instance

## Interface

The facade exposes public contract surfaces that callers may rely on, including facade operations, publicly re-exported types, and one public submodule.

### Admission

A request to schedule a work unit for a flow. Accepts a flow identity and a work quantity; returns either a queue ticket confirming admission or a backpressure rejection carrying a retry-after duration. No admission succeeds without passing the KV-cache gate.

### Queue observation

Two read-only surfaces for inspecting queue state. The depth query returns the total number of queued requests, including those delayed by the KV gate. The snapshot query returns counts of active flows, waiting requests, and total flow count.

### Flow accounting

Algorithm-specific per-flow queries. A service-total query returns cumulative service for a flow — meaningful only for work-tracking algorithms. A credit query returns the current credit balance — meaningful only for credit-based algorithms. An accounting report accepts a completed request's outcome and updates the flow's internal accounting, or silently no-ops for algorithms without accounting.

### Shared state access

Shared ownership of the flow progress tracker, available to all callers regardless of configured algorithm. The returned tracker reference grants shared access equivalent to the facade's own internal reference.

### Construction

Two construction paths exist. The full constructor accepts all policy parameters and produces an instance without failure. The defaults constructor accepts a reduced parameter set and fills remaining values with fixed defaults, including an empty backend monitor and a KV-cache configuration derived from type-level defaults.

### Publicly re-exported types

The module re-exports algorithm schedulers, policy types, admission artifacts, and helper functions. Callers may import these directly for use outside the facade, including instantiating algorithm backends that bypass the facade's KV gate and shared policy layer.

### Public submodule

The lifecycle submodule is publicly accessible, exposing request lifecycle types as a supplementary contract surface beyond the re-exported types.

## Invariants

These statements hold regardless of which scheduling algorithm is configured or how the facade is reconstructed.

### KV gate ordering

The KV-cache admission gate always executes before the flow-scheduler consults its own policy. A KV-policy rejection prevents the flow-scheduler from seeing the request.

### Queue metric aggregation

The reported queue depth always equals the sum of the flow-scheduler's queue depth and the number of requests currently delayed by the KV gate.

### Snapshot completeness

The waiting count in any queue snapshot always includes both the flow-scheduler's waiting requests and the KV-delayed requests.

### Algorithm-exhaustive dispatch

Every public operation that delegates to the underlying scheduler covers all configured algorithm variants; no variant is left unhandled.

### Neutral accounting for non-applicable algorithms

Querying service total on a non-work-tracking algorithm always yields zero. Querying credit on a non-credit algorithm always yields zero. Reporting accounting to a non-accounting algorithm has no observable effect.

## Constraints

These are limitations imposed by the design, not implementation accidents.

### Single algorithm per instance

An instance dispatches to exactly one scheduling algorithm, selected at construction time. The algorithm cannot change during the instance's lifetime.

### Fixed defaults for backward-compatible construction

The defaults constructor applies fixed values: starvation timeout at 300 seconds, completion bias at its type-level default, an empty backend monitor, and a KV-cache configuration derived from type-level defaults.

### Backpressure rejection carries retry information

Every backpressure rejection includes a retry-after duration; the facade never rejects silently.

### Configuration-driven admission

Admission policy — including maximum active flows, queue depth limits, and wait timeouts — is entirely determined at construction. No runtime reconfiguration is exposed.

### Public algorithm types bypass the facade

Re-exported algorithm scheduler types enable direct instantiation outside the facade. Callers using these types bypass the facade's KV gate and shared policy layer entirely.

## Rationale

The facade exists to make algorithm selection an operational concern rather than an architectural one.

- **Single facade**: Callers depend on admission and queue metrics, not on which algorithm handles dispatch. A facade isolates callers from algorithm churn.
- **KV gate before scheduling**: KV-cache capacity is a hard resource limit. Checking it first avoids wasting scheduler state on requests that cannot be served regardless of policy.
- **Aggregated queue metrics**: Operators need a total picture of backlogged work. Splitting between scheduler-queued and KV-delayed would require callers to reconcile two sources.
- **Neutral accounting for irrelevant algorithms**: Exposing service and credit queries uniformly avoids caller-side branching on algorithm type. Algorithms that do not track accounting return neutral values instead of errors.
- **Default constructor for convenience**: Most deployments do not need custom starvation or KV policy. A defaults path reduces boilerplate for common configurations.

## Related

- [[?c_fifo_scheduler]] — FIFO scheduling algorithm
- [[?c_wfq_scheduler]] — Weighted-fair-queue scheduling algorithm
- [[?c_drr_scheduler]] — Deficit-round-robin scheduling algorithm
- [[?c_kv_admission]] — KV-cache admission gate policy
- [[?c_backpressure]] — Backpressure and retry-after computation
- [[?c_flow_progress]] — Flow progress tracking
- [[?c_completion_bias]] — Completion bias policy
- [[src/scheduler/mod.rs]] — Facade implementation
- [[src/scheduler/backpressure.rs]] — Backpressure rejection and retry-after helpers
- [[src/scheduler/kv_admission.rs]] — KV-cache admission gate
- [[src/scheduler/flow_progress.rs]] — Flow progress tracker
- [[src/scheduler/fifo.rs]] — FIFO scheduler implementation
- [[src/scheduler/wfq.rs]] — WFQ scheduler implementation
- [[src/scheduler/drr.rs]] — DRR scheduler implementation
- [[src/scheduler/lifecycle.rs]] — Lifecycle types and accounting
