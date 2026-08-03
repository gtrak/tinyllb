# WFQ Scheduler — Extraction

Source: `src/scheduler/wfq.rs`

## Responsibilities

- Admits requests into an active set bounded by `max_active_flows` permits.
- Selects flows from waiting queues using starvation bypass, priority, and WFQ ratio.
- Returns `QueueTicket` to admitted callers; ticket drop releases the permit and credits service accounting.
- Exposes three backpressure modes governing admission wait/reject behavior.
- Reports queue depth, queue snapshot, and per-flow service_done counters.

## Interface Surfaces

### `WfqScheduler::new()` — Construction

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn new(max_active_flows: u32, metrics: Arc<Metrics>, registry: Arc<FlowRegistry>, backpressure_mode: BackpressureMode, max_queue_depth: u32, max_wait: Duration, retry_after_base: Duration) -> Self` |
| Line | 96-129 |
| Postcondition | Spawns a background admission loop; initializes `available_permits` to `max_active_flows` |
| Code evidence | `available_permits: max_active_flows` (line 175); `tokio::spawn(Self::admission_loop(...))` (line 185) |

### `WfqScheduler::new_with_policies()` — Construction with policy hooks

| Aspect | Evidence |
|--------|----------|
| Signature | `pub(crate) fn new_with_policies(..., starvation_timeout: Duration, policies: Policies) -> Self` |
| Line | 133-155 |
| Visibility | `pub(crate)` — internal-only constructor |
| Postcondition | Delegates to `new_inner` with caller-provided `CompletionBiasGate` and `starvation_timeout` |

### `admit()` — Request admission

| Aspect | Evidence |
|--------|----------|
| Signature | `pub async fn admit(&self, flow_id: FlowId, work_unit: f64) -> Result<QueueTicket, BackpressureRejected>` |
| Line | 388-410 |
| Input: `flow_id` | Flow identifier; guarantees flow exists via `registry.get_or_create` (line 394) |
| Input: `work_unit` | Work unit estimate; used for service_done accounting and WFQ ratio |
| Output: `Ok(QueueTicket)` | Caller holds a ticket; flow is counted active; permit consumed |
| Output: `Err(BackpressureRejected)` | Request rejected; contains `retry_after` duration |
| Error contract | Behavior is dispatched by `backpressure_mode` (lines 396-409) |

#### Blocking mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Waits indefinitely for admission; on channel close, returns `BackpressureRejected { retry_after: 1s }` |
| Lines | 412-486 |
| Postcondition on success | Records `queue_wait_seconds` metric; increments `active_flows` (lines 479-483) |

#### Fail-fast mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Rejects immediately if `queue_depth() > max_queue_depth`; otherwise delegates to blocking |
| Lines | 488-506 |
| Error: `retry_after` | Computed by `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)` (line 499) |

#### Hybrid mode

| Aspect | Evidence |
|--------|----------|
| Behavior | Races ticket receipt against `max_wait` timeout; timeout yields rejection with computed `retry_after` |
| Lines | 508-597 |
| Gate timeout | `completion_bias_gate.check()` bounded by `max_wait` (lines 529-531) |

### `queue_depth()` — Queue depth query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn queue_depth(&self) -> u32` |
| Line | 600-602 |
| Postcondition | Returns `registry.sum_depths()` — sum of per-flow depth counters |

### `queue_snapshot()` — Queue state query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn queue_snapshot(&self) -> QueueSnapshot` |
| Lines | 605-613 |
| Postcondition | Returns `QueueSnapshot` with active count, waiting count, and ordered list of waiting flow IDs |
| Code evidence | `metrics.active_flows.get()` (line 606); `registry.queue_snapshot(...)` (line 612) |

### `service_done()` — Per-flow service accounting query

| Aspect | Evidence |
|--------|----------|
| Signature | `pub fn service_done(&self, flow_id: &FlowId) -> f64` |
| Lines | 617-623 |
| Postcondition | Returns total work_unit credited to this flow; `0.0` if no counter exists |
| Code evidence | `f64::from_bits(counter.load(Ordering::Relaxed))` (line 620) |

## Invariants

### Selection ordering

