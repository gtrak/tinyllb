# Scheduler Facade and Policy Selection

The scheduler facade presents a single admission-control surface that hides algorithm-specific dispatch, enforcing a KV-cache gate before every admit attempt and aggregating queue metrics uniformly.

## Purpose

The scheduler facade guarantees that every admission attempt passes through a shared KV-cache gate before consulting the selected scheduling policy, and that all queue metrics uniformly account for requests delayed by that gate.

- Provides a unified entry point regardless of which scheduling algorithm is configured.
- Enforces a KV-cache admission gate before every admit attempt.
- Ensures queue depth and snapshot metrics always include KV-delayed requests.
- Exposes shared policy state — completion bias, starvation timeout, flow progress — to all algorithm variants.
- Supports algorithm-specific accounting through a uniform query interface.
- Runs a per-flow cadence heuristic on every admission that classifies flows by inter-request timing and adjusts priority automatically, promoting interactive (slow-gapping) flows and demoting batch (rapid-fire) flows.

## Non-goals

The facade deliberately does not implement scheduling logic or define policy parameters.

- Does not implement any scheduling algorithm; dispatch is delegated to an algorithm-specific backend.
- Does not decide admission policy; delegates to the configured scheduler and the KV gate.
- Does not manage request lifecycle beyond admission and accounting report.
- Does not define backpressure policy; delegates retry-after computation to the underlying scheduler.
- Does not persist state; all metrics and accounting are ephemeral to the current instance.

## Interface

The facade exposes public contract surfaces: admission, queue observation, flow accounting, shared state access, construction, re-exported types, and a lifecycle submodule.

**Admission.** A request to schedule a work unit for a flow. Accepts a flow identity and a work quantity; returns either a queue ticket confirming admission or a backpressure rejection carrying a retry-after duration. No admission succeeds without passing the KV-cache gate.
Before the KV-cache gate, the facade records an arrival timestamp for the flow and runs the cadence classification heuristic, which may adjust the flow's priority based on its recent inter-request gap pattern. Flows with explicit priority overrides (header or admin) are skipped by the heuristic. See [[flow#Flow Registry and State]].

**Queue observation.** Two read-only surfaces for inspecting queue state. The depth query returns the total number of queued requests, including those delayed by the KV gate. The snapshot query returns counts of active flows, waiting requests, and total flow count.

**Flow accounting.** Algorithm-specific per-flow queries. A service-total query returns cumulative service for a flow — meaningful only for work-tracking algorithms. A credit query returns the current credit balance — meaningful only for credit-based algorithms. An accounting report accepts a completed request's outcome and updates the flow's internal accounting, or silently no-ops for algorithms without accounting.

**Shared state access.** Shared ownership of the flow progress tracker, available to all callers regardless of configured algorithm. The returned tracker reference grants shared access equivalent to the facade's own internal reference.

**Construction.** Two construction paths exist. The full constructor accepts all policy parameters and produces an instance without failure. The defaults constructor accepts a reduced parameter set and fills remaining values with fixed defaults, including an empty backend monitor and a KV-cache configuration derived from type-level defaults.

**Publicly re-exported types.** The module re-exports algorithm schedulers, policy types, admission artifacts, and helper functions. Callers may import these directly for use outside the facade, including instantiating algorithm backends that bypass the facade's KV gate and shared policy layer.

**Public submodule.** The lifecycle submodule is publicly accessible, exposing request lifecycle types as a supplementary contract surface beyond the re-exported types.

## Invariants

These statements hold regardless of which scheduling algorithm is configured or how the facade is reconstructed.

**KV gate ordering.** The KV-cache admission gate always executes before the flow-scheduler consults its own policy. A KV-policy rejection prevents the flow-scheduler from seeing the request.

**Queue metric aggregation.** The reported queue depth always equals the sum of the flow-scheduler's queue depth and the number of requests currently delayed by the KV gate.

**Snapshot completeness.** The waiting count in any queue snapshot always includes both the flow-scheduler's waiting requests and the KV-delayed requests.

**Algorithm-exhaustive dispatch.** Every public operation that delegates to the underlying scheduler covers all configured algorithm variants; no variant is left unhandled.

