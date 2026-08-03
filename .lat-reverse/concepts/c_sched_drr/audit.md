# DRR Scheduler — Cycle 3 Audit

## Role

Auditor. Identifies contradictions, mismatches, violations, interface gaps, and implementation leakage. Does not rewrite, fix, or suggest implementation changes.

## Status

Build: pass · Tests: pass · Clippy: pass

## No-How Lint

Four statements use implementation-specific terminology instead of domain concepts:

1. **Invariants > Selection ordering** — "Ties in priority are broken by round-robin cursor position" — "cursor position" is an implementation-specific data structure reference. Replace with "round-robin turn order" or "the flow most recently served."

2. **Invariants > Selection ordering** — "When cursor positions are identical, the flow that enqueued earliest wins" — same issue ("cursor positions"), and additionally documents an unreachable condition (each flow appears at most once in the cursor).

3. **Rationale** — "A three-level tie-breaking chain (priority, cursor position, enqueue time)" — "cursor position" repeats the implementation-specific term from (1).

4. **Interface > Observation** — "metric gauges that expose credit may briefly include the eligibility accumulator during the scheduling round's accumulation phase" — "scheduling round's accumulation phase" describes internal timing. Rephrase as "during credit accumulation before a selection decision" or similar domain language.

## Findings

### spec_error (1)

1. **Unreachable tiebreaker documented.** The spec states "When cursor positions are identical, the flow that enqueued earliest wins." In the implementation each flow appears at most once in the round-robin cursor, so cursor positions are always distinct — this third tiebreaker is unreachable. The spec documents behavior that cannot occur.

### undocumented_behavior (1)

1. **`credit()` creates flows on the read path.** `DrrScheduler::credit()` calls `self.registry.get_or_create()`, which creates a new flow with default weight/priority/credit if one does not exist. The Interface section describes `credit()` as "per-flow credit reports only the permanent credit balance" but does not state that invoking the read path can mutate the registry. (Symmetrically, `report_accounting` does the same — that case IS documented in the Accounting section, but the `credit()` case is not.)

### missing_interface (1)

1. **Constructor contract incomplete.** The spec describes configuration parameters (starvation timeout default, backpressure modes, etc.) in Constraints but does not document the construction interface as a contract surface. External callers depend on `DrrScheduler::new()` and `Scheduler::new()` (which dispatches to `DrrScheduler::new_with_policies()`). The parameters, their domain meaning, valid ranges, and defaults are scattered across Constraints rather than declared as a single construction contract.

### bug (0)

No mismatches between spec claims and observed implementation behavior were found. Specifically verified:

- Fail-fast rejection before admission state is established produces no depth or permit side effects (depth check precedes guard creation).
- Hybrid-mode worst-case wait is approximately 2× max_wait (gate timeout + channel timeout, sequential).
- Starvation force-selection debits permanent credit identically to normal selection.
- Permanent credit is never zeroed by the scheduler (explicit comments guard against accidental reset).
- Eligibility accumulator (deficit) is cleared on selection, cancellation, and empty-queue cleanup.
- `report_accounting` correctly ignores `delivered_tokens` in the `Completed` variant.
- The `active` flag in `DrrAdmitGuard` ensures exactly one decrement (consume or drop, never both).
- `queue_depth()` delegates to `registry.sum_depths()` as specified.

## Blockers / Uncertainties

- The Related section contains only unresolved placeholder references (`[[?c_sched_fifo]]`, `[[?c_sched_backpressure]]`, etc.) and one source code link. This is expected for an in-progress concept; no finding.