- **Starvation bypass**: Flows waiting longer than `starvation_timeout` are force-selected before normal priority/WFQ rules. Flows with weight <= 0 are excluded from selection. Evidence: lines 306-330 (starvation phase), line 343 (weight check), line 365 (priority selection).
- **Priority dominates WFQ**: Among non-starved, eligible flows, higher priority is selected first; WFQ ratio (`service_done / weight`) breaks ties. Evidence: line 352 (ratio computation), line 365 (`priority::select_best`).
- **FIFO within priority**: When priority and WFQ ratio are equal, the flow with the earliest enqueue time wins. Evidence: line 359 (`enqueued_at` passed to candidate), line 365 (`select_best` uses it as tiebreaker).

### Permit accounting

- **Permit pool bound**: `available_permits` starts at `max_active_flows`, decrements on selection, increments on ticket drop. The sum of active flows + available_permits equals `max_active_flows`. Evidence: `available_permits: max_active_flows` (line 175); `s.available_permits -= 1` (lines 321, 373); `s.available_permits += 1` (line 258).
- **Zero-permit stall**: When `available_permits == 0`, `try_select` returns `None`; no flow is admitted. Evidence: lines 301-303.

### Service accounting

- **service_done is cumulative**: Each ticket drop adds `work_unit` to the flow's `service_done` counter; the counter never decreases. Evidence: `f64::to_bits(current + work_unit)` (line 248).
- **service_done is flow-scoped**: Each flow has an independent `service_done` counter keyed by `FlowId`. Evidence: `s.service_done.entry(flow_id_for_ticket.clone())` (line 244).

### Admission guard cleanup

- **Depth consistency**: `WfqAdmitGuard` increments flow depth on creation and decrements it on both success (`consume()`) and cancellation (`drop`). Guard is never double-consumed via `active` flag. Evidence: `fetch_add(1)` (line 648); `fetch_sub(1)` (lines 678, 696); `if !self.active { return; }` (lines 671, 690).
- **Waiting queue consistency**: Guard adds flow to `waiting_queue` on creation and removes it on both consume and drop. Evidence: `push_back(flow_id)` (line 653); `remove_from_queue(...)` (lines 684, 703).
- **Pending entry removal on cancellation**: When guard drops without being consumed, the `Pending` entry is removed from `state.waiting` by `pending_id`. Evidence: `queue.retain(|p| p.pending_id != my_id)` (line 707).

### Backpressure rejection semantics

- **Blocking mode**: Never rejects due to queue depth or timeout; only fails on channel closure (unusual path). Evidence: `rx.await.map_err(...)` (line 467).
- **Fail-fast mode**: Rejects before queuing when depth exceeds threshold. Evidence: `if depth > self.max_queue_depth` (line 497).
- **Hybrid mode**: Rejects after `max_wait` with `retry_after` computed from current depth. Evidence: `_ = tokio::time::sleep(self.max_wait)` branch (lines 586-593).

## Assumptions

- `work_unit` values are positive and finite; zero/negative values are not explicitly rejected by `admit()` but weight <= 0 flows are excluded from selection (line 343).
- `Mutex` locks (`state.inner`) are never poisoned; all `.lock()` calls use `.unwrap()`. Evidence: lines 242, 300, 440, 618, etc.
- The background admission loop runs for the lifetime of `WfqScheduler`; no shutdown mechanism is exposed.
- `FlowRegistry::get_or_create` is always callable and returns a valid `Flow`.
- `QueueTicket::disarm()` prevents the drop handler from executing when a ticket is sent to a cancelled receiver. Evidence: `ticket.disarm()` (line 275).

## Dependencies

- `depends_on` [[flow]] — `FlowRegistry`, `FlowId`, `Flow` for per-flow state
- `depends_on` [[metrics]] — `Metrics` for counters and gauges
- `depends_on` [[scheduler/backpressure]] — `BackpressureMode`, `BackpressureRejected`, `fail_fast_retry_after`
- `depends_on` [[scheduler/fifo]] — `QueueTicket`, `make_ticket`
- `depends_on` [[scheduler/priority]] — `priority::select_best`, `priority::FlowCandidate`
- `depends_on` [[scheduler/starvation]] — `starvation::is_starved`, `starvation::record_force_admit`
- `depends_on` [[scheduler/completion_bias]] — `CompletionBiasGate`