**Neutral accounting for non-applicable algorithms.** Querying service total on a non-work-tracking algorithm always yields zero. Querying credit on a non-credit algorithm always yields zero. Reporting accounting to a non-accounting algorithm has no observable effect.
**Cadence heuristic ordering.** The cadence classification step executes before the KV-cache gate on every admission. It records an arrival timestamp, computes the median inter-request gap over a rolling window, and may update the flow's priority. Flows with a non-zero priority source (explicit header or admin pin) are never modified by the heuristic. When the heuristic is disabled (`priority_policy.enabled = false`), no priority changes occur from the cadence step.

## Constraints

These are limitations imposed by the design, not implementation accidents.

**Single algorithm per instance.** An instance dispatches to exactly one scheduling algorithm, selected at construction time. The algorithm cannot change during the instance's lifetime.

**Fixed defaults for backward-compatible construction.** The defaults constructor applies fixed values: starvation timeout at 300 seconds, completion bias at its type-level default, an empty backend monitor, and a KV-cache configuration derived from type-level defaults.

**Backpressure rejection carries retry information.** Every backpressure rejection includes a retry-after duration; the facade never rejects silently.

**Configuration-driven admission.** Admission policy — including maximum active flows, queue depth limits, and wait timeouts — is entirely determined at construction. No runtime reconfiguration is exposed.

**Public algorithm types bypass the facade.** Re-exported algorithm scheduler types enable direct instantiation outside the facade. Callers using these types bypass the facade's KV gate and shared policy layer entirely.
**Priority policy construction.** The full constructor accepts a `PriorityPolicy` and `Priorities` value; the defaults constructor uses type-level defaults (`PriorityPolicy::default()`, `Priorities::default()`). The priority policy cannot be changed at runtime.

## Rationale

The facade exists to make algorithm selection an operational concern rather than an architectural one.

- Single facade: callers depend on admission and queue metrics, not on which algorithm handles dispatch. A facade isolates callers from algorithm churn.
- KV gate before scheduling: KV-cache capacity is a hard resource limit. Checking it first avoids wasting scheduler state on requests that cannot be served regardless of policy.
- Aggregated queue metrics: operators need a total picture of backlogged work. Splitting between scheduler-queued and KV-delayed would require callers to reconcile two sources.
- Neutral accounting for irrelevant algorithms: exposing service and credit queries uniformly avoids caller-side branching on algorithm type. Algorithms that do not track accounting return neutral values instead of errors.
- Default constructor for convenience: most deployments do not need custom starvation or KV policy. A defaults path reduces boilerplate for common configurations.

## Related

