# Starvation Detection (Spec)

## Purpose

Starvation detection determines when a flow has exceeded an acceptable wait time while queued, enabling a scheduler to bypass normal selection policy and force-admit the flow. It also emits observability signals so that starvation events can be monitored and thresholds tuned.

- Guarantees that no flow waits indefinitely behind flows favored by a scheduling policy
- Supplies a starvation decision based on elapsed queue time versus a timeout threshold
- Records the observed wait duration and increments a global starvation counter when force-admission occurs

## Non-goals

Starvation detection does not determine what threshold to use, enforce admission, or manage queue ordering.

- Does not define starvation thresholds — each consumer supplies its own timeout duration
- Does not enforce admission — it only reports whether a flow qualifies for force-admission
- Does not deduplicate starvation logic across different code paths; multiple independent implementations exist
- Does not operate on flows that have not yet been enqueued
- Does not protect against lock poisoning or clock regression — these are accepted panic conditions

## Interface

The concept manifests in three observable code paths: a paired check-and-record operation used by fair-share schedulers, a standalone combined operation in the completion bias gate, and a standalone metrics function called after an independent check.

### Starvation determination

- Accepts a flow and a timeout duration; returns the observed wait when the flow has exceeded the threshold, or nothing when the flow is not starved
- A flow without a recorded enqueue instant is never starved
- Equality with the threshold does not constitute starvation — the wait must strictly exceed the timeout
- Measured against monotonic time, not wall clock
- Returns the actual wait duration, enabling the caller to record metrics

### Metrics recording

- Accepts a metrics handle, a flow, and the observed wait duration as preconditions; all three must be supplied by the caller
- Records the wait as a gauge keyed by flow identity and increments a global starvation force-admit counter
- Side-effect only — produces no return value usable by the caller
- Carries no declared error contract; depends on the supplied metrics handle remaining valid

### Completion bias gate check

- Independently determines starvation and records metrics in a single operation
- Uses an internal timeout duration rather than accepting a caller-supplied threshold
- Returns a binary decision — starved or not — rather than exposing the wait duration
- Emits the same two metrics (flow-starvation gauge, force-admit counter) as the paired operation
- Serves a different access pattern: the gate combines check-and-record inline rather than separating them

## Invariants

These conditions hold across all code paths regardless of implementation.

- A flow without a recorded enqueue instant never satisfies the starvation predicate
- Starvation requires the wait to strictly exceed the threshold, not merely reach it
- Wait duration is always derived from a monotonic clock, making it immune to wall-clock adjustments
- The starvation decision is stateless — it depends only on the current instant, the recorded enqueue time, and the applicable threshold
- All code paths emit the same two metrics: per-flow starvation duration gauge and global force-admit counter

## Constraints

Boundaries within which starvation detection must operate.

- Accessing the enqueue instant may panic if concurrent access corrupts the flow's synchronization state
- If the monotonic clock reports a time before the recorded enqueue instant, duration computation will panic — no guard against clock regression exists
- The timeout threshold is caller-supplied; no code path validates or constrains the range of acceptable values
- The panic surfaces above apply to all three code paths, not only the starvation module's public functions

## Rationale

Starvation detection exists to prevent fairness regressions in schedulers that can, under certain load patterns, indefinitely defer certain flows.

- Without a starvation safety net, priority-based schedulers can starve lower-priority flows under sustained high-throughput conditions
- A per-call threshold allows each consumer to tune its own starvation tolerance without hard-coding a single policy
- Metric emission enables operators to observe starvation frequency and tune thresholds reactively
- The completion bias gate's inline implementation reflects a different access pattern where combining check and record into one operation is more efficient than the two-step pattern used by fair-share schedulers

## Related

- [[?c_flow]] — flow lifecycle and enqueue semantics
- [[?c_scheduler]] — scheduler selection policies that consume starvation signals
- [[src/scheduler/starvation.rs]] — starvation check and metrics recording
- [[src/scheduler/drr.rs]] — DRR scheduler invocation of starvation check
- [[src/scheduler/wfq.rs]] — WFQ scheduler invocation of starvation check
- [[src/scheduler/completion_bias.rs]] — independent completion bias gate starvation path
- [[src/flow/mod.rs#Flow]] — flow struct with enqueue instant field
