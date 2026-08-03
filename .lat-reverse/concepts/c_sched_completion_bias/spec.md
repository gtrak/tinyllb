# Completion Bias Gate — Spec

## Purpose

The completion bias gate controls admission of new flows into the scheduler by deferring admission when the number of active flows reaches a configured threshold. It guarantees eventual admission for every flow while preferencing completion of already-active work.

- Admits flows immediately when the active flow count is below the effective target.
- Defers admission of new flows when the active flow count meets or exceeds the effective target.
- Guarantees eventual admission through a starvation timeout, provided the flow carries a valid enqueued timestamp.
- Permits predictive admission of new flows when any active flow has completed at least ninety percent of its estimated token output.
- Allows flows that are already active to bypass the gate entirely.

## Non-goals

This concept does not address scheduling priorities, flow ordering, or throughput optimization beyond the completion-bias mechanism itself.

- Does not determine which waiting flow is admitted first when multiple flows are blocked.
- Does not enforce fairness among waiting flows beyond the starvation timeout fallback.
- Does not track or limit total queue depth or waiting flow count.
- Does not participate in flow selection, batching, or dispatch decisions.
- Does not manage the lifecycle of flows outside the admission gate.

## Interface

The gate exposes an admission surface that returns control to the caller when a flow is permitted to proceed. All contracts are stated in domain terms.

### Admission

- A caller presenting a flow receives admission either immediately or after an asynchronous wait.
- A new flow is admitted immediately when the active flow count is below the effective target.
- A new flow is admitted immediately when any active flow has delivered at least ninety percent of its estimated tokens.
- A flow that is already active bypasses the gate without evaluation.
- Admission is eventual: the gate never rejects a flow under normal operation; it only delays admission.

### Configuration

- The target active flow count determines the threshold at which new flows are deferred.
- When the target is zero, the maximum active flow count becomes the effective target for deferral decisions; only when both are zero does the gate never wait.
- The maximum active flow count provides a fallback threshold when the configured target is zero.
- An enabled flag controls whether the gate operates or bypasses all flows immediately.
- Starvation timeout defines the maximum duration a new flow waits before being force-admitted.
- Predictive admit toggle enables or disables early admission based on active flow completion progress.

### Preconditions

- Flows must carry an enqueued timestamp for starvation protection to activate; flows without one rely solely on active-count drops or predictive admit.

### Notification

- The gate wakes all blocked callers when the active flow count changes.
- Missed notifications are tolerated because the starvation timeout provides a fallback; callers re-evaluate admission on each wake.

## Invariants

These statements hold regardless of implementation. Every admissible rewrite must preserve them.

- An active flow never waits at the gate; admission is always immediate for active flows.
- When the gate is disabled, all flows pass through without evaluation.
- The effective target equals the configured target when the configured target is non-zero; when the configured target is zero, it equals the maximum active flow count.
- The predictive admit threshold is fixed at ninety percent of estimated tokens delivered.
- The gate never produces an error result under normal operation.

## Constraints

The gate operates within these boundaries and cannot guarantee correctness beyond them.

- Admission decisions may observe stale active flow counts due to concurrent updates between read and decision.
- A blocked flow relies on the starvation timeout as its fallback if notification is missed.
- Force admission of a starving flow is not coordinated with other blocked flows and may exceed the active flow target.
- A poisoned lock on the enqueued timestamp value causes a runtime failure that precludes admission.
- Flows lacking an enqueued timestamp have no starvation safety net and may wait indefinitely under sustained saturation.
- Predictive admit evaluates per-flow progress independently; partial completion of one flow can trigger admission of another.
- The starvation re-check interval is currently derived as one quarter of the starvation timeout; this ratio is an implementation detail, not a domain invariant.

## Rationale

The completion bias gate trades admission throughput for predictable per-flow latency. Limiting concurrent flows prevents resource contention, while the starvation and predictive mechanisms prevent the limit from becoming a hard bottleneck.

- Deferring new flows during saturation reduces tail latency for already-running work.
- Starvation timeout ensures no flow with a valid enqueued timestamp is indefinitely blocked under sustained saturation.
- Predictive admit amortizes the cost of the target limit by overlapping new work with near-completion flows.
- Active flows bypass the gate to avoid self-inflicted contention on their own continuation.
- Zero target defaults to the maximum active flow count, preserving operational bounds when no explicit target is configured.

## Related

- [[?c_flow_registry]] — Flow registration and lookup
- [[?c_flow_progress]] — Per-flow token delivery tracking
- [[?c_metrics]] — Scheduler metrics collection
- [[src/scheduler/completion_bias.rs#CompletionBiasGate]] — Gate implementation
- [[src/scheduler/flow_progress.rs#FlowProgressTracker]] — Progress tracking
- [[src/flow/mod.rs#Flow]] — Flow type
- [[src/metrics/mod.rs]] — Metrics infrastructure
