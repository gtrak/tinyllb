# Priority-Aware Flow Selection

Priority-aware flow selection picks one flow from eligible candidates using a three-level ordering: priority dominates, base preference breaks ties, and enqueue time is the final tiebreaker. Empty set yields none.

## Purpose

Priority-aware flow selection picks one flow from eligible candidates using three-level ordering: priority dominates, base preference breaks ties, and enqueue time resolves remaining ties. Empty set yields no winner.

- Higher priority always wins over lower priority, unconditionally.
- Among equal priority, base preference score determines the winner; scores within a tolerance band are treated as equal.
- Among equal priority and equivalent base score, earliest enqueued flow wins.
- The winner is always one of the provided candidates, never a synthesized identity.

## Non-goals

This concept does not manage priorities, admit flows, or distribute load.

- Does not assign, modify, or derive priority values.
- Does not enforce admission gates or rate limits.
- Does not balance work across executors or partitions.
- Does not handle preemption or rescinding of selections.
- Does not guarantee that the selected flow can be executed.

## Interface

The selection contract accepts a set of candidates, each carrying a priority level, a base preference score, and a monotonic arrival time, and returns one winner identity or indicates no candidate was supplied.

**Selection Contract**

- Inputs: a set of candidates (possibly empty), each with a priority value (unsigned, larger = more urgent), a real-valued base preference score (lower = preferred), and a monotonically increasing enqueue timestamp from a single clock domain.
- Output: the identity of the single winning candidate, or absent when the candidate set is empty.
- No error conditions beyond the empty-set case; the selection surface does not propagate failures.
- The winner is always one of the provided candidates, never a synthesized identity.
- Comparison is deterministic: the same inputs always produce the same winner.
- Candidates with non-finite base scores (not-a-number or infinity) are excluded from winning; they are silently bypassed as if absent.

## Invariants

Selection ordering preserves a strict three-level hierarchy. The base-score equivalence check takes precedence over the "lower score wins" rule: scores within a tolerance band trigger the arrival-order tiebreak, not the lower-score preference.

- **Priority dominance:** A candidate with a strictly higher priority always wins regardless of base score or enqueue time.
- **Base-score direction (strict difference):** When priorities are equal and base scores differ by more than the tolerance threshold, the candidate with the lower base score wins.
- **Base-score equivalence:** Two base scores are treated as equivalent when their absolute difference does not exceed a small positive tolerance value. Equivalent scores do not trigger the "lower wins" rule.
- **Arrival-order tiebreak:** When both priority and base score are equivalent, the candidate with the earlier enqueue time wins.
- **Output membership:** The returned identity corresponds to exactly one element from the input set.

## Constraints

The ordering and selection logic is bounded by a fixed hierarchy depth and the properties of real-valued scores compared with tolerance.

- The ordering has exactly three levels; no additional tiebreakers are guaranteed beyond enqueue time.
- Base scores are real-valued; callers must supply finite scores (not not-a-number and not infinity). Non-finite scores are silently excluded from consideration rather than rejected.
- Priority is an unsigned scalar; larger values indicate higher urgency.
- Enqueue timestamps originate from a monotonic clock within a single process lifetime; they are not comparable across process restarts or distributed processes.
- Among candidates identical on all three ordering fields, any one may be selected; the specific choice is not part of the contract.

## Rationale

The three-level hierarchy balances urgency, algorithmic preference, and fairness.

- Priority dominates because urgent work must preempt lower-priority flows regardless of other metrics.
- Base score provides a secondary preference signal from DRR (round-robin cursor position) without overriding explicit priority.
- Enqueue-time tiebreak preserves fairness: among equally deserving candidates, the earliest arrival goes first.
- Tolerance-based score equivalence prevents spurious ordering differences from numerical noise, and equivalence is checked before the "lower wins" rule to avoid bypassing the arrival-order tiebreak.
- Empty-set absence (rather than error) reflects that no candidates is a valid, non-fault state.

This section lists related concepts and source references for priority-aware selection.

