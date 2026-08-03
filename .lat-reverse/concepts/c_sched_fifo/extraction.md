# FIFO Scheduler — Extraction

## Responsibilities

- Limits the number of concurrently active flows to a configured maximum (`max_active_flows`).
- Manages admission of incoming requests into the active set under one of three backpressure modes: Blocking, FailFast, or Hybrid.
- Tracks per-flow queue depth and waiting-queue position for observability.
- Applies a completion-bias gate to each admission attempt before granting the permit.
- Guarantees slot release via RAII (`QueueTicket::Drop`) on all exit paths (success, error, panic, client disconnect).
- Reports three metrics: `llm_queue_depth{flow_id=...}`, `llm_queue_wait_seconds`, `llm_active_flows`.

---

## Interface Surfaces

### `FifoScheduler::new` / `new_with_policies` — Constructor

- **Inputs**: `max_active_flows: u32`, `metrics: Arc<Metrics>`, `registry: Arc<FlowRegistry>`, `backpressure_mode: BackpressureMode`, `max_queue_depth: u32`, `max_wait: Duration`, `retry_after_base: Duration`. Constructor with policies additionally accepts `policies: Policies`.
- **Output**: A fully configured `FifoScheduler` instance.
- **Evidence**: `pub fn new` line 182; `pub(crate) fn new_with_policies` line 220.

### `FifoScheduler::admit(flow_id, work_unit) -> Result<QueueTicket, BackpressureRejected>` — Admission Gate

- **Inputs**: `flow_id: FlowId` identifies the flow; `work_unit: f64` is the estimated work for the request.
- **Output**: On success, a `QueueTicket` holding the admission slot. On backpressure, `Err(BackpressureRejected { retry_after })`.
- **Error contract**: `BackpressureRejected` is returned when:
  - FailFast mode and `queue_depth > max_queue_depth` (line 336).
  - Hybrid mode and `max_wait` expires before a permit is acquired (line 416).
  - `retry_after` is computed as `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)` (lines 338, 420).
- **Guarantees**:
  - The flow is created or retrieved atomically from the registry before any blocking (line 261).
  - A `DepthGuard` is created before the completion-bias check so that blocked flows are counted in `queue_depth()` (line 289, 298–299).
  - The semaphore is never closed; `acquire_owned().expect()` panics are treated as internal errors (lines 306, 362, 412).
- **Evidence**: `pub async fn admit` line 255.

### `FifoScheduler::queue_depth() -> u32` — Queue Depth Query

- **Input**: none.
- **Output**: Sum of per-flow depth counters across all registered flows.
- **Evidence**: `pub fn queue_depth` line 444.

### `FifoScheduler::queue_snapshot() -> QueueSnapshot` — Queue Snapshot

- **Input**: none.
- **Output**: `QueueSnapshot` containing active count, total waiting count, and per-flow 1-indexed queue positions.
- **Evidence**: `pub fn queue_snapshot` line 452.

### `QueueTicket` — Admission Slot Holder (RAII)

- **Fields (visible)**: `flow_id: FlowId`, `work_unit: f64`.
- **Guarantees on Drop**:
  - Releases the admission slot (semaphore permit).
  - Decrements `llm_active_flows` gauge.
  - Decrements per-flow active counter.
  - Notifies completion-bias waiters.
- **Evidence**: `pub struct QueueTicket` line 130; `impl Drop` line 476.

### `QueueTicket::disarm()` — Disarm Drop Handler

- **Input**: none.
- **Effect**: Removes the drop handler so that `Drop` becomes a no-op. Used when the receiver is gone (timeout/abort) to prevent double-release.
- **Evidence**: `pub fn disarm` line 471.

### `make_ticket(flow_id, work_unit, drop_handler) -> QueueTicket` — Ticket Factory

- **Inputs**: `flow_id: FlowId`, `work_unit: f64`, `drop_handler: impl FnOnce() + Send + 'static`.
- **Output**: A `QueueTicket` wrapping the given drop handler.
- **Evidence**: `pub fn make_ticket` line 497.

---

## Invariants

1. **Active flow count never exceeds `max_active_flows`** — enforced by the semaphore initialized to `max_active_flows` (line 205). Every `admit()` call acquires exactly one permit; every `QueueTicket::Drop` releases exactly one.

2. **Queue depth reflects actual in-flight admits** — incremented by `DepthGuard::new()` (line 38), decremented by `DepthGuard::consume()` (line 65) or `DepthGuard::drop()` (line 92). The `active` flag prevents double-decrement (lines 61–63, 88–91).

3. **`llm_queue_depth{flow_id=...}` gauge stays consistent with depth counter** — updated atomically with every depth change: `new()` line 39–42, `consume()` line 70–73, `drop()` line 97–100.

4. **Waiting queue is FIFO-ordered for position reporting** — `FlowId` appended in creation order (line 43), first-occurrence removed on consume (line 82) or cancellation (line 103).

5. **`enqueued_at` is set exactly while waiting, cleared on consume or cancellation** — set in `DepthGuard::new()` (line 47), cleared in `consume()` (line 78) and `drop()` (line 108).

6. **Active flows gauge == semaphore permits consumed** — `active_flows.inc()` called exactly once per successful admission (line 489 via `record_wait_and_active`), `active_flows.dec()` called exactly once per ticket drop (line 319/373/435).

7. **Completion-bias gate is always checked before permit grant** — checked after `DepthGuard` creation in all three admit paths: `admit_blocking` line 299, `admit_fail_fast` line 355, `admit_hybrid` line 400.

8. **Hybrid mode uses biased select to prefer permit acquisition over timeout** — `biased` keyword in `tokio::select!` (line 408) ensures the acquire branch wins if both are ready simultaneously, preventing spurious rejection.

9. **Per-flow active counter tracks admission state** — `flow.inc_active()` on admission (line 311/366/428), `flow.dec_active()` on ticket drop (line 320/374/436).

10. **Retry-After header reflects current queue depth at rejection time** — computed with the depth value measured at the moment of rejection (lines 335–338 for FailFast, lines 419–422 for Hybrid timeout).

---

## Failure Modes

1. **Semaphore panic on close** — If the semaphore were ever closed (noted as impossible per comment), `acquire_owned().expect()` would panic (lines 306, 362, 412). This is an internal invariant violation, not a caller-facing error.

2. **Mutex poisoning on waiting queue** — `waiting_queue.lock().unwrap()` will panic if any thread panics while holding the lock (lines 43, 115, 142, 456). Same for `enqueued_at.write().unwrap()` (lines 46, 77, 107).

3. **Depth underflow protection** — `fetch_sub(1, Ordering::Relaxed).saturating_sub(1)` in both `consume()` (line 69) and `drop()` (line 96) uses saturating subtraction to prevent depth from going negative if the guard is dropped twice (mitigated by `active` flag).

4. **Fail-fast rejects even with available permits** — In FailFast mode, the depth check (line 336) can reject a request even though a semaphore permit may be available, because queue depth and semaphore capacity are measured independently.

5. **Completion-bias gate blocks indefinitely in Blocking mode** — `completion_bias_gate.check()` in blocking mode has no timeout (line 299), so a flow can be blocked at the gate indefinitely. In Hybrid mode the check is subject to `max_wait` timeout (line 400).

6. **Ticket disarm leaves permit unreleased** — Calling `disarm()` (line 471) removes the drop handler, so the semaphore permit is never released by the ticket. The caller must release it manually; failure to do so leaks a permit.
