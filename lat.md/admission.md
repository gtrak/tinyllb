# Backpressure and Admission Rejection

Error signaling and retry-after computation for queue-full backpressure. Defines a rejection error type with a suggested wait duration, computes scaled retry-after values, and maps backpressure modes to stable labels.

## Purpose

This concept defines the contract for rejecting requests when the scheduler queue is at capacity and computing retry-after durations that scale with queue pressure.

- Provides a single error signal that represents queue-full rejection
- Suggests a retry-after duration proportional to how saturated the queue is
- Maps each backpressure mode variant to a stable label for observability
- Establishes a lower bound on retry-after equal to the configured base duration

## Non-goals

This concept does not decide when backpressure should be applied, nor does it manage queue state or scheduling policy.

- Does not determine which backpressure mode is active — that is a configuration concern
- Does not enforce backpressure on any scheduler — only computes values consumed by [[scheduler#Scheduler Facade and Policy Selection]]
- Does not define blocking or hybrid behavior — only the fail-fast retry-after computation
- Does not rate-limit, throttle, or shape traffic

## Interface

The public contract consists of an error type for rejection signaling, a retry-after computation based on queue pressure, and a mode-to-label mapping for metrics instrumentation.

**Rejection Error.** Callers receive a rejection error when the queue is full. The error carries a retry-after duration that represents the minimum suggested wait before resubmitting.

- The error is inspectable for its retry-after duration via a public field
- The error displays a human-readable message indicating queue-full rejection
- The error is cloneable for propagation across boundaries
- The error implements the standard error trait for use in error-chain APIs
- The error is debuggable for diagnostic output

**Retry-After Computation.** Given current queue depth, maximum queue depth, and a base retry-after duration, the computation returns a scaled retry-after duration that increases as the queue fills.

- Accepts queue depth, capacity, and a base duration as inputs
- Returns a duration that is always at least the base duration
- Returns exactly three times the base duration when capacity is zero
- The retry-after duration scales affinely with the ratio of depth to capacity (factor = 1 + depth/capacity)
- When depth exceeds capacity, the factor exceeds two and grows without upper bound; this is not an error condition

**Mode Label Mapping.** Each backpressure mode variant maps to a canonical string label used for metrics and logging.

- Every backpressure mode variant has exactly one label
- Labels are stable across implementation changes
- Adding a new mode variant requires an explicit label mapping

## Invariants

These properties hold regardless of implementation details and must survive complete rewrite.

- The computed retry-after duration is never less than the provided base duration
- The computation never produces an undefined value for any valid input, including zero capacity
- Every backpressure mode variant has a corresponding label; the mapping is exhaustive
- The retry-after duration grows monotonically with queue depth, holding capacity constant

## Constraints

These are the operational boundaries within which the concept functions.

- Retry-after scaling is used for fail-fast queue-depth rejections, hybrid max-wait timeout rejections, and hybrid/fail-fast KV delay-timeout rejections; blocking mode never rejects and does not use the computed value
- Queue depth and capacity are non-negative integers; negative inputs are ill-formed
- Zero-capacity configurations produce a higher multiplier (3× base) than a fully loaded queue (2× base)
- The concept assumes the queue depth does not typically exceed capacity; over-capacity depth produces a ratio greater than one and scales without upper bound but is not an error condition

## Rationale

These decisions exist to ensure predictable client behavior under load and to maintain observability across backpressure modes.

- Affine scaling of retry-after with queue pressure prevents thundering-herd retries when the queue is saturated while preserving a guaranteed minimum wait
- Guaranteeing a minimum retry-after equal to the base duration prevents clients from retrying instantaneously under any conditions
- Zero-capacity configurations produce a higher multiplier than full-queue pressure to avoid undefined arithmetic without requiring callers to validate capacity beforehand
- Exhaustive mode-to-label mapping is compile-time enforced so metrics remain consistent when modes evolve
- A single rejection error type ensures all queue-full failures are distinguishable from processing errors

## Related

Related concepts and source locations for backpressure and admission rejection.
- Backpressure mode variants defining the modes this concept maps to labels
- [[scheduler#Scheduler Facade and Policy Selection]] — consumes retry-after values and rejection errors
- [[src/scheduler/backpressure.rs]] — source implementation
- [[src/config/mod.rs]] — `BackpressureMode` enum definition

# KV-Cache-Aware Admission Gate

An admission gate that decides whether to admit, delay, or reject requests based on aggregate KV-cache pressure. Two-level thresholds provide graceful degradation when the backend cache approaches capacity.

## Purpose

The KV-admission policy governs whether incoming requests are admitted, delayed, or rejected based on current key-value cache pressure from the backend.

- Admits requests when KV-cache usage is within normal operating range
- Delays requests when usage exceeds a configured delay threshold, holding them until pressure subsides
- Rejects requests with a `Retry-After` hint when usage exceeds a configured reject threshold
- Exposes a count of delayed requests observable by queue-depth queries
- Records admission decisions for metrics collection

## Non-goals

This concept does not address scheduling fairness, ordering, or resource allocation beyond the admission gate.

- Does not decide which admitted request runs next — that is the domain of [[scheduler#Deficit Round Robin Discipline]]
- Does not manage KV-cache eviction or memory allocation — that is the domain of the backend runtime
- Does not provide per-request or per-tenant admission control — decisions are based on aggregate cache pressure
- Does not implement rate limiting independent of KV-cache state

## Interface

The policy exposes one admission gate, a decision type, a configuration surface split across two structures, and one observable counter.

**Admission Gate.** The gate method returns a `Result` where success indicates admission and failure carries a backpressure rejection with a `Retry-After` duration.

- `Ok(())` is returned when the request is admitted directly or a delay wait completes within bounds
- `Err(BackpressureRejected { retry_after })` is returned when the request must be back-pressured, carrying the backoff duration
- When the policy is enabled, records the initial decision outcome to the metrics subsystem before returning

**Decision Type (`KVMDecision`).** A public enum with three variants representing the initial admission outcome before any delay-wait resolution.

- **Accept** — KV pressure is at or below the delay threshold — proceed immediately
- **Delay** — KV pressure exceeds the delay threshold — request enters a delay wait
- **Reject(Duration)** — KV pressure exceeds the reject threshold — return rejection with the embedded duration as `Retry-After`

**Configuration.** The admission thresholds (`enabled`, `delay_threshold`, `reject_threshold`) are carried in a dedicated policy config: defaults are `enabled: false`, `reject_threshold: 0.95`, `delay_threshold: 0.80`.

- The wait behavior (`backpressure_mode`, `max_wait`, `retry_after_base`, `max_queue_depth`) is carried in a separate backpressure config and threaded into the policy at construction time
- If disabled, the gate always admits — thresholds and modes are ignored

**Delayed-Request Counter.** Returns the current number of requests held in a delay wait at the time of the query.

- Increments when a request enters a delay wait and decrements when it exits (by success, timeout, or cancellation)
- Approximate counts are acceptable due to relaxed memory ordering

## Invariants

These statements hold regardless of implementation details and survive a complete rewrite of the admission logic.

**Threshold Ordering.** The delay threshold must be strictly less than the reject threshold for the delay path to be reachable. This is a configuration requirement, not an enforced invariant — misconfigured thresholds may silently render the delay path unreachable.

**Delayed-Count Consistency.** The reported delayed count reflects the number of requests currently in a delay wait.

- The count decrements on every exit path from a delay wait — success, timeout, and cancellation are all covered

**Safe Degradation.** When the backend monitor becomes unavailable, the gate admits by default rather than rejecting. This applies both when the monitor is unavailable at the initial check and when it closes during an active delay wait.

**Metrics Completeness.** When the policy is enabled, every admission decision produces exactly one metrics record with a label identifying the outcome. The disabled path bypasses all metrics recording. The metrics label corresponds to the initial decision, not to the final disposition of a delayed request that later times out.

## Constraints

Operational and configurational boundaries that shape the design space.

- Backpressure mode determines whether delay waits can be unbounded: blocking mode allows indefinite waits, while other modes enforce a timeout
- `Retry-After` on reject-scale overflows follows the formula 5 s base + (excess × 10 s), where excess is the fraction of KV usage above the reject threshold. These constants are hardcoded
- `Retry-After` on delay-timeout is computed from the delayed-request count relative to the configured maximum
- The policy operates on a single aggregate KV-usage value — per-segment or per-key granularity is not available
- At exactly the delay threshold value, the request is admitted (not delayed). At exactly the reject threshold value, the request is delayed (not rejected). Strict inequality governs both comparisons
- Underflow may wrap silently if decrement operations exceed increment operations on the delayed count

## Rationale

Admission gating is necessary because KV-cache exhaustion degrades all concurrent requests, not just the one that triggered overflow.

- Two-level thresholds (delay then reject) provide graceful degradation — mild pressure causes waits, severe pressure causes rejections
- The monitor-closed fallback admits by default because a missing signal is less dangerous than wholesale rejection
- `Retry-After` scales with excess pressure so callers receive proportional backoff rather than fixed delays
- Counting delayed requests lets downstream observability correlate admission waits with queue depth
- The policy is independently toggleable — disabling it restores unconditional admission for debugging or low-pressure environments

## Related

Related concepts and source locations for KV-cache admission.
- [[backend#Backend KV-Cache Monitor]] — provides KV-cache usage snapshots consumed by admission decisions
- [[metrics#Metrics Registry]] — receives admission decision records
- [[admission#Backpressure and Admission Rejection]] — defines rejection semantics and `Retry-After` computation
- [[scheduler#Scheduler Facade and Policy Selection]] — consumes the delayed-count for queue-depth queries
- [[src/scheduler/kv_admission.rs]] — implementation
- [[src/scheduler/backpressure.rs]] — rejection types and retry-after computation
- [[src/config/mod.rs]] — policy configuration and backpressure config

# Per-Flow Token Progress Tracking

Aggregate token progress tracking per flow. Maintains estimated and delivered token counts per flow so the scheduler can determine whether a flow has nearly completed, without exposing per-request bookkeeping.

## Purpose

The system maintains aggregate progress state per flow so that the scheduler can determine whether a flow has nearly completed. Each flow is characterized by two quantities: estimated tokens (work expected) and delivered tokens (work completed).

- Tracks cumulative estimated and delivered tokens per flow across the set of active requests belonging to that flow
- Answers whether a specific flow is near completion relative to a fractional threshold
- Answers whether any tracked flow is near completion relative to a fractional threshold
- Allows flows to enter and leave the tracker as requests are admitted and released
- Preserves state across multiple incremental updates without requiring full recomputation

## Non-goals

This concept does not manage scheduling decisions, request admission, or token budgets.

- Does not admit, schedule, or evict requests; only tracks progress for flows already admitted
- Does not validate incoming values; trusts callers to supply accurate and consistent counts
- Does not persist state beyond the lifetime of the scheduler
- Does not produce metrics, diagnostics, or observability output
- Does not coordinate access across scheduler instances; operates within a single scheduler

## Interface

The tracker exposes operations to register flows, update progress, remove flows, and query near-completion status.

**Register flow.** A flow identifier and an estimated token count for a newly admitted request are required to create or accumulate into an existing flow entry.

- **Preconditions:** A flow identifier and an estimated token count for a newly admitted request
- **Postconditions:** The flow exists in the tracker with the estimated count added to its aggregate
- **Behavior:** Creates a new entry if the flow does not already exist; otherwise accumulates into the existing estimate

**Update delivered tokens.** A flow identifier and a delta of delivered tokens are required to increment the flow's delivered aggregate.

- **Preconditions:** A flow identifier and a delta of delivered tokens
- **Postconditions:** The flow's delivered aggregate is incremented by the delta
- **Behavior:** If the flow has no entry, the delta is silently discarded and no entry is created

**Unregister flow.** A flow identifier and the request's estimated and delivered token counts are required to subtract from the flow's aggregates.

- **Preconditions:** A flow identifier and the request's estimated and delivered token counts
- **Postconditions:** Both counts are subtracted from the flow's aggregates; the entry is removed if both aggregates reach zero
- **Behavior:** If the flow has no entry, the operation is a silent no-op. Subtraction saturates at zero; residual entries persist if subtraction does not reach zero on both aggregates

**Query near-completion.** A flow identifier and a fractional threshold determine whether the delivered-to-estimated ratio meets or exceeds the threshold.

- **Preconditions:** A flow identifier and a fractional threshold
- **Postconditions:** Returns whether the delivered-to-estimated ratio meets or exceeds the threshold
- **Behavior:** Returns false if the flow has no entry or its estimated aggregate is non-positive. The ratio reflects the raw arithmetic of delivered divided by estimated; negative delivered values produce negative ratios

**Query any near-completion.** A fractional threshold determines whether at least one tracked flow satisfies the near-completion condition.

- **Preconditions:** A fractional threshold
- **Postconditions:** Returns whether at least one tracked flow satisfies the near-completion condition
- **Behavior:** Returns false if the tracker contains no entries or no entry satisfies the threshold

**Query delivered tokens.** A flow identifier determines the delivered tokens currently tracked for that flow.

- **Preconditions:** A flow identifier
- **Postconditions:** Returns the flow's delivered-token aggregate, or zero if the flow has no entry

**Initialize.** An empty tracker with no tracked flows is created.

- **Preconditions:** None
- **Postconditions:** An empty tracker with no tracked flows

## Invariants

The tracker maintains consistency between registration, updates, and removal.

**Aggregate monotonicity.** Estimated tokens remain non-negative for valid registration inputs, while delivered tokens may become negative from update deltas but saturate at zero from unregister.

- Estimated tokens remain non-negative provided callers only supply non-negative values during registration
- Delivered tokens may become negative when negative deltas are applied during updates; the tracker does not enforce a lower bound
- Subtraction during unregister saturates at zero, so estimated and delivered cannot go negative from unregister alone

**Entry lifecycle.** An entry is removed from the tracker if and only if both its estimated and delivered aggregates are zero after unregister.

- Entries persist across updates; removal requires an explicit unregister operation

**Near-completion semantics.** A flow satisfies the near-completion condition when its delivered-to-estimated ratio meets or exceeds the threshold and the estimated aggregate is positive.

- A flow with no entry or a non-positive estimated aggregate never satisfies the near-completion condition

**Update isolation.** Updating delivered tokens for an unknown flow does not create an entry, and each update affects only the targeted flow.

- Each update affects only the targeted flow; no other flow's aggregates change

## Constraints

The tracker is designed for a single scheduler instance and accepts external inputs without validation.

- Operates on a single scheduler; no cross-instance state or replication
- Accepts arbitrary deltas for delivered tokens; negative deltas reduce the delivered aggregate without saturation
- The near-completion threshold is expected in the zero-to-one range but is not enforced; callers may supply any threshold value
- Aggregate subtraction in unregister saturates at zero; mismatched estimates and deliveries leave stale residue rather than producing negative values from unregister
- Near-completion compares a floating-point ratio against the threshold; boundary precision varies with the threshold value and aggregate magnitudes

## Rationale

Aggregate tracking replaces per-request bookkeeping and enables efficient threshold queries without scanning individual requests.

- Aggregating by flow rather than by request avoids per-request scans when evaluating near-completion across a batch
- Saturating subtraction in unregister prevents negative aggregates from propagating through estimate mismatches, keeping the tracker in a valid state even when callers supply inconsistent values
- Silent no-op on unknown flows in both update and unregister avoids spurious entries from late, duplicate, or out-of-order messages
- Zero-removal keeps the tracker bounded by removing completed flows while retaining partial entries that represent in-flight requests
- Floating-point thresholds provide a flexible completion signal; the caller controls sensitivity through the threshold parameter

## Related

Related concepts and source locations for per-flow token progress tracking.
- [[scheduler_policies#Completion Bias Gate]] — consumes near-completion signals from this tracker
- Flow identity and grouping used as the key for progress tracking
- [[scheduler_policies#Request Lifecycle and Credit Restoration]] — the lifecycle guard that registers requests with the progress tracker at construction and unregisters at termination
- [[src/scheduler/flow_progress.rs]] — implementation of flow progress tracking
