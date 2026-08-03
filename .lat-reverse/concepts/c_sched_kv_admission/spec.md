# KV-Admission Policy — Spec

## Purpose

The KV-admission policy governs whether incoming requests are admitted, delayed, or rejected based on current key-value cache pressure from the backend. It provides a single async gate that callers must pass before admitting a request, and it ensures the system does not overload a backend whose cache is approaching capacity.

- Admits requests when KV-cache usage is within normal operating range.
- Delays requests when usage exceeds a configured delay threshold, holding them until pressure subsides.
- Rejects requests with a `Retry-After` hint when usage exceeds a configured reject threshold.
- Exposes a count of delayed requests observable by queue-depth queries.
- Records admission decisions for metrics collection.

## Non-goals

This concept does not address scheduling fairness, ordering, or resource allocation beyond the admission gate.

- Does not decide which admitted request runs next — that is the domain of [[?c_sched_queue]].
- Does not manage KV-cache eviction or memory allocation — that is the domain of the backend runtime.
- Does not provide per-request or per-tenant admission control — decisions are based on aggregate cache pressure.
- Does not implement rate limiting independent of KV-cache state.

## Interface

The policy exposes one admission gate, a decision type, a configuration surface split across two structures, and one observable counter. Each surface makes a contractual guarantee about what the caller receives.

### Admission Gate

- The gate method returns a `Result` where success indicates admission and failure carries a backpressure rejection with a `Retry-After` duration.
- `Ok(())` is returned when the request is admitted directly or a delay wait completes within bounds.
- `Err(BackpressureRejected { retry_after })` is returned when the request must be back-pressured, carrying the backoff duration.
- When the policy is enabled, records the initial decision outcome to the metrics subsystem before returning.

### Decision Type (`KVMDecision`)

A public enum with three variants representing the initial admission outcome before any delay-wait resolution. Crate-internal public — any module within the crate can reference it directly.

| Variant | Meaning |
|---|---|
| `Accept` | KV pressure is at or below the delay threshold — proceed immediately. |
| `Delay` | KV pressure exceeds the delay threshold — request enters a delay wait. |
| `Reject(Duration)` | KV pressure exceeds the reject threshold — return rejection with the embedded duration as `Retry-After`. |

### Configuration

- The admission thresholds (`enabled`, `delay_threshold`, `reject_threshold`) are carried in a dedicated policy config: defaults are `enabled: false`, `reject_threshold: 0.95`, `delay_threshold: 0.80`.
- The wait behavior (`backpressure_mode`, `max_wait`, `retry_after_base`, `max_queue_depth`) is carried in a separate backpressure config and threaded into the policy at construction time.
- If disabled, the gate always admits — thresholds and modes are ignored.

### Delayed-Request Counter

- Returns the current number of requests held in a delay wait at the time of the query.
- Increments when a request enters a delay wait and decrements when it exits (by success, timeout, or cancellation).
- Approximate counts are acceptable due to relaxed memory ordering.

## Invariants

These statements hold regardless of implementation details. They survive a complete rewrite of the admission logic.

### Threshold Ordering

- The delay threshold must be strictly less than the reject threshold for the delay path to be reachable. This is a configuration requirement, not an enforced invariant — misconfigured thresholds may silently render the delay path unreachable.

### Delayed-Count Consistency

- The reported delayed count reflects the number of requests currently in a delay wait.
- The count decrements on every exit path from a delay wait — success, timeout, and cancellation are all covered.

### Safe Degradation

- When the backend monitor becomes unavailable, the gate admits by default rather than rejecting. This applies both when the monitor is unavailable at the initial check and when it closes during an active delay wait.

### Metrics Completeness

- When the policy is enabled, every admission decision produces exactly one metrics record with a label identifying the outcome. The disabled path bypasses all metrics recording. The metrics label corresponds to the initial decision, not to the final disposition of a delayed request that later times out.

## Constraints

Operational and configurational boundaries that shape the design space.

- Backpressure mode determines whether delay waits can be unbounded: blocking mode allows indefinite waits, while other modes enforce a timeout.
- `Retry-After` on reject-scale overflows follows the formula 5 s base + (excess × 10 s), where excess is the fraction of KV usage above the reject threshold. These constants are hardcoded.
- `Retry-After` on delay-timeout is computed from the delayed-request count relative to the configured maximum.
- The policy operates on a single aggregate KV-usage value — per-segment or per-key granularity is not available.
- At exactly the delay threshold value, the request is admitted (not delayed). At exactly the reject threshold value, the request is delayed (not rejected). Strict inequality governs both comparisons.
- Underflow may wrap silently if decrement operations exceed increment operations on the delayed count.

## Rationale

Admission gating is necessary because KV-cache exhaustion degrades all concurrent requests, not just the one that triggered overflow.

- Two-level thresholds (delay then reject) provide graceful degradation — mild pressure causes waits, severe pressure causes rejections.
- The monitor-closed fallback admits by default because a missing signal is less dangerous than wholesale rejection.
- `Retry-After` scales with excess pressure so callers receive proportional backoff rather than fixed delays.
- Counting delayed requests lets downstream observability correlate admission waits with queue depth.
- The policy is independently toggleable — disabling it restores unconditional admission for debugging or low-pressure environments.

## Related

- [[?c_backend_monitor]] — provides KV-cache usage snapshots consumed by admission decisions
- [[?c_metrics]] — receives admission decision records
- [[?c_backpressure]] — defines rejection semantics and `Retry-After` computation
- [[?c_sched_queue]] — consumes the delayed-count for queue-depth queries
- [[src/scheduler/kv_admission.rs]] — implementation
- [[src/scheduler/backpressure.rs]] — rejection types and retry-after computation
- [[src/config/mod.rs]] — policy configuration and backpressure config
