# Scheduler Lifecycle Guard

## Purpose

The scheduler lifecycle guard guarantees that every scheduled request stream is accounted for at termination, with correct metrics and scheduling credit regardless of how the stream ends. It bridges the gap between estimated scheduling cost and actual delivered work so that fair-scheduling algorithms receive accurate charge-and-credit information.

- Emits exactly one terminal lifecycle event — either normal completion or cancellation — at scope exit
- Reports accounting to the scheduler so that DRR credit reflects actual delivered work, not estimates
- Registers the request with a progress tracker at construction and unregisters at termination for predictive admission
- Publishes per-token delivery events as tokens arrive during the stream's lifetime

## Non-goals

This concept does not govern scheduling decisions, token generation, or request admission logic.

- Does not decide whether to admit, schedule, or preempt a request
- Does not generate tokens or communicate with the inference backend
- Does not determine cost estimates — it only reconciles estimates against delivered work
- Does not persist lifecycle state beyond the current process lifetime

## Interface

The lifecycle guard exposes five contractual surfaces: construction, token event recording, delivered-token reporting, completion marking, and automatic termination accounting.

### Construction

- Accepts a request identifier, estimated cost, and shared references to the scheduler, metrics, and an optional progress tracker
- Guarantees a `request_started` event is emitted and the request is registered with the progress tracker (if present) before returning

### Token Event Recording

- Records that a token was delivered during the request's lifetime
- Guarantees the per-token `token_received` event counter increments by one per invocation
- Does not affect cumulative delivered-token count or accounting calculations

### Delivered Token Reporting

- Accepts an additive token count and increases the cumulative delivered-token total by that amount
- Guarantees the flow progress tracker is updated with the same count (if present), enabling real-time predictive admission adjustments
- Is the primary mechanism by which usage-frame data feeds into termination accounting

### Completion Marking

- Marks the request as normally completed
- Guarantees the terminal event at scope exit reflects normal completion rather than cancellation

### Termination Accounting

- Triggers automatically when the guard goes out of scope — no explicit call required
- Emits either `request_completed` or `request_cancelled` depending on whether completion was marked
- Reports an `AccountingReport` to the scheduler with variant-dependent fields:
  - Normal completion: reports both the delivered token count and restore cost
  - Cancellation: reports only the restore cost (delivered tokens are not included in this variant)
- When the cumulative delivered-token count is zero on normal completion (no usage data received), falls back to charging the full estimated cost with zero restore credit; the `delivered_tokens` field of the report is set to the estimated cost, not zero
- When delivered tokens exceed the estimated cost on normal completion, emits a tracing warning containing the flow identifier, delivered tokens, estimated cost, and overrun amount
- When the cumulative delivered-token count is zero on normal completion, emits a tracing warning containing the flow identifier and estimated cost
- Always unregisters the request from the progress tracker, passing the estimated cost and final delivered count (if present)

## Invariants

The guard maintains consistent state between construction and termination, ensuring accounting is always derivable from the sequence of operations.

- The completion flag is unset at construction and, once set, never reverts — marking completion is idempotent
- Delivered token count is intended to be monotonically non-decreasing from zero — this is the semantic contract, but the implementation does not enforce it against negative increments
- On normal completion with delivered tokens greater than zero, restore credit equals estimated cost minus delivered tokens; net DRR charge equals negative delivered tokens (credit reflects actual work)
- On cancellation, restore credit equals estimated cost minus delivered tokens, saturated at zero; net DRR charge equals delivered tokens capped at the estimated cost — the cap is an arithmetic consequence of the saturation, not a separate design bound
- The request is unregistered from the progress tracker at termination regardless of completion status, carrying the estimated cost and final delivered count

## Constraints

The guard operates within the boundaries of scope-lifetime tracking and scheduler-specific accounting behavior.

- Accounting reports are dispatched unconditionally to all scheduler types; non-DRR schedulers receive the report and ignore it — this is a consumer-side convention, not a dispatch gate
- Termination accounting depends on the guard being dropped — if the guard value is leaked, neither metrics nor accounting fire and the progress tracker retains the request indefinitely
- Over-delivery (delivered tokens exceeding the estimate) on normal completion produces a negative restore cost, applying an additional debit; on cancellation, over-delivery silently clamps restore to zero
- When no usage data arrives by scope exit, the full estimated cost is charged with no restore on normal completion — this Phase-1 limitation trades precision for safety
- The delivered-token update interface accepts any signed integer without validation — the monotonicity invariant stated above is an intended property, not an enforced constraint
- The progress tracker is optional — construction, delivered-token updates, and termination behave correctly without it

## Rationale

RAII-scoped termination eliminates the need for explicit cleanup calls across multiple error paths and prevents accounting drift.

- Scope-exit accounting ensures every request — even those terminated by panic, cancellation, or timeout — is accounted for
- Separating completion marking from termination enables the system to distinguish intentional completion from forced cancellation
- Reporting actual delivered tokens rather than estimates prevents systematic credit inflation in fair-scheduling algorithms
- The progress tracker updates on every delivered-token report, enabling incremental predictive admission adjustments rather than relying solely on termination-time registration
- The zero-delivery fallback charges the full estimate as a conservative bound, ensuring credit cannot inflate when usage data is absent

## Related

- `[[src/scheduler/lifecycle.rs#LifecycleGuard]]` — lifecycle guard implementation
- `[[src/scheduler/lifecycle.rs#AccountingReport]]` — accounting report contract
- `[[src/scheduler/lifecycle.rs#event]]` — lifecycle event constants
- `[[src/scheduler/mod.rs#report_accounting]]` — scheduler accounting entry point
- [[?c_drr_scheduler]] — DRR scheduling and credit system
- [[?c_metrics]] — metrics instrumentation
- [[?c_progress_tracker]] — predictive admission progress tracking