- [[flow#Flow Registry and State]] — flows submitted to the scheduler
- [[scheduler#Deficit Round Robin Discipline]] — the scheduler that supplies base preference scores (RR cursor position)
- [[src/scheduler/priority.rs#FlowCandidate]] — candidate identity, priority, score, and timestamp
- [[src/scheduler/priority.rs#select_best]] — selection entry point

# Starvation Protection

Starvation protection detects when a queued flow exceeds an acceptable wait time, enabling the scheduler to force-admit it regardless of selection policy. It emits observability signals for monitoring and threshold tuning.

## Purpose

Starvation detection determines when a queued flow has exceeded an acceptable wait time, enabling the scheduler to bypass normal selection policy and force-admit the flow. It emits observability signals for monitoring and threshold tuning.

- Guarantees that no flow waits indefinitely behind flows favored by a scheduling policy.
- Supplies a starvation decision based on elapsed queue time versus a timeout threshold.
- Records the observed wait duration and increments a global starvation counter when force-admission occurs.

## Non-goals

Starvation detection does not determine what threshold to use, enforce admission, or manage queue ordering.

- Does not define starvation thresholds — each consumer supplies its own timeout duration.
- Does not enforce admission — it only reports whether a flow qualifies for force-admission.
- Does not deduplicate starvation logic across different code paths; multiple independent implementations exist.
- Does not operate on flows that have not yet been enqueued.
- Does not protect against lock poisoning or clock regression — these are accepted panic conditions.

## Interface

The concept manifests in three observable code paths: a paired check-and-record operation used by fair-share schedulers, a combined operation in the completion bias gate, and a standalone metrics function.

**Starvation determination**

- Accepts a flow and a timeout duration; returns the observed wait when the flow has exceeded the threshold, or nothing when the flow is not starved.
- A flow without a recorded enqueue instant is never starved.
- Equality with the threshold does not constitute starvation — the wait must strictly exceed the timeout.
- Measured against monotonic time, not wall clock.
- Returns the actual wait duration, enabling the caller to record metrics.

**Metrics recording**

- Accepts a metrics handle, a flow, and the observed wait duration as preconditions; all three must be supplied by the caller.
- Records the wait as a gauge keyed by flow identity and increments a global starvation force-admit counter.
- Side-effect only — produces no return value usable by the caller.
- Carries no declared error contract; depends on the supplied metrics handle remaining valid.

**Completion bias gate check**

- Independently determines starvation and records metrics in a single operation.
- Uses an internal timeout duration rather than accepting a caller-supplied threshold.
- Returns a binary decision — starved or not — rather than exposing the wait duration.
- Emits the same two metrics (flow-starvation gauge, force-admit counter) as the paired operation.
- Serves a different access pattern: the gate combines check-and-record inline rather than separating them.

## Invariants

These conditions hold across all code paths regardless of implementation.

- A flow without a recorded enqueue instant never satisfies the starvation predicate.
- Starvation requires the wait to strictly exceed the threshold, not merely reach it.
- Wait duration is always derived from a monotonic clock, making it immune to wall-clock adjustments.
- The starvation decision is stateless — it depends only on the current instant, the recorded enqueue time, and the applicable threshold.
- All code paths emit the same two metrics: per-flow starvation duration gauge and global force-admit counter.

## Constraints

Boundaries within which starvation detection must operate.

- Accessing the enqueue instant may panic if concurrent access corrupts the flow's synchronization state.
- If the monotonic clock reports a time before the recorded enqueue instant, duration computation will panic — no guard against clock regression exists.
- The timeout threshold is caller-supplied; no code path validates or constrains the range of acceptable values.
- The panic surfaces above apply to all three code paths, not only the starvation module's public functions.

## Rationale

Starvation detection exists to prevent fairness regressions in schedulers that can, under certain load patterns, indefinitely defer certain flows.

- Without a starvation safety net, priority-based schedulers can starve lower-priority flows under sustained high-throughput conditions.
- A per-call threshold allows each consumer to tune its own starvation tolerance without hard-coding a single policy.
- Metric emission enables operators to observe starvation frequency and tune thresholds reactively.
- The completion bias gate's inline implementation reflects a different access pattern where combining check and record into one operation is more efficient than the two-step pattern used by fair-share schedulers.

This section lists related concepts and source references for starvation protection.

- [[flow#Flow Registry and State]] — flow lifecycle and enqueue semantics
- [[scheduler#Scheduler Facade and Policy Selection]] — scheduler selection policies that consume starvation signals
- [[scheduler_policies#Completion Bias Gate]] — independent completion bias gate starvation path
- [[src/scheduler/starvation.rs]] — starvation check and metrics recording
- [[src/scheduler/drr.rs]] — DRR scheduler invocation of starvation check
- [[src/scheduler/completion_bias.rs]] — independent completion bias gate starvation path
- [[src/flow/mod.rs#Flow]] — flow struct with enqueue instant field

# Completion Bias Gate

The completion bias gate defers admission of new flows when active flows reach a configured target, guaranteeing eventual admission for every flow while preferencing completion of active work.

## Purpose

The completion bias gate controls admission by deferring new flows when active count reaches the configured threshold, while guaranteeing eventual admission and preferencing completion of active work.

- Admits flows immediately when the active flow count is below the effective target.
- Defers admission of new flows when the active flow count meets or exceeds the effective target.
- Guarantees eventual admission through a starvation timeout, provided the flow carries a valid enqueued timestamp.
- Permits predictive admission of new flows when any active flow has completed at least ninety percent of its estimated token output.
- Allows flows that are already active to bypass the gate entirely.

## Non-goals

This concept does not address scheduling priorities, flow ordering, or throughput optimization beyond the completion-bias mechanism itself.

- Does not determine which waiting flow is admitted first when multiple flows are blocked.
- Does not enforce fairness among waiting flows beyond the starvation timeout fallback.
- Does not track or limit total queue depth or waiting flow count.
- Does not participate in flow selection, batching, or dispatch decisions.
- Does not manage the lifecycle of flows outside the admission gate.

## Interface

The gate exposes an admission surface that returns control to the caller when a flow is permitted to proceed. All contracts are stated in domain terms.

**Admission**

- A caller presenting a flow receives admission either immediately or after an asynchronous wait.
- A new flow is admitted immediately when the active flow count is below the effective target.
- A new flow is admitted immediately when any active flow has delivered at least ninety percent of its estimated tokens.
- A flow that is already active bypasses the gate without evaluation.
- Admission is eventual: the gate never rejects a flow under normal operation; it only delays admission.

**Configuration**

- The target active flow count determines the threshold at which new flows are deferred.
- When the target is zero, the maximum active flow count becomes the effective target for deferral decisions; only when both are zero does the gate never wait.
- The maximum active flow count provides a fallback threshold when the configured target is zero.
- An enabled flag controls whether the gate operates or bypasses all flows immediately.
- Starvation timeout defines the maximum duration a new flow waits before being force-admitted.
- Predictive admit toggle enables or disables early admission based on active flow completion progress.

**Preconditions**

- Flows must carry an enqueued timestamp for starvation protection to activate; flows without one rely solely on active-count drops or predictive admit.

**Notification**

- The gate wakes all blocked callers when the active flow count changes.
- Missed notifications are tolerated because the starvation timeout provides a fallback; callers re-evaluate admission on each wake.

## Invariants

These statements hold regardless of implementation. Every admissible rewrite must preserve them.

- An active flow never waits at the gate; admission is always immediate for active flows.
- When the gate is disabled, all flows pass through without evaluation.
- The effective target equals the configured target when the configured target is non-zero; when the configured target is zero, it equals the maximum active flow count.
- The predictive admit threshold is fixed at ninety percent of estimated tokens delivered.
- The gate never produces an error result under normal operation.

## Constraints

The gate operates within these boundaries and cannot guarantee correctness beyond them.

- Admission decisions may observe stale active flow counts due to concurrent updates between read and decision.
- A blocked flow relies on the starvation timeout as its fallback if notification is missed.
- Force admission of a starving flow is not coordinated with other blocked flows and may exceed the active flow target.
- A poisoned lock on the enqueued timestamp value causes a runtime failure that precludes admission.
- Flows lacking an enqueued timestamp have no starvation safety net and may wait indefinitely under sustained saturation.
- Predictive admit evaluates per-flow progress independently; partial completion of one flow can trigger admission of another.
- The starvation re-check interval is currently derived as one quarter of the starvation timeout; this ratio is an implementation detail, not a domain invariant.

## Rationale

The completion bias gate trades admission throughput for predictable per-flow latency. Limiting concurrent flows prevents resource contention, while the starvation and predictive mechanisms prevent the limit from becoming a hard bottleneck.

- Deferring new flows during saturation reduces tail latency for already-running work.
- Starvation timeout ensures no flow with a valid enqueued timestamp is indefinitely blocked under sustained saturation.
- Predictive admit amortizes the cost of the target limit by overlapping new work with near-completion flows.
- Active flows bypass the gate to avoid self-inflicted contention on their own continuation.
- Zero target defaults to the maximum active flow count, preserving operational bounds when no explicit target is configured.

This section lists related concepts and source references for the completion bias gate.

- [[flow#Flow Registry and State]] — flow registration and lookup
- [[admission#Per-Flow Token Progress Tracking]] — per-flow token delivery tracking
- [[metrics#Metrics Registry]] — scheduler metrics collection
- [[scheduler_policies#Starvation Protection]] — starvation timeout used as fallback for eventual admission
- [[src/scheduler/completion_bias.rs#CompletionBiasGate]] — gate implementation
- [[src/scheduler/flow_progress.rs#FlowProgressTracker]] — progress tracking
- [[src/flow/mod.rs#Flow]] — flow type
- [[src/metrics/mod.rs]] — metrics infrastructure

# KV-Cache-Aware Selection Bias

Reorders selection among eligible waiting flows under KV-cache pressure so the flow with the largest resident KV footprint wins the next permit — never rejects or delays.

Under KV-cache pressure, the scheduler reorders selection among *eligible* waiting flows so the flow holding the largest resident KV footprint is granted the next permit, letting it finish and free blocks rather than being preempted and paged into/out of CPU-offloaded KV cache. This is a scheduling bias: it never rejects or delays a request, it only picks which eligible flow wins a permit.

## Purpose

Reduce KV-cache thrash under pressure by continuing the flow that has already invested the most resident cache, instead of fairly rotating to a cold flow whose admission would evict the hot one.

- Reorders DRR's `try_select` Phase 3 selection.
- Footprint per flow = delivered tokens tracked by [[admission#Per-Flow Token Progress Tracking]].
- Pressure is the backend's global KV usage gauge from [[backend#Backend KV-Cache Monitor]].
- Bias strength ramps linearly from 0 below `pressure_below` to full dominance at/above `bias_full_at`; when all footprints are equal it collapses to existing priority→base→enqueue fairness.

## Non-goals

This bias is not an admission control mechanism.

- Does not reject, delay, or 429 any request (contrast [[admission#KV-Cache-Aware Admission Gate]]).
- Does not source per-flow KV block counts (vLLM exposes only a global gauge); footprint is a proxy via delivered tokens.
- Applies only to DRR selection among waiting flows; it has no other scheduling path to influence.

## Interface

The bias handle ([`KvBiasHandle`](src/scheduler/kv_bias.rs)) is constructed in [`Scheduler::new`](src/scheduler/mod.rs) from the `KvBias` config, the backend monitor, and the shared `FlowProgressTracker`, then passed into DRR's admission loop and `try_select`.

- `pressure() -> f64` reads the latest backend snapshot's `kv_usage`, clamped to `[0,1]`.
- `bias_weight(pressure) -> f64` ramps `0` below `pressure_below`, `1` at/above `bias_full_at`.
- `footprint(flow_id) -> f64` returns the flow's currently-delivered tokens (0 if unknown).
- `select(candidates, pressure) -> Option<FlowId>` selects the best candidate by weighted footprint, falling back to [[scheduler_policies#Priority-Aware Flow Selection]] order on ties. When bias is disabled or pressure is 0, it delegates to `priority::select_best`.

## Invariants

The bias reorders eligible flows only; it never changes permit accounting or admission gating.

- Selection only reorders *eligible* flows; it never changes permit accounting or admission gating.
- When `bias_weight` is 0 (low pressure) or all candidate footprints are equal, the selected flow is identical to `priority::select_best` — no fairness regression.
- Starvation force-selection ([[scheduler_policies#Starvation Protection]]) still takes precedence over the bias.

## Constraints

The bias uses a delivered-token proxy and a global pressure gauge.

- Footprint is a proxy, not a direct KV-block count; a flow that has delivered many tokens is assumed to hold proportionally more resident cache.
- `bias_full_at` must be >= `pressure_below`; otherwise both clamp to `pressure_below` (bias disabled).
- The bias is consulted on every selection; pressure is read once per selection round from the watch channel.

## Rationale

Letting the high-footprint flow finish under pressure avoids the paging-in/out cost of CPU-offloaded KV cache, while keeping the existing fairness ordering when pressure is low or footprints are equal.

- Delivered tokens as the footprint signal requires no tokenizer dependency and is already streamed via [[admission#Per-Flow Token Progress Tracking]].
- A continuous weight (rather than a hard threshold) avoids a cliff where selection flips between fair and footprint-ordered as pressure hovers near the threshold.
- Reusing the shared `FlowCandidate` + `select_best` path keeps DRR consistent.

## Related

Cross-concept links and source references for the KV-cache-aware selection bias.

- [[admission#Per-Flow Token Progress Tracking]] — delivered-token source
- [[backend#Backend KV-Cache Monitor]] — global pressure gauge
- [[scheduler#Deficit Round Robin Discipline]] — DRR selection integration
- [[scheduler_policies#Priority-Aware Flow Selection]] — fairness fallback ordering
- [[scheduler_policies#KV-Cache-Aware Selection Bias]] — configuration
- [[src/scheduler/kv_bias.rs#KvBiasHandle]] — bias implementation
- [[src/scheduler/priority.rs]] — `FlowCandidate`, `select_best`, `cmp_fair`

# Request Lifecycle and Credit Restoration

The scheduler lifecycle guard accounts for every request at termination with correct metrics and scheduling credit. It reconciles estimated cost with actual delivered work so fair schedulers receive accurate charge-and-credit information.

## Purpose

The scheduler lifecycle guard accounts for every scheduled request at termination, emitting correct metrics and reconciling estimated cost with actual delivered work so fair schedulers receive accurate charge-and-credit information.

- Emits exactly one terminal lifecycle event — either normal completion or cancellation — at scope exit.
- Reports accounting to the scheduler so that DRR credit reflects actual delivered work, not estimates.
- Registers the request with a progress tracker at construction and unregisters at termination for predictive admission.
- Publishes per-token delivery events as tokens arrive during the stream's lifetime.

## Non-goals

This concept does not govern scheduling decisions, token generation, or request admission logic.

- Does not decide whether to admit, schedule, or preempt a request.
- Does not generate tokens or communicate with the inference backend.
- Does not determine cost estimates — it only reconciles estimates against delivered work.
- Does not persist lifecycle state beyond the current process lifetime.

## Interface

The lifecycle guard exposes five contractual surfaces: construction, token event recording, delivered-token reporting, completion marking, and automatic termination accounting.

**Construction**

- Accepts a request identifier, estimated cost, and shared references to the scheduler, metrics, and an optional progress tracker.
- Guarantees a `request_started` event is emitted and the request is registered with the progress tracker (if present) before returning.

**Token Event Recording**

- Records that a token was delivered during the request's lifetime.
- Guarantees the per-token `token_received` event counter increments by one per invocation.
- Does not affect cumulative delivered-token count or accounting calculations.

**Delivered Token Reporting**

- Accepts an additive token count and increases the cumulative delivered-token total by that amount.
- Guarantees the flow progress tracker is updated with the same count (if present), enabling real-time predictive admission adjustments.
- Is the primary mechanism by which usage-frame data feeds into termination accounting.

**Completion Marking**

- Marks the request as normally completed.
- Guarantees the terminal event at scope exit reflects normal completion rather than cancellation.

**Termination Accounting**

- Triggers automatically when the guard goes out of scope — no explicit call required.
- Emits either `request_completed` or `request_cancelled` depending on whether completion was marked.
- Reports an `AccountingReport` to the scheduler with variant-dependent fields:
  - Normal completion: reports both the delivered token count and restore cost.
  - Cancellation: reports only the restore cost (delivered tokens are not included in this variant).
- When the cumulative delivered-token count is zero on normal completion (no usage data received), falls back to charging the full estimated cost with zero restore credit; the `delivered_tokens` field of the report is set to the estimated cost, not zero.
- When delivered tokens exceed the estimated cost on normal completion, emits a tracing warning containing the flow identifier, delivered tokens, estimated cost, and overrun amount.
- When the cumulative delivered-token count is zero on normal completion, emits a tracing warning containing the flow identifier and estimated cost.
- Always unregisters the request from the progress tracker, passing the estimated cost and final delivered count (if present).

## Invariants

The guard maintains consistent state between construction and termination, ensuring accounting is always derivable from the sequence of operations.

- The completion flag is unset at construction and, once set, never reverts — marking completion is idempotent.
- Delivered token count is intended to be monotonically non-decreasing from zero — this is the semantic contract, but the implementation does not enforce it against negative increments.
- On normal completion with delivered tokens greater than zero, restore credit equals estimated cost minus delivered tokens; net DRR charge equals negative delivered tokens (credit reflects actual work).
- On cancellation, restore credit equals estimated cost minus delivered tokens, saturated at zero; net DRR charge equals delivered tokens capped at the estimated cost — the cap is an arithmetic consequence of the saturation, not a separate design bound.
- The request is unregistered from the progress tracker at termination regardless of completion status, carrying the estimated cost and final delivered count.

## Constraints

The guard operates within the boundaries of scope-lifetime tracking and scheduler-specific accounting behavior.

- Accounting reports are passed to the DRR scheduler; it adjusts per-flow credit based on actual delivered tokens.
- Termination accounting depends on the guard being dropped — if the guard value is leaked, neither metrics nor accounting fire and the progress tracker retains the request indefinitely.
- Over-delivery (delivered tokens exceeding the estimate) on normal completion produces a negative restore cost, applying an additional debit; on cancellation, over-delivery silently clamps restore to zero.
- When no usage data arrives by scope exit, the full estimated cost is charged with no restore on normal completion — this Phase-1 limitation trades precision for safety.
- The delivered-token update interface accepts any signed integer without validation — the monotonicity invariant stated above is an intended property, not an enforced constraint.
- The progress tracker is optional — construction, delivered-token updates, and termination behave correctly without it.

## Rationale

RAII-scoped termination eliminates the need for explicit cleanup calls across multiple error paths and prevents accounting drift.

- Scope-exit accounting ensures every request — even those terminated by panic, cancellation, or timeout — is accounted for.
- Separating completion marking from termination enables the system to distinguish intentional completion from forced cancellation.
- Reporting actual delivered tokens rather than estimates prevents systematic credit inflation in fair-scheduling algorithms.
- The progress tracker updates on every delivered-token report, enabling incremental predictive admission adjustments rather than relying solely on termination-time registration.
- The zero-delivery fallback charges the full estimate as a conservative bound, ensuring credit cannot inflate when usage data is absent.

This section lists related concepts and source references.

- [[scheduler_policies#Completion Bias Gate]] — uses progress tracker for predictive admission triggered by lifecycle updates
- [[scheduler#Deficit Round Robin Discipline]] — DRR scheduling and credit system consuming accounting reports
- [[metrics#Metrics Registry]] — metrics instrumentation for lifecycle events
- [[admission#Per-Flow Token Progress Tracking]] — predictive admission progress tracking
- [[scheduler_policies#Starvation Protection]] — starvation timeout interacts with flow lifecycle boundaries
- [[src/scheduler/lifecycle.rs#LifecycleGuard]] — lifecycle guard implementation
- [[src/scheduler/lifecycle.rs#AccountingReport]] — accounting report contract
- The `event` module in the same file defines lifecycle event constants (see `src/scheduler/lifecycle.rs:event`).
- `src/scheduler/mod.rs:report_accounting` — scheduler accounting entry point
