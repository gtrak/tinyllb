# Scheduler Facade and Policy Selection

The scheduler facade is the single admission-control entry point: it wraps the DRR flow scheduler with a shared KV-cache gate, a backend stall gate, and a turn-boundary priority heuristic.

## Purpose

The facade presents the admission contract and aggregated queue metrics while keeping cross-cutting admission concerns out of the DRR scheduler.

- Provides a unified admission entry point that returns a queue ticket or a backpressure rejection carrying a retry-after.
- Runs the per-flow turn-boundary state machine (cadence registry) on every admission, classifying flows as interactive (turn-boundary idle) or agentic (continuous tool-call activity) and adjusting priority automatically.
- Enforces a shared KV-cache admission gate (`KvPolicy`) before every admit to the DRR scheduler.
- Rejects admissions with a fixed 429 + 5-second Retry-After while the backend stall signal is set.
- Ensures queue depth and snapshot metrics always include requests delayed by the KV gate.
- Re-exports `DrrScheduler`, `KvPolicy`, `KvBiasHandle`, `QueueTicket`, `make_ticket`, `FlowProgressTracker`, `AccountingReport`, and the backpressure helpers.

## Non-goals

The facade deliberately does not implement scheduling logic or define scheduling policy.

- Does not implement DRR selection, credit accounting, or backpressure modes; delegates to the wrapped `DrrScheduler`.
- Does not decide KV admission policy; delegates to the shared `KvPolicy` gate.
- Does not handle backend stalls beyond rejecting new admissions with a fixed retry-after.
- Does not manage request lifecycle beyond admission and accounting reports.
- Does not persist state; all metrics and accounting are ephemeral to the current instance.

## Interface

The facade exposes admission, queue observation, flow accounting, shared state access, reaping, construction, and re-exported types.

**Admission.** `admit` (defaults `is_turn_boundary` to true) and `admit_with_turn_boundary` accept a flow identity and a work quantity; they return a queue ticket confirming admission or a backpressure rejection carrying a retry-after duration. Before the KV-cache gate, the facade records the arrival with the `is_turn_boundary` flag in the cadence registry and runs the turn-boundary state machine, which may adjust the flow's priority. Flows with an explicit priority override (header or admin) are skipped by the state machine. See [[flow#Flow Registry and State]]. After the KV gate, if the backend stall signal is set, the admission is rejected with a fixed 5-second retry-after; otherwise admission proceeds to the DRR scheduler.

**Queue observation.** Two read-only surfaces for inspecting queue state. The depth query returns the DRR queue depth plus the number of requests delayed by the KV gate. The snapshot query returns the active-flow count, the waiting count (DRR-waiting plus KV-delayed), and the list of waiting flows.

**Flow accounting.** A credit query returns the flow's DRR credit balance. An accounting report passes a completed request's outcome to the DRR scheduler, which adjusts the flow's credit based on actual delivered tokens.

**Shared state access.** The flow progress tracker reference is available to all callers; the returned reference grants shared access equivalent to the facade's own internal reference.

**Reaping.** Evicts idle flow-registry and cadence entries older than a given TTL; called periodically by the background reaper task to prevent unbounded registry growth.

**Construction.** Two construction paths exist. The full constructor accepts all policy parameters (backpressure mode, queue depth limit, wait time, starvation timeout, completion bias, KV policy config, backend monitor, priority policy, priorities, and KV bias) and produces an instance without failure. The defaults constructor fills remaining values with fixed defaults: a 300-second starvation timeout, type-level default completion bias, an empty backend monitor, a disabled KV policy config, and type-level default priority policy, priorities, and KV bias.

**Publicly re-exported types.** The module re-exports the DRR scheduler, KV gate and bias types, the queue ticket and its factory, the flow progress tracker, the accounting report, and the backpressure helpers. Callers may import these directly for use outside the facade; direct use bypasses the facade's KV gate, stall gate, and cadence heuristic. The `lifecycle` submodule is also publicly accessible.

