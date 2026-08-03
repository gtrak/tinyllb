# DRR Scheduler — Extraction

Source: `src/scheduler/drr.rs`

## Responsibilities

- Admits requests into an active set bounded by `max_active_flows` permits.
- Selects flows from waiting queues using starvation bypass, priority, and deficit round-robin (DRR) credit.
- Returns `QueueTicket` to admitted callers; ticket drop releases the permit and notifies the admission loop.
- Exposes three backpressure modes governing admission wait/reject behavior.
- Reports queue depth, queue snapshot, and per-flow credit balance.
- Restores flow credit on completion or cancellation via accounting reports.

## Interface Surfaces

### `DrrScheduler::new()` — Construction

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn new(max_active_flows: u32, metrics: Arc<Metrics>, registry: Arc<FlowRegistry>, backpressure_mode: BackpressureMode, max_queue_depth: u32, max_wait: Duration, retry_after_base: Duration) -> Self` |
| Line | 104-137 |
| Postcondition | Spawns a background admission loop; initializes `available_permits` to `max_active_flows`; creates a `CompletionBiasGate` with `predictive_admit=false` and 300s starvation timeout |
| Code evidence | `available_permits: max_active_flows` (line 182); `tokio::spawn(Self::admission_loop(...))` (line 194); `CompletionBiasGate::new(false, 0, false, ...)` (lines 115-125) |

### `DrrScheduler::new_with_policies()` — Construction with policy hooks

| Aspect | Evidence |
|--------|----------|
| Signature | `pub(crate) fn new_with_policies(..., starvation_timeout: Duration, policies: Policies) -> Self` |
| Line | 141-163 |
| Visibility | `pub(crate)` — internal-only constructor |
| Postcondition | Delegates to `new_inner` with caller-provided `CompletionBiasGate` from `policies.completion_bias` and caller-provided `starvation_timeout` |

### `admit()` — Request admission

| Aspect | Evidence |
|--------|----------|
| Signature | `pub async fn admit(&self, flow_id: FlowId, work_unit: f64) -> Result<QueueTicket, BackpressureRejected>` |
| Line | 512-536 |
| Input: `flow_id` | Flow identifier; guarantees flow exists via `registry.get_or_create` (line 518) |
| Input: `work_unit` | Work unit estimate (f64); used for cost deduction and credit accounting |
| Output: `Ok(QueueTicket)` | Caller holds a ticket; flow is counted active; permit consumed; credit debited by `work_unit` |
| Output: `Err(BackpressureRejected)` | Request rejected; contains `retry_after` duration |
| Error contract | Behavior is dispatched by `backpressure_mode` (lines 522-535) |

#### Blocking mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Waits indefinitely for admission; on channel close, returns `BackpressureRejected { retry_after: 1s }` |
| Lines | 538-602 |
| Completion bias | `completion_bias_gate.check(&flow)` called after guard creation; may block indefinitely |
| Postcondition on success | Records `queue_wait_seconds` metric; increments `active_flows`; increments flow active count (lines 596-600) |

#### Fail-fast mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Rejects immediately if `queue_depth() > max_queue_depth`; otherwise delegates to blocking |
| Lines | 604-621 |
| Error: `retry_after` | Computed by `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)` (line 616) |

#### Hybrid mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Races ticket receipt against `max_wait` timeout; timeout yields rejection with computed `retry_after` |
| Lines | 623-706 |
| Gate timeout | `completion_bias_gate.check()` bounded by `max_wait` (lines 643-645) |
| Retry-after on timeout | Computed from current `queue_depth()` vs `max_queue_depth` (lines 699-700) |

### `queue_depth()` — Queue depth query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn queue_depth(&self) -> u32` |
| Line | 709-711 |
| Postcondition | Returns `registry.sum_depths()` — sum of per-flow depth counters |

### `queue_snapshot()` — Queue state query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn queue_snapshot(&self) -> QueueSnapshot` |
| Lines | 714-722 |
| Postcondition | Returns `QueueSnapshot` with active count, waiting count, and ordered list of waiting flow IDs |
| Code evidence | `metrics.active_flows.get()` (line 715); `registry.queue_snapshot(...)` (line 721) |

### `credit()` — Per-flow credit query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn credit(&self, flow_id: &FlowId) -> i64` |
| Lines | 725-728 |
| Postcondition | Returns `flow.credit` as `i64` via `Ordering::Relaxed`; guarantees flow existence via `get_or_create` |

