# FIFO Scheduler — Specification

## Purpose

The FIFO scheduler provides a concurrency-gated admission layer for LLM inference flows. It bounds the number of simultaneously active flows, manages request admission under a chosen backpressure policy, and reports queue observability data. Each admitted request holds an admission slot that is guaranteed to be released when the request completes, fails, or is cancelled.

### Guarantees

- Concurrent active flows never exceed a configured maximum.
- Every admitted request is tracked for queue depth and wait-time observability.
- Admission slots are released on all exit paths, including client disconnect and abort.
- A completion-bias gate may be evaluated before permit grants, depending on configuration.

### Non-guarantees

- No ordering guarantee among admitted flows beyond FIFO queue-position reporting.
- No fairness guarantee beyond the maximum concurrency bound.

## Non-goals

The FIFO scheduler is a single-dimensional concurrency limiter, not a multi-criteria scheduler. It does not address load distribution across multiple serving backends, KV-cache pressure, per-flow priority, or starvation avoidance beyond the configured queue bounds.

### Out of scope

- Prioritization or weighted allocation of admission slots ([[?c_sched_priority]]).
- Cache-aware admission decisions ([[?c_sched_kv_admission]]).
- Multi-queue or weighted-fair scheduling ([[?c_sched_wfq]], [[?c_sched_drr]]).
- Starvation detection or mitigation ([[?c_sched_starvation]]).

## Interface

The scheduler exposes admission control, queue observation, and slot-holder contracts. Each surface defines preconditions on inputs, postconditions on outputs, and the failure modes a caller must handle.

### Admission

- Accepts a flow identity and an estimated work unit; produces an admission slot or a rejection. The work unit is stored for interface compatibility but is not used by FIFO for admission decisions.
- The caller must hold an admission slot for the duration of a request; disposal of the slot releases the concurrency permit.
- Depth-based rejection occurs only in FailFast mode, when queue depth exceeds the configured limit. Hybrid mode performs only timeout-based rejection when the maximum-wait expires. Blocking mode never rejects.
- Rejections carry a `Retry-After` hint based on queue depth measured at the moment of rejection.

### Queue Observation

- Reports the aggregate depth of waiting flows across all registered flows.
- Provides raw depth data to [[?c_flow_registry]] for construction of per-flow queue-position snapshots.

### Slot Holder

- The admission slot carries the flow identity and work-unit estimate for observability.
- Disposing the slot releases the concurrency permit, decrements the active-flows metric, decrements the per-flow active counter, and signals completion-bias waiters ([[?c_sched_completion_bias]]).
- A slot may be disarmed so that disposal becomes a no-op; disarming automatically releases the permit.

### Ticket Factory

- `make_ticket` constructs a `QueueTicket` from a flow identity, work-unit estimate, and a drop handler closure. The closure defines the actions performed when the ticket is dropped.
- Used by all scheduler variants ([[?c_sched_fifo]], [[?c_sched_wfq]], [[?c_sched_drr]]) for creating admission slots.

### Construction

- Requires a maximum active-flows bound, a backpressure policy ([[?c_sched_backpressure]]), a queue-depth limit, and a wait-time limit. Scheduling policies ([[?c_sched_lifecycle]]) may be supplied via the advanced constructor; the default constructor creates a disabled completion-bias gate.
- The configured maximum is fixed for the lifetime of the scheduler instance.

## Invariants

The scheduler maintains consistency between its internal concurrency bound, its observable queue metrics, and the lifecycle of admission slots.

### Concurrency Bound

- The number of simultaneously active flows never exceeds the configured maximum.
- Each admission acquires exactly one permit; each slot disposal releases exactly one.

### Queue Depth Consistency

- The reported queue depth equals the number of in-flight admission attempts (waiting for a permit, not yet admitted).
- Depth is incremented at entry to the admission path and decremented on admission, rejection, or cancellation.

### Metrics Alignment

- The `llm_active_flows` gauge equals the number of held admission slots.
- The `llm_queue_depth` gauge per flow equals that flow's depth contribution within the aggregate counter.
- Wait-time metrics are measured from the moment a request enters the waiting queue until it is admitted or rejected.

### FIFO Positioning

- Queue positions reflect creation order within each flow; positions are 1-indexed.

### Completion-Bias Enforcement

- A completion-bias gate ([[?c_sched_completion_bias]]) is checked before every permit grant when enabled. The default constructor creates a disabled gate; callers must supply an enabled gate via the advanced constructor.

## Constraints

The scheduler operates within the boundaries of its concurrency model and backpressure configuration.

### Backpressure Policy

- Exactly one backpressure mode (Blocking, FailFast, or Hybrid) is active per scheduler instance.
- Blocking mode may block indefinitely at both the completion-bias gate and the permit-wait.
- FailFast mode rejects immediately on queue-depth overflow; subsequent requests within depth limits proceed without timeout.
- Hybrid mode applies two sequential timeouts, each bounded by the configured maximum-wait: one for the completion-bias gate evaluation, one for the permit acquisition. Total worst-case wait can reach twice the configured timeout. Hybrid mode does not perform depth-based rejection.

### Concurrency Bound

- The maximum active flows value is immutable after construction; it cannot be increased or decreased without creating a new scheduler instance.

### Slot Release

- Disarming a slot automatically releases the concurrency permit; the caller does not retain permit-release responsibility after disarm.
- Double-release of a permit is prevented by the slot's internal guard.

### Queue Depth Independence

- Queue depth and semaphore capacity are measured independently; a rejection may occur even when permits are available, because depth exceeds its configured limit.

## Rationale

The FIFO scheduler separates concurrency control from admission decisions, enabling callers to choose backpressure behavior without coupling it to the slot-release mechanism.

### Why a Concurrency Gate

LLM inference is flow-intensive; bounding concurrent flows prevents resource saturation and ensures predictable queue behavior. A fixed concurrency cap provides a simple admission boundary that all scheduling algorithms share ([[?c_sched_facade]]).

### Why RAII Slot Release

Admission slots must be released on all exit paths (success, error, panic, client disconnect). RAII guarantees the release happens regardless of how the request ends, eliminating leak paths from error handling.

### Why Automatic Permit Release on Disarm

Disarm is used when the receiver is gone (timeout, abort) and the ticket will never be explicitly disposed. Automatic release ensures the permit cannot leak in this path; the caller has no reference to the permit and cannot release it manually.

### Why Optional Completion-Bias Gate

The completion-bias gate ([[?c_sched_completion_bias]]) favors admitting requests from flows that have recently completed work, reducing head-of-line blocking under contention. It is disabled by default as the scheduler can operate correctly without it.

### Why Three Backpressure Modes

Different callers need different pressure strategies: Blocking for resilience, FailFast for depth-bounded rapid rejection, and Hybrid for bounded waiting with timeout. Providing all three allows the scheduler to be embedded in diverse service topologies ([[?c_sched_backpressure]]).

## Related

- [[?c_sched_facade]] — Scheduler facade that selects among scheduling algorithms
- [[?c_sched_backpressure]] — Backpressure mode configuration and semantics
- [[?c_sched_completion_bias]] — Completion-bias gate mechanism
- [[?c_flow_registry]] — Flow tracking and snapshot construction
- [[?c_sched_flow_progress]] — Per-flow progress counters
- [[?c_flow_id]] — Flow identity model
- [[?c_metrics_families]] — Metric family registration
- [[src/scheduler/fifo.rs#FifoScheduler]] — Source implementation
- [[src/scheduler/mod.rs#FifoScheduler]] — Module re-export