## Invariants

These statements hold regardless of configuration or how the facade is reconstructed.

**KV gate ordering.** The KV-cache admission gate always executes before the DRR scheduler on every admission attempt. A KV-policy rejection prevents the DRR scheduler from seeing the request.

**Stall gate ordering.** While the backend stall signal is set, new admissions are rejected with a 429 carrying a fixed 5-second retry-after; the KV gate and the DRR scheduler are not consulted.

**Queue metric aggregation.** The reported queue depth always equals the DRR scheduler's queue depth plus the number of requests currently delayed by the KV gate.

**Snapshot completeness.** The waiting count in any queue snapshot always includes both the DRR scheduler's waiting requests and the KV-delayed requests.

**Cadence heuristic ordering.** The turn-boundary state machine executes before the KV-cache gate on every admission. It records the arrival and the `is_turn_boundary` flag, then transitions the flow's state: a turn-boundary idle gap at or above `idle_gap_threshold` promotes to Interactive (interactive priority class); continuous non-turn-boundary arrivals increment a counter that demotes through AgenticSuspected (agent class) to AgenticConfirmed (background class). Flows with an explicit priority override (header or admin pin) are never modified by the state machine. When the heuristic is disabled (`priority_policy.enabled = false`), no priority changes occur from the state machine.

## Constraints

These are limitations imposed by the design, not implementation accidents.

**Stall rejection is fixed.** The stall gate rejects with a fixed 5-second retry-after rather than a value computed from queue state.

**Fixed defaults for backward-compatible construction.** The defaults constructor applies fixed values: starvation timeout at 300 seconds, type-level default completion bias, an empty backend monitor, a disabled KV policy config, and type-level default priority policy, priorities, and KV bias.

**Backpressure rejection carries retry information.** Every backpressure rejection includes a retry-after duration; the facade never rejects silently.

**Configuration-driven admission.** Admission policy — including maximum active flows, queue depth limits, wait timeouts, and priority policy — is entirely determined at construction. No runtime reconfiguration is exposed.

**Re-exported types bypass the facade.** Re-exported types enable direct use outside the facade (for example, constructing the DRR scheduler directly or building tickets with `make_ticket`); such use bypasses the facade's KV gate, stall gate, and cadence heuristic.

## Rationale

The facade keeps cross-cutting admission concerns — KV-cache pressure, backend stalls, and interactive-vs-agentic classification — at the boundary instead of inside the DRR scheduler.

- KV gate before scheduling: KV-cache capacity is a hard resource limit. Checking it first avoids wasting scheduler state on requests that cannot be served regardless of policy.
- Stall gate: when the backend is stalled, an immediate 429 + Retry-After lets clients back off instead of being admitted and then aborted by the stall watchdog.
- Cadence before the gate: every arrival updates the flow's classification state whether or not admission proceeds, so rejections at the KV gate do not stop the heuristic from learning.
- Aggregated queue metrics: operators need a total picture of backlogged work. Splitting between DRR-queued and KV-delayed would require callers to reconcile two sources.
- Default constructor for convenience: most deployments do not need custom starvation or KV policy. A defaults path reduces boilerplate for common configurations.

## Related

