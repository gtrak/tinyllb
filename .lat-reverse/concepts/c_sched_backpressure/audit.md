# Audit: c_sched_backpressure

**Cycle:** 3 of 3 (final)
**Spec:** `.lat-reverse/concepts/c_sched_backpressure/spec.md`
**Implementation:** `src/scheduler/backpressure.rs`
**Status:** 3 issues found

---

## Findings

### 1. bug — Retry-after field admits zero duration

The `BackpressureRejected` struct exposes `retry_after: Duration` as a public field without validation. The type accepts `Duration::ZERO`, directly contradicting invariant: *"A backpressure rejection always carries a positive retry-after duration"*. A struct constructed with zero duration satisfies all derive traits (Debug, Clone) and implements `std::error::Error`, but violates the stated invariant.

- **Spec line:** 60 (invariant)
- **Code:** `pub retry_after: Duration` admits any duration value
- **Severity:** The invariant is unenforceable by the type system

### 2. spec_error — Unconditional invariant conflicts with precondition constraint

Invariant line 60 states: *"A backpressure rejection always carries a positive retry-after duration"* — unconditional ("always"). Constraint line 70 states: *"The retry-after base duration must be strictly positive (greater than zero)"* — a caller precondition. The invariant cannot hold unconditionally; it only holds when the precondition is met. An unconditional invariant and a guarded precondition are semantically incompatible. Either the invariant must be qualified (*"When the base duration is strictly positive, the rejection carries a positive retry-after"*) or the type must enforce positivity.

- **Spec lines:** 60 (invariant), 70 (constraint)
- **Impact:** The invariant violates the invariant validity constraint — it is not true under all implementations, only under a subset where callers honor the precondition

### 3. missing_interface — No enforcement surface for strictly-positive base duration

Constraint line 70 requires the base duration to be strictly positive. The function `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)` accepts `Duration::ZERO` without error, warning, or validation. A zero base duration produces a zero return value, violating invariant line 60 downstream. There is no `Result` return type, no assert, no type-level wrapper that would reject zero. The constraint is documented but unenforced.

- **Spec line:** 70 (constraint)
- **Code:** `retry_after_base: Duration` parameter with no validation

---

## "No How" Lint: PASS

The spec contains no prohibited content:
- No control flow descriptions
- No data structure details or field lists
- No function/method names used as concept identifiers
- No implementation-specific terminology

The affine formula `factor = 1 + depth/capacity` (line 42) is a contractual scaling law, not implementation detail. All other statements describe observable contracts, preconditions, or postconditions.

---

## Verification Summary

| Check | Result |
|---|---|
| Rejection Error interface | Matches implementation |
| Retry-after formula (depth/capacity scaling) | Matches implementation |
| Zero-capacity multiplier (3× base) | Matches implementation |
| Mode label mapping (exhaustive match) | Matches implementation |
| Interface exports (`fail_fast_retry_after`, `mode_label`, `BackpressureRejected`) | Confirmed via `src/scheduler/mod.rs` re-exports |
| BackpressureMode variants (Blocking, FailFast, Hybrid) | Confirmed via `src/config/mod.rs` |
| "No How" constraint | Pass |
| Invariant-enforcement alignment | **Fails** (issues 1-3) |
