# WFQ Scheduler — Spec

## Purpose

The WFQ scheduler guarantees fair admission across competing flows by bounding
concurrency, enforcing deterministic selection order, and coupling each admitted
request to a lifecycle-managed ticket that returns its permit on completion.

### Goals

- Bound the number of concurrently active flows to a configured maximum.
- Select which waiting flow to admit next using starvation bypass, then priority,
  then WFQ fairness ratio, then enqueue-time tiebreaker.
- Return a ticket per admission that automatically releases its permit when
  dropped, regardless of success or cancellation.
- Expose three distinct backpressure behaviors — blocking, fail-fast, and hybrid —
  so consumers choose their preferred rejection semantics.
- Report cumulative per-flow service accounting for fairness ratio computation.

## Non-goals

The WFQ scheduler is an admission controller, not a work executor. It makes no
guarantees about how work is performed, how many requests a flow submits, or how
the backend processes admitted work.

### Out of scope

- Does not execute work units or orchestrate inference.
- Does not validate or interpret work-unit estimates beyond accepting them as
  provided.
- Does not expose shutdown or drain; the scheduler lives for the lifetime of the
  process.
- Does not schedule within a flow (intra-flow ordering is outside this boundary).
- Does not rate-limit or throttle admitted flows.

## Interface

The scheduler exposes a ticket-based admission contract and read-only
observability queries. All configuration is fixed at construction time.

### Admission

- `admit(flow_id, work_unit)` — admits a request on behalf of the identified
  flow. On success returns a ticket representing a consumed permit; on rejection
  returns an error carrying a suggested retry duration.
- Backpressure mode determines rejection behavior: blocking waits indefinitely
  for a permit, fail-fast rejects immediately when queue depth exceeds a
  configured threshold, hybrid rejects after a bounded wait timeout expires.
- Work-unit estimate is recorded for the flow's fairness accounting and credited
  upon ticket drop.
- The flow is created automatically if it does not already exist in the
  [[?flow]].
- Admission may be deferred by a completion bias gate [[?scheduler/completion_bias]]:
  in blocking mode the gate blocks before the request enters the waiting queue;
  in hybrid mode the gate check is bounded by the wait timeout and proceeds
  without gate clearance on timeout.
- Blocking mode returns a fixed retry-after duration on error, independent of
  queue state. Fail-fast and hybrid modes compute retry-after from current queue
  depth.

### Observability

- `queue_depth` — returns the sum of per-flow depth counters across all waiting
  flows.
- `queue_snapshot` — returns active count, waiting count, and an ordered list of
  waiting flow identifiers.
- `service_done(flow_id)` — returns the cumulative work-unit credit for a flow;
  returns zero for unknown flows.

## Invariants

These properties hold regardless of implementation and must survive any rewrite.

### Permit accounting

- The sum of active flows and available permits always equals the configured
  maximum active flows.
- Every admitted request consumes exactly one permit; every ticket drop returns
  exactly one permit.
- When no permits remain, no further selection or admission occurs.

### Selection ordering

- A flow waiting longer than the starvation threshold is selected before any
  non-starved flow, regardless of priority or WFQ ratio.
- Among eligible non-starved flows, the flow with the highest priority is
  selected first; WFQ fairness ratio (cumulative service divided by weight)
  breaks priority ties.
- When priority and WFQ ratio are equal, the flow that entered the queue first
  is selected.
- Flows with zero or negative weight are excluded from selection.

### Service accounting

- Each flow's cumulative service credit is monotonically non-decreasing.
- Service credit is scoped to the flow: one counter per flow, keyed by its
  identifier.
- The counter advances by the work-unit estimate recorded at admission, credited
  when the ticket is dropped.

### Depth consistency

- Queue depth increments by one when a flow enters the waiting queue and
  decrements by one when the flow is either admitted or cancelled.
- Depth is released at most once per queued request, regardless of whether the
  request is admitted, cancelled, or times out.

## Constraints

These are hard boundaries imposed by design, not implementation artifacts.

### Capacity

- Concurrency is bounded by `max_active_flows`; the scheduler never admits more
  active flows than this value.
- Queue depth can grow unbounded in blocking mode.
- Fail-fast mode imposes a queue depth threshold for immediate rejection.
- Hybrid mode rejects via bounded wait timeout; it does not check depth before
  queuing.

### Admission lifecycle

- A ticket must be held for the lifetime of the admitted request; dropping the
  ticket without consuming it is permitted and correctly releases the permit and
  depth.
- The scheduler cannot distinguish between a request that completed and one that
  was cancelled; both paths return a permit identically.

### Configuration immutability

- Backpressure mode, maximum active flows, and queue depth threshold are fixed
  at construction and cannot be changed at runtime.
- Starvation timeout defaults to a fixed value and is not configurable via the
  primary public constructor.

### Work-unit assumptions

- Work-unit estimates are assumed positive and finite; the scheduler does not
  validate these values.

### No external control over selection

- The scheduler provides no API to pause, drain, or shut down selection.
  Selection proceeds continuously for the lifetime of the scheduler.

## Rationale

The scheduler's design follows from three goals: fairness across flows, bounded
concurrency, and operational flexibility under overload.

### Why WFQ ratios over pure priority

Pure priority scheduling starves low-priority flows indefinitely. WFQ ratios
(service divided by weight) ensure every flow eventually receives credit
proportional to its weight, while still allowing priority to bias selection.
The starvation threshold provides a hard floor against indefinite waiting.

### Why ticket-based permits

Coupling admission to a drop-managed ticket eliminates manual permit tracking
from callers. Regardless of how a request ends — completion, cancellation, or
error — the permit returns automatically. This prevents silent permit leaks that
would otherwise starve the system.

### Why three backpressure modes

Different consumers have different tolerance for queue pressure. Blocking mode
suits callers that can wait. Fail-fast mode suits health checks and rapid
feedback loops. Hybrid mode offers bounded waits with computed retry intervals.

### Why flow-scoped service accounting

Fairness requires per-flow state. Independent service counters keyed by flow
identifier allow the scheduler to track how much each flow has consumed without
coordination overhead between flows.

### Why FIFO as the final tiebreaker

When WFQ ratios converge, enqueue time provides a stable, observable ordering.
Without it, equal-ratio flows could starve each other through oscillation.

## Related

The WFQ scheduler depends on flow state, metrics reporting, and several
selection strategies.

- `[[?flow]]` — Flow registry, flow identifiers, and per-flow state
- `[[?metrics]]` — Counters and gauges reported by the scheduler
- `[[?scheduler/backpressure]]` — Backpressure mode definitions and retry-after
  computation
- `[[?scheduler/fifo]]` — Queue ticket and depth guard mechanisms
- `[[?scheduler/priority]]` — Priority-based selection algorithm
- `[[?scheduler/starvation]]` — Starvation detection and force-admit logic
- `[[?scheduler/completion_bias]]` — Completion bias gate for admission gating
- `[[src/scheduler/wfq.rs#WfqScheduler]]`
- `[[src/scheduler/backpressure.rs#BackpressureRejected]]`
- `[[src/scheduler/fifo.rs#QueueTicket]]`