See also these related concepts and source files.
- [[scheduler#Deficit Round Robin Discipline]] — wrapped DRR scheduler
- [[scheduler#Queue Ticket]] — RAII admission handle returned by admit
- [[admission#KV-Cache-Aware Admission Gate]] — KV-cache admission gate policy
- [[admission#Backpressure and Admission Rejection]] — Backpressure and retry-after computation
- [[admission#Per-Flow Token Progress Tracking]] — Flow progress tracking
- [[scheduler_policies#Completion Bias Gate]] — Completion bias policy
- [[src/scheduler/mod.rs]] — Facade implementation
- [[src/scheduler/backpressure.rs]] — Backpressure rejection and retry-after helpers
- [[src/scheduler/kv_admission.rs]] — KV-cache admission gate
- [[src/scheduler/flow_progress.rs]] — Flow progress tracker
- [[src/scheduler/drr.rs]] — DRR scheduler implementation
- [[src/scheduler/lifecycle.rs]] — Lifecycle types and accounting
- [[src/flow/cadence.rs]] — turn-boundary state machine implementation
- [[src/config/mod.rs#PriorityPolicy]] — priority policy configuration

# Deficit Round Robin Discipline

The DRR scheduler allocates a bounded pool of active-flow permits among competing flows using deficit round-robin credit, guaranteeing proportional scheduling opportunities according to each flow's configured weight.

## Purpose

The DRR scheduler distributes scheduling turns proportionally to each flow's weight, enforces a concurrency ceiling, provides three admission backpressure policies, and restores credit on completion or cancellation.

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

**Admission.** A caller presents a flow identity and a work-unit estimate; the scheduler either grants an admission ticket or rejects with a signal containing a retry duration. See [[scheduler#Queue Ticket]] for ticket semantics and [[admission#Backpressure and Admission Rejection]] for rejection modes. Three backpressure modes govern admission behavior: blocking (waits indefinitely for a permit), fail-fast (rejects immediately when queue depth exceeds a configured threshold), and hybrid (waits before rejecting; total wall-clock wait can reach approximately two times the configured duration due to sequential gate and channel waits). Admission includes a completion-bias gate check that can delay admission indefinitely in blocking mode. In hybrid mode, the gate check and the subsequent channel wait each consume up to the configured duration, yielding a worst-case total wait of roughly double the configured value. See [[scheduler_policies#Completion Bias Gate]]. Fail-fast mode can reject before any admission state is established; such rejections produce no depth or permit accounting side effects. Admission always ensures the requesting flow is registered; a new flow is materialized if not already present. See [[flow#Flow Registry and State]].

**Observation.** Queue depth reports the total number of pending requests across all flows, derived from the flow registry's aggregate depth counters rather than maintained independently. See [[flow#Flow Registry and State]]. Queue snapshot reports active-flow count, waiting-flow count, and the ordered list of flows awaiting scheduling. Per-flow credit reports only the permanent credit balance, excluding any per-round eligibility accumulator; metric gauges that expose credit may briefly include the eligibility accumulator during the scheduling round's accumulation phase, causing a transient divergence from the permanent balance.

**Accounting.** A caller reports a flow outcome using a tagged report indicating either completion or cancellation, each carrying a restore cost that the scheduler adds to the flow's permanent credit balance. The completion report variant carries an additional delivered-tokens field that the scheduler ignores; callers may set it to any value without effect. Credit restoration applies identically to completion and cancellation — neither penalizes the flow beyond the work-unit cost already debited at selection. Reporting an outcome for a flow that was never admitted creates the flow and grants it positive credit equal to the restore cost; the scheduler does not validate that a prior debit exists.

## Invariants

The scheduler maintains four classes of invariants: selection ordering, credit accounting, permit accounting, and admission-lifecycle consistency.

**Selection ordering.** Flows that exceed a starvation timeout are selected before any priority or credit evaluation. See [[scheduler_policies#Starvation Protection]]. Among non-starved, eligible flows, higher priority is selected before lower priority. See [[scheduler_policies#Priority-Aware Flow Selection]]. Ties in priority are broken by round-robin cursor position: the flow earliest in the cursor wins. When cursor positions are identical, the flow that enqueued earliest wins. Starvation force-selection is a full selection: the flow's permanent credit is debited by the work-unit cost at the time of force-selection, identical to normal selection.

**Credit accounting.** The scheduler maintains two independent credit values: a permanent balance (debited on selection, restored via accounting reports) and a per-round eligibility accumulator (grows by the flow's weight each round, consumed on selection). The permanent balance persists across queue-empty cycles and is never zeroed by the scheduler alone. The eligibility accumulator is cleared when a flow is selected, when its admission lifecycle ends via cancellation, or during cleanup of empty waiting flows. Credit restored via accounting reports increases only the permanent balance; the eligibility accumulator is unaffected.

**Permit accounting.** The sum of active flows and available permits equals the configured maximum; permits are never created or destroyed beyond this bound. When no permits are available, the scheduler cannot select additional flows from the waiting set.

**Admission-lifecycle consistency.** A pending flow's queue depth is incremented on admission and decremented exactly once when the flow is either selected (admitted) or abandoned (cancelled). The enqueue timestamp is a single value per flow, shared across all queued entries. When any entry for a flow is selected, the timestamp is cleared unconditionally; remaining queued entries for that flow lose starvation detection until a new admission re-sets the timestamp. Cancelled admissions remove pending entries from the waiting set and, when the flow's queue becomes empty, clear any eligibility accumulator.

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

See also these related concepts and source files.
- [[admission#Backpressure and Admission Rejection]] — BackpressureMode, BackpressureRejected, fail-fast retry computation
- [[scheduler_policies#Completion Bias Gate]] — Completion bias gate admission dependency
- [[scheduler#Queue Ticket]] — QueueTicket, ticket creation and disarm
- [[scheduler_policies#Priority-Aware Flow Selection]] — Priority-based selection among eligible candidates
- [[scheduler_policies#Starvation Protection]] — Starvation detection and force-selection
- [[flow#Flow Registry and State]] — FlowId, Flow, per-flow state management
- [[metrics#Metric Family Contracts]] — Metrics gauges and counters reported by the scheduler
- [[src/scheduler/drr.rs]] — DRR scheduler implementation
- [[src/scheduler/backpressure.rs]] — Backpressure types
- [[src/scheduler/ticket.rs]] — Queue ticket implementation
- [[src/scheduler/completion_bias.rs]] — Completion bias gate
- [[src/scheduler/starvation.rs]] — Starvation detection
- [[src/scheduler/priority.rs]] — Priority selection

# Queue Ticket

The queue ticket is the RAII admission handle returned by `Scheduler::admit`; its drop handler releases the admission permit and updates flow accounting on every exit path.

## Purpose

The ticket couples every admission to a drop-managed handle so the concurrency permit cannot leak regardless of how the request ends.

- Releases the admission permit on all exit paths: success, error, panic (Drop runs on unwind), and client disconnect (the future handler drops).
- `disarm()` takes the drop handler, making a later drop a no-op. It is used when a ticket's oneshot delivery fails, so the handle cannot double-release.
- `make_ticket` constructs a `QueueTicket` from a flow id, a work-unit estimate, and a drop-handler closure; the closure releases the permit and reports completion when the ticket is dropped.

## Invariants

These properties hold regardless of implementation.

- The admission permit is released exactly once per ticket. `disarm()` makes disposal a no-op, and the caller that disarmed is responsible for releasing the permit exactly once itself — in DRR, the admission loop releases it on failed oneshot delivery, preventing a double release.
- Dropping an armed ticket runs the drop handler, which in DRR decrements the `llm_active_flows` gauge and the flow's per-flow active counter, releases one permit, and signals completion-bias waiters.

## Related

See also these related concepts and source files.
- [[src/scheduler/ticket.rs]] — Ticket infrastructure module
- [[src/scheduler/ticket.rs#QueueTicket]] — RAII ticket type
- [[src/scheduler/ticket.rs#make_ticket]] — Ticket factory
- [[scheduler#Deficit Round Robin Discipline]] — Scheduler that builds tickets and owns the permit pool
- [[scheduler_policies#Completion Bias Gate]] — Waiters signaled on armed ticket drop
- [[metrics#Metric Family Contracts]] — `llm_active_flows` gauge updated by the ticket
