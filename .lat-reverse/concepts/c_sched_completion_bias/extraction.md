# Completion Bias Gate — Extraction

Source: `src/scheduler/completion_bias.rs`

## Responsibilities

- Defers admission of requests for new flows when `active_flows >= target_active_flows`.
- Allows already-active flows to bypass the gate.
- Enforces starvation protection by force-admitting flows that exceed a timeout.
- Evaluates predictive admit: allows new flows when any active flow is near completion.
- Notifies waiting requests when active flow count changes.

## Interfaces

### `CompletionBiasGate::new` (line 56–83)

| Aspect | Contract |
|---|---|
| **Inputs** | `enabled: bool`, `target_active_flows: u32`, `predictive_admit: bool`, `max_active_flows: u32`, `metrics: Arc<Metrics>`, `registry: Arc<FlowRegistry>`, `notify: Arc<tokio::sync::Notify>`, `starvation_timeout: Duration`, `flow_progress: Arc<FlowProgressTracker>` |
| **Outputs** | `CompletionBiasGate` |
| **Behavior** | When `target_active_flows == 0`, uses `max_active_flows` as effective target (lines 67–71). Derives `starvation_check_interval` as `starvation_timeout / 4` (line 79). |

### `CompletionBiasGate::check` (line 96–155)

| Aspect | Contract |
|---|---|
| **Inputs** | `flow: &Arc<Flow>` |
| **Outputs** | `async` — no return value; completion means admission is allowed |
| **Immediate-admit conditions** | Returns without waiting when any of these hold: (1) gate is disabled (line 98), (2) flow is already active `flow.is_active()` (line 103), (3) `target_active_flows == 0` (line 109), (4) `active < target_active_flows` (line 115), (5) predictive admit: any active flow delivered >= 90% estimated tokens (lines 120–127) |
| **Wait loop** | When none of the above hold, enters a loop (line 130) that: (a) checks starvation via `maybe_force_admit` (line 132), (b) re-checkes predictive admit (lines 137–143), (c) awaits notification with `starvation_check_interval` timeout (line 147), (d) re-reads `active_flows` and exits if below target (lines 150–153) |

### `CompletionBiasGate::notify_waiters` (line 176–178)

| Aspect | Contract |
|---|---|
| **Inputs** | None |
| **Outputs** | None |
| **Behavior** | Calls `self.notify.notify_waiters()` to wake all waiters blocked in `check` |

## Invariants

- `starvation_check_interval == starvation_timeout / 4` (line 79).
- `effective_target == max_active_flows` when `target_active_flows` input is `0` (lines 67–71).
- `PREDICTIVE_ADMIT_THRESHOLD == 0.9` (line 26); used for `any_flow_near_done` check.
- Active flows (`flow.is_active() == true`) never wait at the gate (line 103–105).
- When `target_active_flows == 0`, the gate never waits (line 109–111).
- The gate never returns an error; admission is eventual (either immediate or after wait/starvation).

## Failure Modes

- **Starvation timeout exceeded**: Flow is force-admitted; `flow_starvation_seconds` metric is set and `starvation_force_admits_total` is incremented (lines 163–169).
- **Concurrency hazard**: `maybe_force_admit` reads `flow.enqueued_at` via `unwrap()` on a `RwLockReadGuard` — panics if the write lock is poisoned (line 160).
- **Notification starvation**: If `notify_waiters()` is never called after an active flow completes, waiters block until `starvation_check_interval` timeout fires and starvation check triggers force admit.
- **Metric read race**: `active_flows.get()` (lines 114, 150) is read outside any lock; the value may change between read and decision.

## Dependencies

- `crate::flow::{Flow, FlowRegistry}`
- `crate::metrics::Metrics`
- `crate::scheduler::flow_progress::FlowProgressTracker`
- `tokio::sync::Notify`
- `tokio::time::timeout`
