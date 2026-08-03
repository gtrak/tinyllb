# Audit: c_backend_metrics_parser (Cycle 3 — Final)

**Scope**: Parser contract (metric-name constants, parse result, snapshot type, monitor access).
**Status**: Spec-implementation comparison complete. Issues found.

---

## Findings

### 1. missing_interface — `BackendMonitor::snapshot()` return type

**Severity**: Medium

The implementation declares:
```rust
pub fn snapshot(&self) -> Option<BackendSnapshot>
```

The spec states:

> The latest-value read returns the most recently published snapshot. The read always succeeds and returns the last written value, including after the sender has been dropped. No sentinel distinguishes active from closed state.

The spec correctly describes the runtime behavior (always returns a value, no `None` at runtime). However, it omits that the return type wraps the snapshot in `Option<BackendSnapshot>`. Callers must pattern-match or unwrap an `Option` even though it is always `Some`. The `Option` is a type-level sentinel that exists despite never being `None`.

**Recommendation**: Document the `Option` wrapper in the Interface section. Note that it is always `Some` but present for Rust idiom (or remove the wrapper if acceptable).

---

### 2. spec_error — `found_usage` flag scope under dual metric names

**Severity**: Low

The Interface section describes `found_usage` as:

> A flag value of `true` means the corresponding metric name was present in the input body

"the corresponding metric name" (singular) is ambiguous — there are two metric names (`METRIC_KV_USAGE` and `METRIC_KV_USAGE_V1`) that set the same flag. The Invariants section clarifies via "Dual usage names unify", but the Interface section should be self-contained. A reader of only the Interface section cannot determine whether `found_usage` reflects presence of one specific name or either name.

**Recommendation**: Restate as: "The usage flag is `true` if either the v0 or v1 usage metric name was present in the input body."

---

### 3. undocumented_behavior — `wait_for` implementation docstring contradicts spec and code

**Severity**: Medium

The implementation docstring (line 269–270) states:

> Returns `true` if the predicate was satisfied, `false` if the channel was closed.

The actual signature returns `()`, and both branches execute `return;` (unit). The spec correctly describes this as "Both outcomes produce the same unit return; callers cannot distinguish satisfaction from closure via the return value." The implementation docstring is factually wrong.

**Recommendation**: Fix the implementation docstring to match the spec and code. This is an implementation documentation bug.

---

### 4. No How lint violation — Rationale references implementation primitives

**Severity**: Low

The Rationale section contains:

> the watch channel's borrow primitive always yields the last written value

This references a specific implementation primitive (`borrow` on `tokio::sync::watch::Receiver`). The "No How" constraint bans "Implementation-specific terminology" and "Function/method names as concept identifiers." While the Rationale section inherently discusses "why," referencing a concrete method name and type from the runtime library leaks implementation detail that would become invalid if the underlying mechanism changed.

**Recommendation**: Replace with a domain-level description, e.g., "the watch channel's read operation always yields the last written value."

---

### 5. No How lint warning — Rationale mentions "Clone on the snapshot and handle types"

**Severity**: Informational

> Clone on the snapshot and handle types is necessary for last-value broadcast semantics

"Clone" here refers to a Rust trait, not a domain concept. This is borderline — it explains the rationale for a type property rather than describing control flow. Acceptable in the Rationale context, but note that "cloneable" would be the domain-level term.

---

## Summary

| # | Classification       | Severity | Description                                           |
|---|----------------------|----------|-------------------------------------------------------|
| 1 | missing_interface    | Medium   | `snapshot()` returns `Option<BackendSnapshot>` not documented |
| 2 | spec_error           | Low      | `found_usage` flag scope ambiguous under dual metric names |
| 3 | undocumented_behavior| Medium   | `wait_for` docstring claims `bool` return; actual is `()`    |
| 4 | No How violation     | Low      | Rationale references `borrow` primitive                |
| 5 | No How warning       | Info     | Rationale references `Clone` trait name                 |

**No bugs found** in the parser contract — the implementation faithfully matches the specified behavior for all parse operations, derivation rules, and monitor semantics. The issues above are documentation gaps and spec precision improvements.
