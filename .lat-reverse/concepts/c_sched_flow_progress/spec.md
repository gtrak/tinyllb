# Flow Progress Tracking

## Purpose

The system maintains aggregate progress state per flow so that the scheduler can determine whether a flow has nearly completed. Each flow is characterized by two quantities: estimated tokens (work expected) and delivered tokens (work completed). The tracker answers threshold-based queries that compare these quantities without exposing the raw aggregates.

- Tracks cumulative estimated and delivered tokens per flow across the set of active requests belonging to that flow.
- Answers whether a specific flow is near completion relative to a fractional threshold.
- Answers whether any tracked flow is near completion relative to a fractional threshold.
- Allows flows to enter and leave the tracker as requests are admitted and released.
- Preserves state across multiple incremental updates without requiring full recomputation.

## Non-goals

This concept does not manage scheduling decisions, request admission, or token budgets.

- Does not admit, schedule, or evict requests; only tracks progress for flows already admitted.
- Does not validate incoming values; trusts callers to supply accurate and consistent counts.
- Does not persist state beyond the lifetime of the scheduler.
- Does not produce metrics, diagnostics, or observability output.
- Does not coordinate access across scheduler instances; operates within a single scheduler.

## Interface

The tracker exposes operations to register flows, update progress, remove flows, and query near-completion status. Each operation is defined by its preconditions, postconditions, and error semantics.

### Register flow
- **Preconditions:** A flow identifier and an estimated token count for a newly admitted request.
- **Postconditions:** The flow exists in the tracker with the estimated count added to its aggregate.
- **Behavior:** Creates a new entry if the flow does not already exist; otherwise accumulates into the existing estimate.

### Update delivered tokens
- **Preconditions:** A flow identifier and a delta of delivered tokens.
- **Postconditions:** The flow's delivered aggregate is incremented by the delta.
- **Behavior:** If the flow has no entry, the delta is silently discarded and no entry is created.

### Unregister flow
- **Preconditions:** A flow identifier and the request's estimated and delivered token counts.
- **Postconditions:** Both counts are subtracted from the flow's aggregates; the entry is removed if both aggregates reach zero.
- **Behavior:** If the flow has no entry, the operation is a silent no-op. Subtraction saturates at zero; residual entries persist if subtraction does not reach zero on both aggregates.

### Query near-completion
- **Preconditions:** A flow identifier and a fractional threshold.
- **Postconditions:** Returns whether the delivered-to-estimated ratio meets or exceeds the threshold.
- **Behavior:** Returns false if the flow has no entry or its estimated aggregate is non-positive. The ratio reflects the raw arithmetic of delivered divided by estimated; negative delivered values produce negative ratios.

### Query any near-completion
- **Preconditions:** A fractional threshold.
- **Postconditions:** Returns whether at least one tracked flow satisfies the near-completion condition.
- **Behavior:** Returns false if the tracker contains no entries or no entry satisfies the threshold.

### Initialize
- **Preconditions:** None.
- **Postconditions:** An empty tracker with no tracked flows.

## Invariants

The tracker maintains consistency between registration, updates, and removal.

### Aggregate monotonicity
- Estimated tokens remain non-negative provided callers only supply non-negative values during registration.
- Delivered tokens may become negative when negative deltas are applied during updates; the tracker does not enforce a lower bound.
- Subtraction during unregister saturates at zero, so estimated and delivered cannot go negative from unregister alone.

### Entry lifecycle
- An entry is removed from the tracker if and only if both its estimated and delivered aggregates are zero after unregister.
- Entries persist across updates; removal requires an explicit unregister operation.

### Near-completion semantics
- A flow satisfies the near-completion condition when its delivered-to-estimated ratio meets or exceeds the threshold and the estimated aggregate is positive.
- A flow with no entry or a non-positive estimated aggregate never satisfies the near-completion condition.

### Update isolation
- Updating delivered tokens for an unknown flow does not create an entry.
- Each update affects only the targeted flow; no other flow's aggregates change.

## Constraints

The tracker is designed for a single scheduler instance and accepts external inputs without validation.

- Operates on a single scheduler; no cross-instance state or replication.
- Accepts arbitrary deltas for delivered tokens; negative deltas reduce the delivered aggregate without saturation.
- The near-completion threshold is expected in the zero-to-one range but is not enforced; callers may supply any threshold value.
- Aggregate subtraction in unregister saturates at zero; mismatched estimates and deliveries leave stale residue rather than producing negative values from unregister.
- Near-completion compares a floating-point ratio against the threshold; boundary precision varies with the threshold value and aggregate magnitudes.

## Rationale

Aggregate tracking replaces per-request bookkeeping and enables efficient threshold queries without scanning individual requests.

- Aggregating by flow rather than by request avoids per-request scans when evaluating near-completion across a batch.
- Saturating subtraction in unregister prevents negative aggregates from propagating through estimate mismatches, keeping the tracker in a valid state even when callers supply inconsistent values.
- Silent no-op on unknown flows in both update and unregister avoids spurious entries from late, duplicate, or out-of-order messages.
- Zero-removal keeps the tracker bounded by removing completed flows while retaining partial entries that represent in-flight requests.
- Floating-point thresholds provide a flexible completion signal; the caller controls sensitivity through the threshold parameter.

## Related

- `[[?c_sched_completion_bias]]` — completion bias gate that consumes near-completion signals
- `[[?c_sched_flow]]` — flow identity and grouping
- `[[?c_sched_admission]]` — request admission that registers and unregisters flows
- `[[src/scheduler/flow_progress.rs]]` — implementation of flow progress tracking
