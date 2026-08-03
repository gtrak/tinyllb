# Starvation Detection — Cycle 3 Audit

## Summary

Spec version: twice-corrected (cycle 3). Source: `src/scheduler/starvation.rs`, `src/scheduler/drr.rs`, `src/scheduler/wfq.rs`, `src/scheduler/completion_bias.rs`, `src/flow/mod.rs`.

**Verdict**: Spec is substantially accurate. No bugs found. Four remaining issues — all spec-side.

---

## Issues

### 1. spec_error — "Three observable code paths" conflates function topology with call sites

The Interface section introduction states: *"The concept manifests in three observable code paths: a paired check-and-record operation used by fair-share schedulers, a standalone combined operation in the completion bias gate, and a standalone metrics function called after an independent check."*

This enumerates three items, but items 1 and 3 describe the same function (`record_force_admit`) viewed from two angles: item 1 describes it as paired with `is_starved`, item 3 describes it as a standalone function. These are not three distinct paths. The actual topology is two patterns consumed in three call sites:

- Pattern A (paired): `is_starved` → `record_force_admit` — used by both DRR and WFQ schedulers
- Pattern B (combined): `maybe_force_admit` — used internally by the completion bias gate

The enumeration needs to distinguish between function topology (two patterns) and consumption sites (three call sites: DRR, WFQ, completion bias).

**File**: `spec.md`, Interface section introduction (line 23)
**Severity**: Minor — confusing but not factually wrong about behavior.

---

### 2. undocumented_behavior — Completion bias gate re-checks starvation periodically

The completion bias gate does not check for starvation once. It enters a wait loop that re-checks starvation at intervals of `starvation_timeout / 4` (computed at construction time as `starvation_check_interval`). A flow can pass through the starvation check multiple times during a single `check()` call before either being force-admitted or the gate's slot condition is met.

The spec describes the gate's starvation check as a single binary decision: *"Independently determines starvation and records metrics in a single operation"* and *"Returns a binary decision — starved or not — rather than exposing the wait duration."* This is accurate for `maybe_force_admit` as a function, but the observable behavior of the gate is that the check recurs during the wait loop, which is not captured.

**File**: `spec.md`, Interface section "Completion bias gate check" (lines 41–47) and Invariants section
**Severity**: Medium — the re-checking behavior is observable (a flow can be admitted by starvation after having been non-starved in a prior check within the same wait loop).

---

### 3. spec_error — "No How" violation: implementation terminology in Interface

The Interface section uses *"code paths"* (line 23) — implementation terminology — to describe the interface surface. Per the "No How" constraint, the spec must reject implementation-specific terminology in concept descriptions. The Interface section should describe contractual boundaries (what consumers rely on), not the structural arrangement of code paths.

**File**: `spec.md`, Interface section introduction (line 23)
**Severity**: Minor — structural rather than semantic.

---

### 4. spec_error — Constraint overgeneralizes timeout provenance

The Constraints section states: *"The timeout threshold is caller-supplied; no code path validates or constrains the range of acceptable values."*

This is accurate for `is_starved` (timeout passed by the scheduler's `try_select`) and `record_force_admit` (wait duration passed by caller). However, the completion bias gate's `starvation_timeout` is not caller-supplied at check time — it is configured once at construction (`CompletionBiasGate::new`) and used internally. The constraint should distinguish between caller-supplied thresholds (schedulers) and internally configured thresholds (gate).

**File**: `spec.md`, Constraints section (line 64)
**Severity**: Minor — the constraint correctly describes scheduler behavior but overgeneralizes to all code paths.

---

## No How Lint

Checked against the "No How" constraint (reject control flow, data structures, function names as identifiers, implementation terminology):

| Check | Result |
|---|---|
| Control flow descriptions | None found |
| Data structure details | None found |
| Function/method names as concept identifiers | None found (functions referenced by contract description, not by name) |
| Implementation-specific terminology | **"code paths"** in Interface section (issue 3 above) |

**Pass** with one minor violation (issue 3).

---

## Verification of Invariants Against Implementation

| Invariant | Status | Evidence |
|---|---|---|
| Flow without enqueue instant never starved | Holds | `is_starved` returns `None` on `None` enqueued_at; `maybe_force_admit` same pattern |
| Strict `>` for threshold | Holds | Both paths use `wait > timeout`, not `>=` |
| Monotonic clock (`Instant`) | Holds | All three paths use `Instant::now().duration_since(queued_at)` |
| Stateless decision | Holds | No mutable state beyond `enqueued_at`; decision is purely `now - enqueued_at > threshold` |
| All paths emit same two metrics | Holds | `record_force_admit` emits both; `maybe_force_admit` emits both directly |

---

## Bug Search

No bugs found. The implementation correctly:
- Returns `None` when `enqueued_at` is `None`
- Uses strict greater-than for threshold comparison
- Uses monotonic `Instant` for time measurement
- Records both metrics in all three consumption patterns
- Clears `enqueued_at` after force-admission in all scheduler paths

---

## Conclusion

The spec is substantially correct after two correction cycles. The remaining issues are all spec-side precision problems (enumeration confusion, undocumented re-checking behavior, one "No How" terminology slip, one overgeneralized constraint). No implementation bugs. No missing interfaces. The spec is audit-ready pending minor cleanup of the four items above.
