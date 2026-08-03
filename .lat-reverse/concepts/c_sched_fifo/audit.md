# FIFO Scheduler — Audit (Cycle 3)

## Scope

- **Spec**: `.lat-reverse/concepts/c_sched_fifo/spec.md`
- **Implementation**: `src/scheduler/fifo.rs`
- **Related**: `src/scheduler/mod.rs`, `src/scheduler/backpressure.rs`, `src/scheduler/completion_bias.rs`, `src/flow/mod.rs`

## "No How" Lint

The spec passes the "No How" constraint. All sections use domain concepts
(admission slot, concurrency permit, queue depth, completion-bias gate, backpressure
policy) without referencing function names, internal data structures, or step-by-step
control flow. The Slot Holder section lists specific metric decrements and signals as
interface guarantees, which is acceptable — it describes *what* disposal accomplishes,
not *how* it does so.

## Findings

### 1. undocumented_behavior: Metrics inconsistency on disarm

**Spec claim** (Interface → Slot Holder):

> Disposing the slot releases the concurrency permit, decrements the active-flows
> metric, decrements the per-flow active counter, and signals completion-bias
> waiters.
> A slot may be disarmed so that disposal becomes a no-op; disarming automatically
> releases the permit.

**Spec claim** (Constraints → Slot Release):

> Disarming a slot automatically releases the concurrency permit; the caller does not
> retain permit-release responsibility after disarm.

**Implementation**: `QueueTicket::disarm()` calls `self.drop_handler.take()`, which
moves the `Option` out and immediately drops it. Dropping the `Some(Box<dyn FnOnce>)`
drops the closure's captured environment, which includes the `OwnedSemaphorePermit`.
Thus the permit IS released. However, the closure body never runs, so
`metrics.active_flows.dec()`, `flow_clone.dec_active()`, and `gate.notify_waiters()`
are NOT executed.

**Discrepancy**: The spec states disarm "automatically releases the permit" which
matches implementation. But the spec is silent on whether `active_flows` is
decremented, per-flow active counter is decremented, or completion-bias waiters are
notified after disarm. The implementation does NOT perform any of these metric
updates on disarm, leaving `active_flows` permanently over-counted by 1 and the
per-flow active counter similarly stale.

**Classification**: `undocumented_behavior`

---

### 2. undocumented_behavior: Flow auto-registration on admit

**Spec claim** (Interface → Admission):

> Accepts a flow identity and an estimated work unit; produces an admission slot or
> a rejection.

**Implementation**: `admit()` calls `self.registry.get_or_create(flow_id.clone())`
before any backpressure check. This atomically creates the flow in the registry with
default weight and priority if it does not already exist — even before the request is
accepted, rejected, or timed out.

**Discrepancy**: The spec does not state that calling `admit()` has the side effect
of creating the flow in the registry if it does not exist. This auto-registration
occurs on every call to `admit()`, including rejections, meaning a rejected request
still creates a registry entry with default weight/priority.

**Classification**: `undocumented_behavior`

---

### 3. missing_interface: `retry_after_base` not in Construction contract

**Spec claim** (Interface → Construction):

> Requires a maximum active-flows bound, a backpressure policy
> ([[?c_sched_backpressure]]), a queue-depth limit, and a wait-time limit.

**Implementation**: Both `FifoScheduler::new()` and `FifoScheduler::new_with_policies()`
accept a `retry_after_base: Duration` parameter that is used to compute the
`Retry-After` hint via `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)`.

**Discrepancy**: The Construction interface surface lists four required parameters
but omits `retry_after_base`. Callers using either constructor must supply it, and
its value directly affects rejection hints returned to clients. The formula itself
(`retry_after_base * (1 + depth / max_queue_depth)`) is also not documented in the
spec.

**Classification**: `missing_interface`

---

### 4. missing_interface: Advanced constructor visibility

**Spec claim** (Interface → Construction):

> Scheduling policies ([[?c_sched_lifecycle]]) may be supplied via the advanced
> constructor; the default constructor creates a disabled completion-bias gate.
> callers must supply an enabled gate via the advanced constructor.

**Implementation**: `FifoScheduler::new_with_policies()` is `pub(crate)`, not `pub`.
It is inaccessible to external crates. The only public constructor is
`FifoScheduler::new()`, which always creates a disabled gate.

**Discrepancy**: The spec instructs callers to use an advanced constructor for
enabled completion-bias, but the advanced constructor is not part of the public API.
External code cannot create a `FifoScheduler` with an enabled completion-bias gate
directly. (The `Scheduler` facade in `mod.rs` provides this path, but the spec
documents `FifoScheduler` directly, not the facade.)

**Classification**: `missing_interface`

---

### 5. undocumented_behavior: `enqueued_at` tracking

**Implementation**: `DepthGuard::new` writes `Instant::now()` into
`flow.enqueued_at`. `DepthGuard::consume` and `DepthGuard::drop` reset it to
`None`. The completion-bias gate reads this value for starvation detection
(`maybe_force_admit` compares wait time against `starvation_timeout`).

**Discrepancy**: The spec does not document that admission attempts set a
timestamp on the flow, nor that this timestamp is used for starvation detection
by the completion-bias gate. The `enqueued_at` field is a cross-cutting concern
between the FIFO scheduler (which sets it via DepthGuard) and the completion-bias
gate (which reads it). Neither the Interface nor Invariants sections describe this
coupling.

**Classification**: `undocumented_behavior`

---

## Summary

| # | Classification | Severity | Status |
|---|---|---|---|
| 1 | undocumented_behavior | Medium | Metrics not decremented on disarm; spec silent |
| 2 | undocumented_behavior | Medium | Auto-registration of flows on admit (even for rejections) |
| 3 | missing_interface | Medium | `retry_after_base` parameter and hint formula absent from spec |
| 4 | missing_interface | Low | Advanced constructor is `pub(crate)`; spec claims caller accessibility |
| 5 | undocumented_behavior | Low | `enqueued_at` timestamp coupling between DepthGuard and completion-bias |

**Prior issues from previous audits**: Both findings from earlier cycles (Hybrid depth-rejection
spec_error and disarm behavior bug) have been corrected in the current spec. The
spec now accurately states that Hybrid does not perform depth-based rejection
and that disarm automatically releases the permit. These items are closed.
