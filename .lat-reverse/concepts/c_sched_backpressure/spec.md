# c_sched_backpressure

## Purpose

This concept defines the contract for rejecting requests when the scheduler queue is at capacity and computing retry-after durations that scale with queue pressure. It enables callers to distinguish backpressure rejections from other failure modes and to inform clients how long to wait before retrying.

- Provides a single error signal that represents queue-full rejection
- Suggests a retry-after duration proportional to how saturated the queue is
- Maps each [[?c_backpressure_mode]] variant to a stable label for observability
- Establishes a lower bound on retry-after so clients never receive zero-wait suggestions

## Non-goals

This concept does not decide when backpressure should be applied, nor does it manage queue state or scheduling policy.

- Does not determine which backpressure mode is active — that is a configuration concern of [[?c_backpressure_mode]]
- Does not enforce backpressure on any scheduler — only computes values consumed by [[?c_scheduler]]
- Does not define blocking or hybrid behavior — only the fail-fast retry-after computation
- Does not rate-limit, throttle, or shape traffic

## Interface

The public contract consists of an error type for rejection signaling, a retry-after computation based on queue pressure, and a mode-to-label mapping for metrics instrumentation.

### Rejection Error

Callers receive a rejection error when the queue is full. The error carries a retry-after duration that represents the minimum suggested wait before resubmitting.

- The error is inspectable for its retry-after duration via a public field
- The error displays a human-readable message indicating queue-full rejection
- The error is cloneable for propagation across boundaries
- The error implements the standard error trait for use in error-chain APIs
- The error is debuggable for diagnostic output

### Retry-After Computation

Given current queue depth, maximum queue depth, and a base retry-after duration, the computation returns a scaled retry-after duration that increases as the queue fills.

- Accepts queue depth, capacity, and a base duration as inputs
- Returns a duration that is always at least the base duration
- Returns exactly three times the base duration when capacity is zero
- The retry-after duration scales affinely with the ratio of depth to capacity (factor = 1 + depth/capacity)
- When depth exceeds capacity, the factor exceeds two and grows without upper bound; this is not an error condition

### Mode Label Mapping

Each backpressure mode variant maps to a canonical string label used for metrics and logging.

- Every backpressure mode variant has exactly one label
- Labels are stable across implementation changes
- Adding a new mode variant requires an explicit label mapping

## Invariants

These properties hold regardless of implementation details and must survive complete rewrite.

- The computed retry-after duration is never less than the provided base duration
- The computation never produces an undefined value for any valid input, including zero capacity
- Every backpressure mode variant has a corresponding label; the mapping is exhaustive
- A backpressure rejection always carries a positive retry-after duration
- The retry-after duration grows monotonically with queue depth, holding capacity constant

## Constraints

These are the operational boundaries within which the concept functions.

- Retry-after scaling applies only to fail-fast mode; blocking and hybrid modes do not use the computed value
- Queue depth and capacity are non-negative integers; negative inputs are ill-formed
- Zero-capacity configurations produce a higher multiplier (3× base) than a fully loaded queue (2× base)
- The retry-after base duration must be strictly positive (greater than zero) to guarantee non-zero retry-after durations
- The concept assumes the queue depth does not typically exceed capacity; over-capacity depth produces a ratio greater than one and scales without upper bound but is not an error condition

## Rationale

These decisions exist to ensure predictable client behavior under load and to maintain observability across backpressure modes.

- Affine scaling of retry-after with queue pressure prevents thundering-herd retries when the queue is saturated while preserving a guaranteed minimum wait
- Guaranteeing a minimum retry-after equal to the base duration prevents clients from retrying instantaneously under any conditions
- Requiring a strictly positive base duration ensures the "never zero-wait" guarantee holds regardless of queue state
- Zero-capacity configurations produce a higher multiplier than full-queue pressure to avoid undefined arithmetic without requiring callers to validate capacity beforehand
- Exhaustive mode-to-label mapping is compile-time enforced so metrics remain consistent when modes evolve
- A single rejection error type ensures all queue-full failures are distinguishable from processing errors

## Related

- [[?c_backpressure_mode]] — `depends_on` — defines the backpressure mode variants
- [[?c_scheduler]] — `constrains` — consumes retry-after values and rejection errors
- [[src/scheduler/backpressure.rs]] — source implementation
- [[src/config/mod.rs]] — `BackpressureMode` enum definition
