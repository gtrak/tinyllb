# Extraction: Scheduler Lifecycle Guard

Source: `src/scheduler/lifecycle.rs`

## Responsibilities

- Tracks whether a request stream completed normally or was aborted (client disconnect, timeout, explicit cancel)
- Accumulates delivered token counts from usage-frame parsing
- On drop, emits one of two lifecycle events (`request_completed` or `request_cancelled`) to metrics
- On drop, reports accounting to the scheduler so DRR credit reflects actual delivered work
- Registers/unregisters the flow with the progress tracker (if provided) for predictive admit
- Emits `request_started` event at construction time
- Emits per-token `token_received` events when `record_token` is called

## Interface Surfaces

### `LifecycleGuard::new` — Construction

- **Inputs**: flow identifier, estimated cost (i64), shared scheduler reference, shared metrics reference, optional flow progress tracker
- **Postconditions**: `request_started` event is emitted to metrics; flow is registered in the progress tracker if provided; `completed_normally` is initialized to false; `delivered_tokens` is initialized to 0
- **Code evidence**: Lines 71–97

### `LifecycleGuard::record_token` — Token Event Recording

- **Inputs**: none (operates on self)
- **Postconditions**: `token_received` event counter is incremented by one
- **Code evidence**: Lines 100–105

### `LifecycleGuard::add_delivered_tokens` — Delivered Token Count Update

- **Inputs**: additive token count (i64)
- **Postconditions**: cumulative `delivered_tokens` increases by the given count; flow progress tracker is updated with the same count if present
- **Code evidence**: Lines 112–118

### `LifecycleGuard::mark_completed` — Normal Completion Flag

- **Inputs**: none (operates on self)
- **Postconditions**: `completed_normally` is set to true, signaling normal completion to the Drop handler
- **Code evidence**: Lines 125–127

### `LifecycleGuard::drop` — Lifecycle Termination (RAII)

- **Inputs**: none (automatic on scope exit)
- **Behavior when `completed_normally == true`**:
  - Emits `request_completed` event to metrics (line 142–145)
  - If `delivered_tokens > 0`: reports `AccountingReport::Completed` with `delivered_tokens` and `restore_cost = estimated - delivered` (lines 152–158)
  - If `delivered_tokens == 0`: reports `AccountingReport::Completed` with `delivered_tokens = estimated_cost` and `restore_cost = 0` (lines 177–183)
  - If `delivered_tokens > estimated_cost` (overrun): logs a warning; restore becomes negative (additional debit) (lines 159–167)
- **Behavior when `completed_normally == false`** (cancel):
  - Emits `request_cancelled` event to metrics (lines 188–191)
  - Reports `AccountingReport::Cancelled` with `restore_cost = estimated.saturating_sub(delivered)` (lines 195–201)
  - `saturating_sub` prevents underflow if `delivered > estimated` (should not occur in normal cancel paths)
- **Always**: Unregisters flow from progress tracker if present (lines 136–138)
- **Code evidence**: Lines 130–203

### `AccountingReport` — Completion Accounting Enum

- **Public enum** with two variants (lines 209–221)
- `Completed { delivered_tokens: i64, restore_cost: i64 }` — used when the stream finished normally
- `Cancelled { restore_cost: i64 }` — used when the stream was aborted
- Routed to `Scheduler::report_accounting` which forwards to DRR-specific handler (FIFO/WFQ ignore)

### `event` Module — Lifecycle Event Constants

- `REQUEST_STARTED`, `TOKEN_RECEIVED`, `REQUEST_COMPLETED`, `REQUEST_CANCELLED` (lines 35–38)
- Used as labels for `metrics.request_events_total` histogram

### `Scheduler::report_accounting` — Accounting Submission

- **Signature**: `(&self, &FlowId, AccountingReport)` — public on Scheduler (line 327 of `src/scheduler/mod.rs`)
- Dispatches to DRR handler only; FIFO/WFQ are no-ops (line 331)

## Invariants

- `I1` (line 58): `completed_normally` is false at construction; only `mark_completed` sets it to true. Once true, it never reverts.
- `I2` (line 61): `delivered_tokens` is zero at construction; only `add_delivered_tokens` increments it. Value is monotonically non-decreasing across calls.
- `I3` (line 151): On normal completion with usage data, `restore_cost = estimated_cost - delivered_tokens`. Net DRR charge equals `-delivered_tokens` (i.e., credit reflects actual work).
- `I4` (line 180): On normal completion without usage data, full `estimated_cost` is charged with no restore. Credit is not adjusted.
- `I5` (line 195): On cancellation, `restore_cost = estimated_cost.saturating_sub(delivered)`. Net DRR charge equals `-delivered_tokens`. If `delivered > estimated`, saturating subtraction yields 0 (no restore), capping net charge at `-estimated`.
- `I6` (line 136–138): Flow is always unregistered from the progress tracker on drop, regardless of completion status.

## Failure Modes

- **Overrun** (`delivered > estimated` on normal completion, lines 159–167): `restore_cost` becomes negative, applying an additional debit. Logged as a warning. Indicates the backend generated more tokens than the max_tokens estimate.
- **No usage data** (lines 170–183): `delivered_tokens` is 0 at drop. Full `estimated_cost` is charged with no restore. Logged as a warning. Occurs when the backend response lacks usage frames.
- **Over-delivered cancel** (line 195): If `delivered > estimated` on cancel (should not happen), `saturating_sub` silently clamps `restore_cost` to 0 instead of negative, losing precision on the over-delivery case.
- **Guard not dropped** (theoretical): If the `LifecycleGuard` value is leaked (never dropped), no completion or cancel event fires, no accounting is reported, and the flow remains registered in the progress tracker indefinitely.
- **Double `mark_completed`** (benign): Calling `mark_completed` multiple times is idempotent — the Cell is just set to true again.
