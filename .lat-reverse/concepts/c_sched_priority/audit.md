# c_sched_priority — Audit Report (Cycle 3 — Final)

**Scope:** Compare `.lat-reverse/concepts/c_sched_priority/spec.md` against `src/scheduler/priority.rs`.
**Method:** Spec → Implementation diff. Report contradictions only.
**Prior cycle:** Cycle 2 identified 5 findings (2 BUG, 2 SPEC_ERROR, 1 UNDOCUMENTED_BEHAVIOR). Spec was corrected twice. Implementation was NOT changed.

---

## Findings

### 1. BUG — Epsilon equivalence does not override "lower wins" in comparison chain

**Severity:** Critical — the three-level hierarchy is broken for scores within the tolerance band.

**Spec claim (Invariants, Base-score equivalence):**
> "Two base scores are treated as equivalent when their absolute difference does not exceed a small positive tolerance value. Equivalent scores do not trigger the 'lower wins' rule."

**Spec claim (Invariants preamble):**
> "The equivalence check for base scores takes precedence over the 'lower score wins' rule — scores within a tolerance band trigger the FIFO tiebreak, not the lower-score preference."

**Implementation behavior (`src/scheduler/priority.rs`, lines 50–57):**
```rust
if cand.base_score < current_best.base_score {
    best = Some(cand);
} else if (cand.base_score - current_best.base_score).abs() < f64::EPSILON {
    if cand.enqueued_at < current_best.enqueued_at {
        best = Some(cand);
    }
}
```

The `<` comparison on `base_score` executes BEFORE the epsilon equivalence check. When `cand.base_score` is strictly less than `current_best.base_score` but the absolute difference is below the tolerance threshold, the first branch fires and selects the candidate by "lower wins" — the equivalence branch is never reached. The FIFO tiebreak is bypassed for all score pairs that are tolerance-close but not bit-identical.

**Expected:** Scores within tolerance should be treated as equivalent, triggering FIFO tiebreak.
**Actual:** Scores within tolerance but strictly lower always win via the `<` branch.

**Status from cycle 2:** Still open. The spec was corrected to state precedence correctly, but the implementation was never modified.

---

### 2. BUG — Non-finite base scores are not filtered

**Severity:** Critical — the spec claims silent exclusion; the implementation provides none.

**Spec claim (Constraints, second bullet):**
> "Base scores are real-valued; callers must supply finite scores (not not-a-number and not infinity). Non-finite scores are silently excluded from consideration rather than rejected."

**Spec claim (Interface, Selection Contract, sixth bullet):**
> "Candidates with non-finite base scores (not-a-number or infinity) are excluded from winning; they are silently bypassed as if absent."

**Implementation behavior (`src/scheduler/priority.rs`):** No filtering or validation of `base_score` exists. Three degenerate cases:

| Score value | Behavior |
|---|---|
| **NaN** | If first candidate, becomes and remains `best` (all comparisons with NaN return `false`). |
| **-Infinity** | Always "lower" than any finite score; always wins the base_score tiebreak. |
| **+Infinity** | Never "lower"; wins only if first candidate and no higher-priority contender appears. |

**Expected:** Non-finite scores are excluded from consideration entirely.
**Actual:** Non-finite scores participate in comparisons with degenerate and caller-undetectable outcomes.

**Status from cycle 2:** Was classified as UNDOCUMENTED_BEHAVIOR (finding #4). The spec has now been updated to explicitly claim silent exclusion, turning this into a spec-vs-implementation contradiction. Upgraded to BUG.

---

### 3. UNDOCUMENTED_BEHAVIOR — Tolerance value not specified

**Severity:** Low — the tolerance threshold is an implementation choice not reflected in the spec.

The spec states "a small positive tolerance value" (Invariants, Base-score equivalence) and "the tolerance threshold" (Invariants, Base-score direction). The implementation uses `f64::EPSILON` (~2.22e-16). The spec does not specify which tolerance value is used, leaving a gap: a reader reconstructing the contract cannot determine whether machine epsilon, a configurable threshold, or some other value applies. This is a minor gap — the spec is deliberately abstract — but it is worth noting for completeness.

**Status:** New finding. Not raised in cycle 2 because the cycle-2 spec referenced "machine epsilon" directly (which was flagged as a No How violation). The corrected spec removed the reference, inadvertently creating this gap.

---

## Corrected Issues from Prior Cycles

| Cycle 2 Finding | Disposition |
|---|---|
| #2 SPEC_ERROR — I2 and I4 internally inconsistent | **Fixed.** The corrected spec now states explicit precedence: "The equivalence check for base scores takes precedence over the 'lower score wins' rule." |
| #3 SPEC_ERROR — "machine epsilon" and "floating-point arithmetic" (No How violations) | **Fixed.** The corrected spec uses "a small positive tolerance value" and "real-valued" respectively. |
| #5 MISSING_INTERFACE — Clock domain/monotonicity of `enqueued_at` unspecified | **Fixed.** Constraints now state: "Enqueue timestamps originate from a monotonic clock within a single process lifetime; they are not comparable across process restarts or distributed processes." |

---

## Summary Table

| # | Classification | Severity | Status |
|---|---|---|---|
| 1 | BUG | Critical | Epsilon check runs after `<`, bypassing FIFO for near-equal scores |
| 2 | BUG | Critical | NaN/Inf not filtered despite spec claiming silent exclusion |
| 3 | UNDOCUMENTED_BEHAVIOR | Low | Tolerance value unspecified in abstracted spec |

---

## No How Lint Results

| Section | Verdict | Detail |
|---|---|---|
| Purpose | Pass | Abstract contract description. No implementation details. |
| Interface | Pass | Domain concepts only: "unsigned", "real-valued", "monotonically increasing". |
| Invariants | Pass | "a small positive tolerance value" replaces prior "machine epsilon". No control flow, data structures, or function names. |
| Constraints | Pass | "real-valued", "unsigned scalar", "monotonic clock" are domain-level terms. Cycle-2 violations removed. |
| Rationale | Pass | Explanatory text, no implementation leakage. |
| Related | Pass | Source code wiki links permitted per workflow rules. |

**No How verdict:** Clean. All cycle-2 violations have been corrected.
