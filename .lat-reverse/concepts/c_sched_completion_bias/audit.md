# Completion Bias Gate — Audit (Cycle 3)

## Summary

Audited `.lat-reverse/concepts/c_sched_completion_bias/spec.md` (twice-corrected) against `src/scheduler/completion_bias.rs`. Four issues remain.

---

## Issues

### 1. `notify_waiters` Public Method Not Documented in Interface

**Classification:** `missing_interface`

The gate exposes `pub fn notify_waiters(&self)`, a public method called by the scheduler when an active flow count changes. The spec's Notification section describes the *effect* ("the gate wakes all blocked callers when the active flow count changes") but does not declare `notify_waiters` as a callable interface surface. Per the Interface-first principle, every public method is an interface surface that must be documented.

**Location:** Interface → Notification section vs. `src/scheduler/completion_bias.rs:176`

---

### 2. Constructor Parameter `max_active_flows` Not Listed in Interface

**Classification:** `missing_interface`

`CompletionBiasGate::new()` accepts `max_active_flows: u32` as a configuration parameter, yet the Interface → Configuration section does not mention this parameter. The spec describes the *effect* ("when the target is zero, the maximum active flow count becomes the effective target") without naming the parameter or its role in construction. Callers cannot construct the gate from the spec alone.

**Location:** Interface → Configuration section vs. `src/scheduler/completion_bias.rs:60`

---

### 3. "Poisoned Lock" — Implementation Leakage in Constraints

**Classification:** `spec_error`

The Constraints section states: "A poisoned lock on the enqueued timestamp value causes a runtime failure that precludes admission." The phrase "poisoned lock" names a Rust `Mutex` implementation detail — a domain spec should not reference `std::sync::Mutex` poison semantics. The domain concern is *concurrent mutation of the enqueued timestamp*, not mutex poisoning specifically. Additionally, the current implementation does `.unwrap()` on the poisoned mutex (line 160), which panics the thread rather than returning an error. This is a genuine bug masked as a constraint.

**Location:** Constraints section, bullet 4 vs. `src/scheduler/completion_bias.rs:160`

---

### 4. Starvation Re-check Interval Exposed in Constraints

**Classification:** `spec_error`

The Constraints section states: "The starvation re-check interval is currently derived as one quarter of the starvation timeout; this ratio is an implementation detail, not a domain invariant." The sentence acknowledges the violation but retains the implementation detail anyway. Per the "No How" constraint, implementation details belong in source code, not in domain specifications. This bullet should be removed or rewritten purely in domain terms (e.g., "The gate periodically re-evaluates admission conditions while waiting; the frequency is an implementation detail.").

**Location:** Constraints section, last bullet vs. `src/scheduler/completion_bias.rs:79`

---

## No-How Lint

| Section | Violation | Status |
|---------|-----------|--------|
| Purpose | Clean — domain terms only | ✅ |
| Interface → Admission | Clean — contract terms only | ✅ |
| Interface → Configuration | Clean except missing `max_active_flows` param (#2 above) | ⚠️ |
| Interface → Preconditions | Clean — domain terms | ✅ |
| Interface → Notification | Describes effect but omits `notify_waiters` surface (#1 above) | ⚠️ |
| Invariants | Clean — all statements are rewrite-invariant | ✅ |
| Constraints | "Poisoned lock" names a Rust mutex detail (#3 above) | ❌ |
| Constraints | Starvation re-check interval retained despite disclaimer (#4 above) | ❌ |
| Rationale | Clean — domain reasoning only | ✅ |
| Related | Clean — source links only in Related section | ✅ |

---

## Verdict

The spec is substantially sound. The purpose, invariants, and rationale are domain-pure and rewrite-invariant. The four issues above are:

- Two missing interface surfaces (`notify_waiters`, `max_active_flows` constructor param)
- Two spec errors (implementation leakage in Constraints: "poisoned lock" semantics, re-check interval ratio)

No bugs in the logical contract were found beyond the masked panic at the poisoned-mutex `.unwrap()`.
