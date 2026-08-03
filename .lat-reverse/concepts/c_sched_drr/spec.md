# DRR Scheduler — Specification

## Purpose

The DRR scheduler allocates a bounded pool of active-flow permits among competing flows using deficit round-robin credit. It guarantees that every flow receives proportional share of scheduling opportunities according to its configured weight, while enforcing a hard concurrency ceiling and providing three admission backpressure policies for callers.

- Admits request flows only while the total number of active flows remains within a configured ceiling.
- Distributes scheduling turns among waiting flows proportionally to each flow's configured weight.
- Returns an admission ticket that releases a permit when the flow's work completes or is abandoned.
- Exposes three distinct backpressure contracts governing how admission requests behave under contention.
- Accepts accounting reports that restore credit to a flow after completion or cancellation.

## Non-goals

This concept does not address request routing, model inference, or GPU resource scheduling.

- Does not determine which model backend handles a request; only admits flows to the scheduling queue.
- Does not enforce fairness across models or hardware partitions; fairness is scoped to flows within the same scheduler instance.
- Does not provide preemption; a flow admitted under a permit retains that permit until ticket release.
- Does not expose lifecycle hooks for graceful shutdown; the scheduler runs until the owning handle is dropped.
- Does not validate work-unit estimates; correctness of cost accounting depends on accurate work-unit input.

## Interface

The scheduler exposes three categories of contracts: admission, observation, and accounting.

### Admission

- A caller presents a flow identity and a work-unit estimate; the scheduler either grants an admission ticket or rejects with a signal containing a retry duration. See [[?c_sched_fifo]] for ticket semantics and [[?c_sched_backpressure]] for rejection modes.
- Three backpressure modes govern admission behavior: blocking (waits indefinitely for a permit), fail-fast (rejects immediately when queue depth exceeds a configured threshold), and hybrid (waits before rejecting; total wall-clock wait can reach approximately two times the configured duration due to sequential gate and channel waits).
- Admission includes a completion-bias gate check that can delay admission indefinitely in blocking mode. In hybrid mode, the gate check and the subsequent channel wait each consume up to the configured duration, yielding a worst-case total wait of roughly double the configured value. See [[?c_sched_completion_bias]].
- Fail-fast mode can reject before any admission state is established; such rejections produce no depth or permit accounting side effects.
- Admission always ensures the requesting flow is registered; a new flow is materialized if not already present. See [[?c_flow_registry]].

### Observation

- Queue depth reports the total number of pending requests across all flows, derived from the flow registry's aggregate depth counters rather than maintained independently. See [[?c_flow_registry]].
- Queue snapshot reports active-flow count, waiting-flow count, and the ordered list of flows awaiting scheduling.
- Per-flow credit reports only the permanent credit balance, excluding any per-round eligibility accumulator; metric gauges that expose credit may briefly include the eligibility accumulator during the scheduling round's accumulation phase, causing a transient divergence from the permanent balance.

### Accounting

- A caller reports a flow outcome using a tagged report indicating either completion or cancellation, each carrying a restore cost that the scheduler adds to the flow's permanent credit balance.
- The completion report variant carries an additional delivered-tokens field that the scheduler ignores; callers may set it to any value without effect.
- Credit restoration applies identically to completion and cancellation — neither penalizes the flow beyond the work-unit cost already debited at selection.
- Reporting an outcome for a flow that was never admitted creates the flow and grants it positive credit equal to the restore cost; the scheduler does not validate that a prior debit exists.

## Invariants

The scheduler maintains four classes of invariants: selection ordering, credit accounting, permit accounting, and admission-lifecycle consistency.

### Selection ordering

- Flows that exceed a starvation timeout are selected before any priority or credit evaluation. See [[?c_sched_starvation]].
- Among non-starved, eligible flows, higher priority is selected before lower priority. See [[?c_sched_priority]].
- Ties in priority are broken by round-robin cursor position: the flow earliest in the cursor wins. When cursor positions are identical, the flow that enqueued earliest wins.
- Starvation force-selection is a full selection: the flow's permanent credit is debited by the work-unit cost at the time of force-selection, identical to normal selection.

### Credit accounting

- The scheduler maintains two independent credit values: a permanent balance (debited on selection, restored via accounting reports) and a per-round eligibility accumulator (grows by the flow's weight each round, consumed on selection).
- The permanent balance persists across queue-empty cycles and is never zeroed by the scheduler alone.
- The eligibility accumulator is cleared when a flow is selected, when its admission lifecycle ends via cancellation, or during cleanup of empty waiting flows.
- Credit restored via accounting reports increases only the permanent balance; the eligibility accumulator is unaffected.

### Permit accounting

- The sum of active flows and available permits equals the configured maximum; permits are never created or destroyed beyond this bound.
- When no permits are available, the scheduler cannot select additional flows from the waiting set.

### Admission-lifecycle consistency

- A pending flow's queue depth is incremented on admission and decremented exactly once when the flow is either selected (admitted) or abandoned (cancelled).
- The enqueue timestamp is a single value per flow, shared across all queued entries. When any entry for a flow is selected, the timestamp is cleared unconditionally; remaining queued entries for that flow lose starvation detection until a new admission re-sets the timestamp.
- Cancelled admissions remove pending entries from the waiting set and, when the flow's queue becomes empty, clear any eligibility accumulator.

## Constraints

The scheduler operates under the following limitations and boundaries.

- Permits are a global pool shared by all flows; no per-flow concurrency limit is enforced.
- Work-unit estimates and weight values are both truncated to integers; fractional work units are silently floored during credit deduction, and fractional weights below 1.0 yield zero per-round eligibility growth.
- The starvation timeout defaults to 300 seconds in the default constructor; the policy-aware constructor accepts a custom duration.
- The background admission loop runs for the lifetime of the scheduler and cannot be independently stopped.
- The scheduler assumes exclusive access to the flow registry; concurrent mutation of registry state by external agents is undefined.
- Hybrid-mode admission can block for up to approximately two times the configured wait duration in the worst case (gate timeout followed by channel timeout); callers must plan for this bound.

## Rationale

Deficit round-robin provides proportional fairness without per-timestep scheduling, making it suitable for async workloads where flows submit bursts of requests.

- Permits cap concurrency rather than throughput, allowing the scheduler to absorb bursty traffic without over-subscribing downstream resources.
- Maintaining permanent credit and per-round eligibility as independent values ensures that fairness is computed relative to current weight while accounting survives across scheduling rounds.
- Three backpressure modes let consumers choose between latency tolerance (blocking), capacity protection (fail-fast), and balanced behavior (hybrid) without changing scheduler internals.
- Admission cleanup guarantees that depth and permit state remain consistent regardless of whether the flow completes, times out, or is abandoned.
- A three-level tie-breaking chain (priority, cursor position, enqueue time) ensures deterministic selection under all conditions, even when flows share identical priority and cursor index.

## Related

- [[c_sched_backpressure]] — BackpressureMode, BackpressureRejected, fail-fast retry computation
- [[c_sched_completion_bias]] — Completion bias gate admission dependency
- [[c_sched_fifo]] — QueueTicket, ticket creation and disarm
- [[c_sched_priority]] — Priority-based selection among eligible candidates
- [[c_sched_starvation]] — Starvation detection and force-selection
- [[c_flow_registry]] — FlowId, Flow, per-flow state management
- [[c_metrics_families]] — Metrics gauges and counters reported by the scheduler
- [[src/scheduler/drr.rs]] — Implementation
