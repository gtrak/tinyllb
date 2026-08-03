# Flow Progress Tracking — Extraction

## Responsibilities

- Aggregate per-flow estimated and delivered token counts across active requests.
- Provide threshold-based queries to determine whether a flow is near completion.
- Maintain thread-safe mutable state over the lifetime of the scheduler.

## Interface Surfaces

### `FlowProgressTracker::new()`

- **Inputs:** None.
- **Output:** Empty `FlowProgressTracker` with zero tracked flows.
- **Evidence:** Lines 28–34.

### `FlowProgressTracker::register(flow_id, estimated: i64)`

- **Inputs:** Flow identifier, estimated token count for a newly admitted request.
- **Output:** None (mutates internal state).
- **Contract:** Adds `estimated` to the flow's aggregate. Creates the flow entry if it does not exist.
- **Evidence:** Lines 38–48.

### `FlowProgressTracker::update_delivered(flow_id, delta: i64)`

- **Inputs:** Flow identifier, positive token delta from a delivered usage frame.
- **Output:** None (mutates internal state).
- **Contract:** Increments the flow's delivered aggregate by `delta`. Silent no-op if the flow has no entry.
- **Evidence:** Lines 52–59.

### `FlowProgressTracker::unregister(flow_id, estimated: i64, delivered: i64)`

- **Inputs:** Flow identifier, the request's estimated and delivered token counts.
- **Output:** None (mutates internal state).
- **Contract:** Subtracts both values from the flow's aggregates. Removes the entry entirely if both aggregates reach zero.
- **Evidence:** Lines 63–78.

### `FlowProgressTracker::is_near_done(flow_id, threshold: f64) -> bool`

- **Inputs:** Flow identifier, fractional threshold (0..1 range expected).
- **Output:** `true` if `delivered >= threshold * estimated` and `estimated > 0`; `false` otherwise.
- **Contract:** Returns `false` when the flow has no entry or `estimated <= 0`.
- **Evidence:** Lines 82–98.

### `FlowProgressTracker::any_flow_near_done(threshold: f64) -> bool`

- **Inputs:** Fractional threshold (0..1 range expected).
- **Output:** `true` if any tracked flow satisfies `delivered >= threshold * estimated` with `estimated > 0`.
- **Contract:** Iterates all entries; short-circuits on first match. Returns `false` if no entry qualifies or the tracker is empty.
- **Evidence:** Lines 102–117.

### `Default for FlowProgressTracker`

- **Contract:** `default()` delegates to `new()`.
- **Evidence:** Lines 119–123.

## Invariants

- **Non-negative aggregates:** Subtraction in `unregister` uses `saturating_sub`, guaranteeing `estimated >= 0` and `delivered >= 0` on every entry (lines 68–69).
- **Zero-removal:** An entry is removed from the tracker iff both `estimated == 0` and `delivered == 0` (line 70).
- **Near-done ratio:** The near-done check computes `delivered / estimated` as `f64`; `estimated <= 0` always yields `false` (lines 89–94, 108–113).
- **Silent-miss on update:** `update_delivered` is a no-op if the flow entry does not exist; no entry is created (lines 56–58).

## Failure Modes

- **Orphaned entry:** If `unregister` is never called for a request, the flow entry persists indefinitely with stale aggregates.
- **Negative delta injection:** `update_delivered` accepts negative `delta`, which can reduce `delivered` below the true count (line 55).
- **Estimate mismatch:** If `register` is called with an estimate different from the value passed to `unregister`, `saturating_sub` clamps at zero rather than correcting, leaving stale residue.
- **Float precision:** The `f64` ratio in near-done checks may produce boundary discrepancies around the threshold (lines 90, 109).
- **Update-before-register:** Calling `update_delivered` for a flow that has no entry silently discards the delta; the aggregate becomes inaccurate.
