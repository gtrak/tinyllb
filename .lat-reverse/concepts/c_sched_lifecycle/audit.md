# Audit: Scheduler Lifecycle Guard

**Cycle:** 3 (final)
**Spec:** `.lat-reverse/concepts/c_sched_lifecycle/spec.md`
**Implementation:** `src/scheduler/lifecycle.rs`
**Auditor constraints:** Per `.lat-reverse/workflows/reconstruction.md`

---

## No-How Lint

**Pass.** The spec contains no control flow descriptions, data structure details, function names as concept identifiers, or implementation-specific terminology. All sections use domain concepts and contractual language. Source code links appear only in the Related section. Wiki links omit `.md` extensions.

---

## Findings

### 1. `undocumented_behavior` — Zero-delivery fallback covers negative delivered tokens on normal completion

**Location:** Interface → Termination Accounting, line 54–56

**Spec states:**
> "When the cumulative delivered-token count is zero on normal completion (no usage data received), falls back to charging the full estimated cost with zero restore credit..."

**Implementation (line 147):**
```rust
if delivered > 0 {
    // normal path
} else {
    // zero-delivery fallback — fires when delivered <= 0, including negative
}
```

The spec describes the fallback condition as "zero" (exact match) with the rationale "no usage data received." The code fires the same fallback path when `delivered <= 0`, which includes negative delivered token counts.

The Constraints section acknowledges that negative increments are possible (`add_delivered_tokens` accepts any `i64` without validation). However, the spec never documents what occurs when delivered tokens are negative at normal completion. The code treats negative identically to zero — charging the full estimated cost and emitting a warning that says "backend response had no usage data," which is inaccurate when negative delivered tokens indicate accounting drift, not missing data.

**Impact:** If `add_delivered_tokens` is called with a net-negative value (e.g., a compensating negative correction), normal completion will trigger the wrong warning message and the same conservative charge as the truly-zero case. The semantic distinction between "no data" and "negative data" is lost.

### 2. `spec_error` — Termination Accounting condition is underspecified

**Location:** Interface → Termination Accounting, lines 54–56

The condition for the zero-delivery fallback is stated as "cumulative delivered-token count is zero" but the implementation condition is `delivered <= 0`. These are not equivalent. The spec should either:

- State the condition precisely as "delivered-token count is zero or negative", or
- State the intended invariant that `delivered` cannot be negative and the zero-delivery path applies only when `delivered == 0`

The current spec creates a mismatch between the documented contract and the implemented guard condition.

### 3. `bug` — Misleading diagnostic for negative delivered tokens on normal completion

**Location:** Implementation line 171–176

```rust
tracing::warn!(
    flow_id = %self.flow_id,
    estimated_cost = self.estimated_cost,
    "backend response had no usage data; charging full estimated cost of {} tokens",
    self.estimated_cost
);
```

When `delivered` is negative (reachable per documented constraints), this warning fires and claims "backend response had no usage data." Negative delivered tokens do not indicate missing usage data — they indicate the delivered token counter went below zero, which is an accounting anomaly, not an absence of data. The warning is diagnostic misinformation that would mislead operators investigating credit anomalies.

---

## Verified Correct

The following spec claims match the implementation exactly:

- Construction emits `request_started` and registers with progress tracker before returning
- `record_token` increments only the per-token metrics counter; does not affect accounting
- `add_delivered_tokens` is additive and updates both internal counter and progress tracker
- `mark_completed` sets the completion flag; idempotent (set-to-true is idempotent for `Cell<bool>`)
- Drop triggers unconditionally on scope exit; emits `request_completed` or `request_cancelled` based on completion flag
- `AccountingReport::Completed` includes both `delivered_tokens` and `restore_cost`
- `AccountingReport::Cancelled` includes only `restore_cost`
- Zero-delivery fallback sets `delivered_tokens: estimated_cost` and `restore_cost: 0`
- Over-delivery on normal completion produces negative restore cost (additional debit)
- Over-delivery on cancellation saturates restore to zero via `saturating_sub`
- Over-delivery warning contains flow_id, delivered_tokens, estimated_cost, and overrun
- Progress tracker is unregistered at termination regardless of completion status
- All source code wiki links in Related section resolve to existing symbols
- All invariants hold: completion flag idempotence, monotonicity-as-intended-but-unenforced, restore arithmetic matches for both positive and cancellation cases

---

## Verdict

**3 issues found:** 1 `spec_error`, 1 `undocumented_behavior`, 1 `bug`.

All three stem from the same root cause: the spec defines the zero-delivery fallback condition as "delivered-token count is zero" but the implementation uses `delivered <= 0`, leaving negative-delivered-tokens on normal completion both misdocumented and misdiagnosed. The fix is a spec clarification on the condition boundary and a code change to either distinguish the negative case or document it explicitly.

The remainder of the spec is accurate, well-specified, and implementation-aligned.