### `report_accounting()` — Completion/cancellation accounting

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn report_accounting(&self, flow_id: &FlowId, report: AccountingReport)` |
| Lines | 734-762 |
| Input: `report` | Either `Completed { restore_cost }` or `Cancelled { restore_cost }` |
| Postcondition | Adds `restore_cost` to `flow.credit`; updates `flow_credit` metric |
| Code evidence | `flow.credit.store(current + restore_cost, ...)` (lines 745, 755) |

## Invariants

### Selection ordering

- **Starvation bypass**: Flows waiting longer than `starvation_timeout` are force-selected before normal priority/credit rules. Evidence: lines 303-360 (starvation phase), `starvation::is_starved(&flow, starvation_timeout)` (line 313).
- **Priority dominates RR among eligible**: Among non-starved flows whose deficit credit >= cost, highest priority is selected first. Evidence: `priority::select_best(&candidates)` (line 430).
- **Round-robin tiebreak**: When priority ties, the flow appearing earlier in the RR cursor wins. Evidence: `base_score: *idx as f64` (line 425) passed to `FlowCandidate`.

### Credit accounting

- **Deficit vs permanent credit separation**: DRR eligibility uses `deficit` map (cleared on selection); permanent accounting uses `flow.credit` (debited at selection, restored on cancel/completion). Evidence: `s.deficit.entry(flow_id.clone())` (line 396); `s.deficit.remove(&selected_flow_id)` (line 447); `flow.credit.store(current + restore_cost)` (lines 745, 755).
- **Credit is not zeroed on queue drain**: When a flow's queue empties, its permanent `flow.credit` is preserved; only `deficit` is cleared. Evidence: comment at lines 337-340; `s.deficit.remove(&selected_flow_id)` (line 447); no `flow.credit.store(0)` anywhere.
- **Credit debited at selection**: On any selection (starvation force-select or normal), `work_unit as i64` is subtracted from `flow.credit`. Evidence: `flow.credit.store(new_credit)` where `new_credit = load - (work_unit as i64)` (lines 327-328, 450-451).
- **Credit restored on accounting**: `report_accounting` adds `restore_cost` to `flow.credit` for both completion and cancellation. Evidence: `current + restore_cost` (lines 744, 754).

### Permit accounting

- **Permit pool bound**: `available_permits` starts at `max_active_flows`, decrements on selection, increments on ticket drop. The sum of active flows + available_permits equals `max_active_flows`. Evidence: `available_permits: max_active_flows` (line 182); `s.available_permits -= 1` (lines 355, 484); `s.available_permits += 1` (lines 259, 271).
- **Zero-permit stall**: When `available_permits == 0`, `try_select` returns `(None, false)` immediately. Evidence: lines 294-296.

### Admission guard cleanup

- **Depth consistency**: `DrrAdmitGuard` increments flow depth on creation and decrements it on both success (`consume()`) and cancellation (`drop`). Guard is never double-consumed via `active` flag. Evidence: `flow.depth.fetch_add(1)` (line 788); `flow.depth.fetch_sub(1)` (lines 814, 834); `if !self.active { return; }` (lines 809, 828).
- **Waiting queue consistency**: Guard adds flow to `waiting_queue` on creation and removes it on both consume and drop. Evidence: `push_back(flow_id)` (line 791); `remove_from_queue(...)` (lines 822, 841).
- **Pending entry removal on cancellation**: When guard drops without being consumed, the `Pending` entry is removed from `state.waiting` by `pending_id`. Evidence: `queue.retain(|p| p.pending_id != my_id)` (line 845).
- **Deficit cleared on guard drop**: When the guard drops and the flow's queue is empty, `deficit` entry is removed. Evidence: `s.deficit.remove(&self.flow_id)` (line 852).

### DRR deficit semantics

- **Deficit accumulates per-round**: Each round, every waiting flow's deficit increases by `weight as i64`. Evidence: `*deficit_entry += weight_i64` (line 397).
- **Zero-weight flows excluded**: Flows with `weight <= 0` are skipped during deficit accumulation. Evidence: `if weight_i64 <= 0 { continue; }` (lines 391-393).
- **Cursor rotation on no-selection**: When no flow is eligible, the front flow in the RR cursor is rotated to the back. Evidence: `s.rr_cursor.push_back(front)` (line 492).
- **Cursor rotation on selection**: A selected flow is removed from the cursor and re-added at the back if it still has pending requests. Evidence: `s.rr_cursor.retain(|id| id != &selected_flow_id)` then `s.rr_cursor.push_back(selected.clone())` (lines 344-347 for starvation, lines 470-476 for normal).

### Backpressure rejection semantics

- **Blocking mode**: Never rejects due to queue depth or timeout; only fails on channel closure. Evidence: `rx.await.map_err(...)` (line 590).
- **Fail-fast mode**: Rejects before queuing when depth exceeds threshold. Evidence: `if depth > self.max_queue_depth` (line 613).
- **Hybrid mode**: Rejects after `max_wait` with `retry_after` computed from current depth. Evidence: `_ = tokio::time::sleep(self.max_wait)` branch (lines 697-702).

## Assumptions

- `work_unit` values are non-negative and finite; `work_unit as i64` truncates the fractional part. Evidence: `work_unit as i64` (lines 327, 385, 450).
- `Mutex` locks (`state.inner`) are never poisoned; all `.lock()` calls use `.unwrap()`. Evidence: lines 228, 293, 440, 618, 718, 842, etc.
- The background admission loop runs for the lifetime of `DrrScheduler`; no shutdown mechanism is exposed.
- `FlowRegistry::get_or_create` is always callable and returns a valid `Flow`.
- `QueueTicket::disarm()` prevents the drop handler from executing when a ticket is sent to a cancelled receiver. Evidence: `ticket.disarm()` (line 269).
- `flow.weight()` returns a finite, non-negative f64. Evidence: `flow.weight() as i64` (line 390).
- Starvation detection via `starvation::is_starved()` relies on `flow.enqueued_at` being set at admission time. Evidence: `*enq = Some(enter)` (lines 585, 673); `starvation::is_starved(&flow, starvation_timeout)` (line 313).

## Dependencies

- `depends_on` [[flow]] — `FlowRegistry`, `FlowId`, `Flow` for per-flow state
- `depends_on` [[metrics]] — `Metrics` for counters and gauges
- `depends_on` [[scheduler/backpressure]] — `BackpressureMode`, `BackpressureRejected`, `fail_fast_retry_after`
- `depends_on` [[scheduler/fifo]] — `QueueTicket`, `make_ticket`
- `depends_on` [[scheduler/priority]] — `priority::select_best`, `priority::FlowCandidate`
- `depends_on` [[scheduler/starvation]] — `starvation::is_starved`, `starvation::record_force_admit`
- `depends_on` [[scheduler/completion_bias]] — `CompletionBiasGate`
- `depends_on` [[scheduler/lifecycle]] — `AccountingReport`
