# WFQ Scheduler — Audit (Cycle 3)

> Auditor role: identify contradictions, mismatches, violations, interface gaps,
> and implementation leakage. No rewriting or fixing.

## "No How" Lint: PASS

The spec avoids control flow descriptions, data structure details, function names
as concept identifiers, and implementation-specific terminology. All statements
are framed as domain contracts and behavioral invariants. One minor note:
"credited when the ticket is dropped" references the Rust `Drop` trait, which is
standard for RAII concepts in Rust and does not leak implementation structure.

---

## Issues

### 1. spec_error — WFQ ratio comparison direction underspecified

**Section:** Invariants > Selection ordering

The spec states: *"WFQ fairness ratio (cumulative service divided by weight)
breaks priority ties."* It does not state whether a lower or higher ratio is
preferred. A reader could implement this as either direction and the spec would
not catch the error. The implementation selects the flow with the minimum
`service_done / weight` ratio (least-served-per-weight unit first).

**Verdict:** The spec must state the comparison direction. Lower ratio wins.

---

### 2. bug — `Flow.enqueued_at` cleared on any selection, breaking sibling starvation detection

**Section:** Invariants > Selection ordering ("A flow waiting longer than the
starvation threshold is selected before any non-starved flow.")

The starvation check reads `flow.enqueued_at` — a single `Option<Instant>` per
Flow. When ANY request for that flow is selected (starved or normal),
`try_select` sets `flow.enqueued_at = None`. If the same flow has multiple
pending requests concurrently (multiple `admit` calls), clearing `enqueued_at`
for one selection blindes starvation detection for all remaining sibling
requests. The invariant *"A flow waiting longer than the starvation threshold is
selected before any non-starved flow"* can be violated for those siblings.

**Root cause:** `Flow.enqueued_at` is per-flow (shared), but selection clears it
for any individual request. The `Pending` struct has its own `enqueued_at` used
for FIFO tiebreaking, but starvation detection does not use it.

---

### 3. missing_interface — Constructor parameters omitted from Constraints

**Section:** Constraints > Configuration immutability

The spec lists three fixed parameters: *"Backpressure mode, maximum active flows,
and queue depth threshold."* The constructor `WfqScheduler::new` additionally
accepts `max_wait: Duration` (hybrid timeout bound) and
`retry_after_base: Duration` (retry-after computation base). Both are fixed at
construction and affect observable behavior, but are absent from the spec.

A rewriter would not know to parameterize these values.

---

### 4. undocumented_behavior — `admit_fail_fast` delegates to blocking after depth check passes

**Section:** Interface > Admission

The spec describes fail-fast rejection: *"fail-fast rejects immediately when
queue depth exceeds a configured threshold."* It does not describe the behavior
when the depth check passes. The implementation delegates to `admit_blocking`,
which waits indefinitely for a permit. A rewriter might implement fail-fast as
"check depth, reject or return immediately" and miss the blocking delegation.

---

### 5. undocumented_behavior — Depth counter incremented before completion bias gate check

**Section:** Interface > Admission ("the gate blocks before the request enters
the waiting queue")

The spec states the gate blocks before the request enters the waiting queue. The
implementation increments the depth counter (via `WfqAdmitGuard::new`) BEFORE
the gate check. A flow blocked at the completion bias gate IS counted in
`queue_depth()`. The spec says the gate blocks before entering the waiting queue
(which is technically true for the pending entry) but does not clarify that
depth is already incremented during gate blocking.

---

### 6. undocumented_behavior — Default starvation timeout value not specified

**Section:** Constraints > Configuration immutability

The spec says: *"Starvation timeout defaults to a fixed value and is not
configurable via the primary public constructor."* The default value
(`Duration::from_secs(300)`) is not stated. A rewriter would have no basis for
choosing the default.

---

## Summary

| # | Classification | Severity | Description |
|---|---|---|---|
| 1 | spec_error | Medium | WFQ ratio direction (min vs. max) not specified |
| 2 | bug | High | Sibling starvation detection broken when `Flow.enqueued_at` cleared on any selection |
| 3 | missing_interface | Medium | `max_wait` and `retry_after_base` constructor parameters omitted from Constraints |
| 4 | undocumented_behavior | Low | fail-fast post-depth-check behavior (delegates to blocking) not described |
| 5 | undocumented_behavior | Low | Depth incremented before gate check — gate-blocked flows counted in depth |
| 6 | undocumented_behavior | Low | Default starvation timeout value (300s) not stated |