See also these related concepts and source files.
- [[scheduler#FIFO Queueing Discipline]] — FIFO scheduling algorithm
- [[scheduler#Weighted Fair Queueing Discipline]] — Weighted-fair-queue scheduling algorithm
- [[scheduler#Deficit Round Robin Discipline]] — Deficit-round-robin scheduling algorithm
- [[admission#KV-Cache-Aware Admission Gate]] — KV-cache admission gate policy
- [[admission#Backpressure and Admission Rejection]] — Backpressure and retry-after computation
- [[admission#Per-Flow Token Progress Tracking]] — Flow progress tracking
- [[scheduler_policies#Completion Bias Gate]] — Completion bias policy
- [[src/scheduler/mod.rs]] — Facade implementation
- [[src/scheduler/backpressure.rs]] — Backpressure rejection and retry-after helpers
- [[src/scheduler/kv_admission.rs]] — KV-cache admission gate
- [[src/scheduler/flow_progress.rs]] — Flow progress tracker
- [[src/scheduler/fifo.rs]] — FIFO scheduler implementation
- [[src/scheduler/wfq.rs]] — WFQ scheduler implementation
- [[src/scheduler/drr.rs]] — DRR scheduler implementation
- [[src/scheduler/lifecycle.rs]] — Lifecycle types and accounting
- [[src/flow/cadence.rs]] — cadence heuristic implementation
- [[src/config/mod.rs#PriorityPolicy]] — priority policy configuration

# FIFO Queueing Discipline

The FIFO scheduler provides a concurrency-gated admission layer that bounds simultaneous active flows, manages request admission under a chosen backpressure policy, and guarantees that every admission slot is released on all exit paths.

## Purpose

The FIFO scheduler bounds concurrent active flows, tracks every admitted request for queue-depth and wait-time observability, and guarantees that each admission slot releases on all exit paths.

- Concurrent active flows never exceed a configured maximum.
- Every admitted request is tracked for queue depth and wait-time observability.
- Admission slots are released on all exit paths, including client disconnect and abort.
- A completion-bias gate may be evaluated before permit grants, depending on configuration.
- No fairness guarantee beyond the maximum concurrency bound.

## Non-goals

The FIFO scheduler is a single-dimensional concurrency limiter, not a multi-criteria scheduler. It does not address load distribution, KV-cache pressure, per-flow priority, or starvation avoidance.

- Prioritization or weighted allocation of admission slots ([[scheduler_policies#Priority-Aware Flow Selection]]).
- Cache-aware admission decisions ([[admission#KV-Cache-Aware Admission Gate]]).
- Multi-queue or weighted-fair scheduling ([[scheduler#Weighted Fair Queueing Discipline]], [[scheduler#Deficit Round Robin Discipline]]).
- Starvation detection or mitigation ([[scheduler_policies#Starvation Protection]]).

## Interface

The scheduler exposes admission control, queue observation, and slot-holder contracts. Each surface defines preconditions on inputs, postconditions on outputs, and the failure modes a caller must handle.

**Admission.** Accepts a flow identity and an estimated work unit; produces an admission slot or a rejection. The work unit is stored for interface compatibility but is not used by FIFO for admission decisions. The caller must hold an admission slot for the duration of a request; disposal of the slot releases the concurrency permit. Depth-based rejection occurs only in FailFast mode, when queue depth exceeds the configured limit. Hybrid mode performs only timeout-based rejection when the maximum-wait expires. Blocking mode never rejects. Rejections carry a `Retry-After` hint based on queue depth measured at the moment of rejection.

**Queue Observation.** Reports the aggregate depth of waiting flows across all registered flows. Provides raw depth data to [[flow#Flow Registry and State]] for construction of per-flow queue-position snapshots.

**Slot Holder.** The admission slot carries the flow identity and work-unit estimate for observability. Disposing the slot releases the concurrency permit, decrements the active-flows metric, decrements the per-flow active counter, and signals completion-bias waiters ([[scheduler_policies#Completion Bias Gate]]). A slot may be disarmed so that disposal becomes a no-op; disarming automatically releases the permit.

**Ticket Factory.** `make_ticket` constructs a `QueueTicket` from a flow identity, work-unit estimate, and a drop handler closure. The closure defines the actions performed when the ticket is dropped. Used by all scheduler variants ([[scheduler#FIFO Queueing Discipline]], [[scheduler#Weighted Fair Queueing Discipline]], [[scheduler#Deficit Round Robin Discipline]]) for creating admission slots.

**Construction.** Requires a maximum active-flows bound, a backpressure policy ([[admission#Backpressure and Admission Rejection]]), a queue-depth limit, and a wait-time limit. Scheduling policies ([[scheduler_policies#Request Lifecycle and Credit Restoration]]) may be supplied via the advanced constructor; the default constructor creates a disabled completion-bias gate. The configured maximum is fixed for the lifetime of the scheduler instance.

## Invariants

The scheduler maintains consistency between its internal concurrency bound, its observable queue metrics, and the lifecycle of admission slots.

**Concurrency Bound.** The number of simultaneously active flows never exceeds the configured maximum. Each admission acquires exactly one permit; each slot disposal releases exactly one.

**Queue Depth Consistency.** The reported queue depth equals the number of in-flight admission attempts (waiting for a permit, not yet admitted). Depth is incremented at entry to the admission path and decremented on admission, rejection, or cancellation.

**Metrics Alignment.** The `llm_active_flows` gauge equals the number of held admission slots. The `llm_queue_depth` gauge per flow equals that flow's depth contribution within the aggregate counter. Wait-time metrics are measured from the moment a request enters the waiting queue until it is admitted or rejected.

**FIFO Positioning.** Queue positions reflect creation order within each flow; positions are 1-indexed.

**Completion-Bias Enforcement.** A completion-bias gate ([[scheduler_policies#Completion Bias Gate]]) is checked before every permit grant when enabled. The default constructor creates a disabled gate; callers must supply an enabled gate via the advanced constructor.

## Constraints

The scheduler operates within the boundaries of its concurrency model and backpressure configuration.

**Backpressure Policy.** Exactly one backpressure mode (Blocking, FailFast, or Hybrid) is active per scheduler instance. Blocking mode may block indefinitely at both the completion-bias gate and the permit-wait. FailFast mode rejects immediately on queue-depth overflow; subsequent requests within depth limits proceed without timeout. Hybrid mode applies two sequential timeouts, each bounded by the configured maximum-wait: one for the completion-bias gate evaluation, one for the permit acquisition. Total worst-case wait can reach twice the configured timeout. Hybrid mode does not perform depth-based rejection.

**Concurrency Bound.** The maximum active flows value is immutable after construction; it cannot be increased or decreased without creating a new scheduler instance.

**Slot Release.** Disarming a slot automatically releases the concurrency permit; the caller does not retain permit-release responsibility after disarm. Double-release of a permit is prevented by the slot's internal guard.

**Queue Depth Independence.** Queue depth and semaphore capacity are measured independently; a rejection may occur even when permits are available, because depth exceeds its configured limit.

## Rationale

The FIFO scheduler separates concurrency control from admission decisions, enabling callers to choose backpressure behavior without coupling it to the slot-release mechanism.

- Concurrency gate: LLM inference is flow-intensive; bounding concurrent flows prevents resource saturation and ensures predictable queue behavior. A fixed concurrency cap provides a simple admission boundary that all scheduling algorithms share ([[scheduler#Scheduler Facade and Policy Selection]]).
- RAII slot release: admission slots must be released on all exit paths (success, error, panic, client disconnect). RAII guarantees the release happens regardless of how the request ends, eliminating leak paths from error handling.
- Automatic permit release on disarm: disarm is used when the receiver is gone (timeout, abort) and the ticket will never be explicitly disposed. Automatic release ensures the permit cannot leak in this path.
- Optional completion-bias gate ([[scheduler_policies#Completion Bias Gate]]): favors admitting requests from flows that have recently completed work, reducing head-of-line blocking under contention. It is disabled by default as the scheduler can operate correctly without it.
- Three backpressure modes ([[admission#Backpressure and Admission Rejection]]): different callers need different pressure strategies — Blocking for resilience, FailFast for depth-bounded rapid rejection, and Hybrid for bounded waiting with timeout.

## Related

See also these related concepts and source files.
- [[scheduler#Scheduler Facade and Policy Selection]] — Scheduler facade that selects among scheduling algorithms
- [[admission#Backpressure and Admission Rejection]] — Backpressure mode configuration and semantics
- [[scheduler_policies#Completion Bias Gate]] — Completion-bias gate mechanism
- [[flow#Flow Registry and State]] — Flow tracking and snapshot construction
- [[admission#Per-Flow Token Progress Tracking]] — Per-flow progress counters
- [[flow#Flow Identifier Contract]] — Flow identity model
- [[metrics#Metric Family Contracts]] — Metric family registration
- [[src/scheduler/fifo.rs]] — Source implementation
- [[src/scheduler/mod.rs]] — Module re-export

# Weighted Fair Queueing Discipline

The WFQ scheduler guarantees fair admission across competing flows by bounding concurrency, enforcing deterministic selection order, and coupling each admitted request to a lifecycle-managed ticket that returns its permit on completion.

## Purpose

The WFQ scheduler selects waiting flows by starvation bypass, priority, WFQ fairness ratio, and enqueue-time tiebreaker. Each admission returns a drop-managed ticket. Per-flow service accounting tracks fairness.

- Bound the number of concurrently active flows to a configured maximum.
- Select which waiting flow to admit next using starvation bypass, then priority, then WFQ fairness ratio, then enqueue-time tiebreaker.
- Return a ticket per admission that automatically releases its permit when dropped, regardless of success or cancellation.
- Expose three distinct backpressure behaviors — blocking, fail-fast, and hybrid — so consumers choose their preferred rejection semantics.
- Report cumulative per-flow service accounting for fairness ratio computation.

## Non-goals

The WFQ scheduler is an admission controller, not a work executor. It makes no guarantees about how work is performed, how many requests a flow submits, or how the backend processes admitted work.

- Does not execute work units or orchestrate inference.
- Does not validate or interpret work-unit estimates beyond accepting them as provided.
- Does not expose shutdown or drain; the scheduler lives for the lifetime of the process.
- Does not schedule within a flow (intra-flow ordering is outside this boundary).
- Does not rate-limit or throttle admitted flows.

## Interface

The scheduler exposes a ticket-based admission contract and read-only observability queries. All configuration is fixed at construction time.

**Admission.** `admit(flow_id, work_unit)` admits a request on behalf of the identified flow. On success returns a ticket representing a consumed permit; on rejection returns an error carrying a suggested retry duration. Backpressure mode determines rejection behavior: blocking waits indefinitely for a permit, fail-fast rejects immediately when queue depth exceeds a configured threshold, hybrid rejects after a bounded wait timeout expires. Work-unit estimate is recorded for the flow's fairness accounting and credited upon ticket drop. The flow is created automatically if it does not already exist in the [[flow#Flow Registry and State]]. Admission may be deferred by a completion bias gate ([[scheduler_policies#Completion Bias Gate]]): in blocking mode the gate blocks before the request enters the waiting queue; in hybrid mode the gate check is bounded by the wait timeout and proceeds without gate clearance on timeout. Blocking mode returns a fixed retry-after duration on error, independent of queue state. Fail-fast and hybrid modes compute retry-after from current queue depth.

**Observability.** `queue_depth` returns the sum of per-flow depth counters across all waiting flows. `queue_snapshot` returns active count, waiting count, and an ordered list of waiting flow identifiers. `service_done(flow_id)` returns the cumulative work-unit credit for a flow; returns zero for unknown flows.

## Invariants

These properties hold regardless of implementation and must survive any rewrite.

**Permit accounting.** The sum of active flows and available permits always equals the configured maximum active flows. Every admitted request consumes exactly one permit; every ticket drop returns exactly one permit. When no permits remain, no further selection or admission occurs.

**Selection ordering.** A flow waiting longer than the starvation threshold is selected before any non-starved flow, regardless of priority or WFQ ratio. Among eligible non-starved flows, the flow with the highest priority is selected first; WFQ fairness ratio (cumulative service divided by weight) breaks priority ties. When priority and WFQ ratio are equal, the flow that entered the queue first is selected. Flows with zero or negative weight are excluded from selection.

**Service accounting.** Each flow's cumulative service credit is monotonically non-decreasing. Service credit is scoped to the flow: one counter per flow, keyed by its identifier. The counter advances by the work-unit estimate recorded at admission, credited when the ticket is dropped.

**Depth consistency.** Queue depth increments by one when a flow enters the waiting queue and decrements by one when the flow is either admitted or cancelled. Depth is released at most once per queued request, regardless of whether the request is admitted, cancelled, or times out.

## Constraints

These are hard boundaries imposed by design, not implementation artifacts.

**Capacity.** Concurrency is bounded by `max_active_flows`; the scheduler never admits more active flows than this value. Queue depth can grow unbounded in blocking mode. Fail-fast mode imposes a queue depth threshold for immediate rejection. Hybrid mode rejects via bounded wait timeout; it does not check depth before queuing.

**Admission lifecycle.** A ticket must be held for the lifetime of the admitted request; dropping the ticket without consuming it is permitted and correctly releases the permit and depth. The scheduler cannot distinguish between a request that completed and one that was cancelled; both paths return a permit identically.

**Configuration immutability.** Backpressure mode, maximum active flows, and queue depth threshold are fixed at construction and cannot be changed at runtime. Starvation timeout defaults to a fixed value and is not configurable via the primary public constructor.

**Work-unit assumptions.** Work-unit estimates are assumed positive and finite; the scheduler does not validate these values.

**No external control over selection.** The scheduler provides no API to pause, drain, or shut down selection. Selection proceeds continuously for the lifetime of the scheduler.

## Rationale

The scheduler's design follows from three goals: fairness across flows, bounded concurrency, and operational flexibility under overload.

- WFQ ratios over pure priority: pure priority scheduling starves low-priority flows indefinitely. WFQ ratios (service divided by weight) ensure every flow eventually receives credit proportional to its weight, while still allowing priority to bias selection. The starvation threshold provides a hard floor against indefinite waiting.
- Ticket-based permits: coupling admission to a drop-managed ticket eliminates manual permit tracking from callers. Regardless of how a request ends — completion, cancellation, or error — the permit returns automatically. This prevents silent permit leaks that would otherwise starve the system.
- Three backpressure modes: different consumers have different tolerance for queue pressure. Blocking mode suits callers that can wait. Fail-fast mode suits health checks and rapid feedback loops. Hybrid mode offers bounded waits with computed retry intervals.
- Flow-scoped service accounting: fairness requires per-flow state. Independent service counters keyed by flow identifier allow the scheduler to track how much each flow has consumed without coordination overhead between flows.
- FIFO as the final tiebreaker: when WFQ ratios converge, enqueue time provides a stable, observable ordering. Without it, equal-ratio flows could starve each other through oscillation.

## Related

See also these related concepts and source files.
- [[flow#Flow Registry and State]] — Flow registry, flow identifiers, and per-flow state
- [[metrics#Metrics Registry]] — Counters and gauges reported by the scheduler
- [[admission#Backpressure and Admission Rejection]] — Backpressure mode definitions and retry-after computation
- [[scheduler#FIFO Queueing Discipline]] — Queue ticket and depth guard mechanisms
- [[scheduler_policies#Priority-Aware Flow Selection]] — Priority-based selection algorithm
- [[scheduler_policies#Starvation Protection]] — Starvation detection and force-admit logic
- [[scheduler_policies#Completion Bias Gate]] — Completion bias gate for admission gating
- [[src/scheduler/wfq.rs]] — WFQ scheduler implementation
- [[src/scheduler/backpressure.rs]] — Backpressure rejection types
- [[src/scheduler/fifo.rs]] — Queue ticket and depth guard mechanisms

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

**Admission.** A caller presents a flow identity and a work-unit estimate; the scheduler either grants an admission ticket or rejects with a signal containing a retry duration. See [[scheduler#FIFO Queueing Discipline]] for ticket semantics and [[admission#Backpressure and Admission Rejection]] for rejection modes. Three backpressure modes govern admission behavior: blocking (waits indefinitely for a permit), fail-fast (rejects immediately when queue depth exceeds a configured threshold), and hybrid (waits before rejecting; total wall-clock wait can reach approximately two times the configured duration due to sequential gate and channel waits). Admission includes a completion-bias gate check that can delay admission indefinitely in blocking mode. In hybrid mode, the gate check and the subsequent channel wait each consume up to the configured duration, yielding a worst-case total wait of roughly double the configured value. See [[scheduler_policies#Completion Bias Gate]]. Fail-fast mode can reject before any admission state is established; such rejections produce no depth or permit accounting side effects. Admission always ensures the requesting flow is registered; a new flow is materialized if not already present. See [[flow#Flow Registry and State]].

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
- [[scheduler#FIFO Queueing Discipline]] — QueueTicket, ticket creation and disarm
- [[scheduler_policies#Priority-Aware Flow Selection]] — Priority-based selection among eligible candidates
- [[scheduler_policies#Starvation Protection]] — Starvation detection and force-selection
- [[flow#Flow Registry and State]] — FlowId, Flow, per-flow state management
- [[metrics#Metric Family Contracts]] — Metrics gauges and counters reported by the scheduler
- [[src/scheduler/drr.rs]] — DRR scheduler implementation
- [[src/scheduler/backpressure.rs]] — Backpressure types
- [[src/scheduler/fifo.rs]] — Queue ticket implementation
- [[src/scheduler/completion_bias.rs]] — Completion bias gate
- [[src/scheduler/starvation.rs]] — Starvation detection
- [[src/scheduler/priority.rs]] — Priority selection
