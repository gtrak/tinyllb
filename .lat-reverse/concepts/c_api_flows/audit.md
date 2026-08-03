# Audit: API Flow Registration

**Audited against:** `src/api/flows.rs`
**Spec:** `.lat-reverse/concepts/c_api_flows/spec.md`
**Date:** 2026-08-03

---

## Summary

The implementation broadly conforms to the spec. Two correctness bugs were found, and four edge-case behaviors are undocumented. No "No How" violations were found in the spec.

---

## Contradictions

### BUG-1: NaN weight bypasses positivity check (bug)

**Spec invariant:** "Weight positivity — Every successfully registered flow has `weight > 0`."

**Implementation:** `src/api/flows.rs:40` checks `if req.weight <= 0.0`. Since IEEE 754 NaN satisfies neither `<= 0.0` nor `> 0.0`, NaN passes this check and is stored in the registry.

**Evidence:** In Rust, `f64::NAN <= 0.0` evaluates to `false`. The code path falls through to `register()` which persists NaN as the weight.

**Impact:** The weight positivity invariant is violated for NaN inputs.

---

### BUG-2: TOCTOU race in `FlowRegistry::register()` (bug)

**Spec invariant:** "Status matches outcome — The response status field reflects the actual resolution: `\"created\"` only for previously unknown identities, `\"updated\"` only for existing ones."

**Implementation:** `src/flow/mod.rs:188-211` performs a non-atomic check-then-act pattern:

1. `get_mut(&id)` checks for existence (returns `None` if absent)
2. If absent, `insert(&id, flow)` creates the entry
3. Returns `true` (created) based on the observation at step 1

Between steps 1 and 3, a concurrent caller can insert the same ID. Both threads observe "not present," both insert, and both return `true` (`\"created\"`). The second writer overwrites the first, yet still reports `\"created\"` for an identity that already existed when the insert occurred.

**Impact:** Under concurrent registration of the same flow ID, the `\"created\"`/`\"updated\"` discriminator and HTTP 201/200 status can be incorrect, violating the invariant.

---

## Undocumented Behaviors

### UNDOC-1: Negative priority values produce serde deserialization errors (undocumented_behavior)

**Spec claim:** "HTTP 400 Bad Request — Returned when weight or priority violates bounds; response body is a plain-text error message."

**Actual behavior:** The `priority` field is typed `u32` (`src/api/flows.rs:17`). A negative JSON value cannot be deserialized into `u32`. Serde returns a deserialization error before the handler body executes. Axum's `Json` extractor converts this to an HTTP 400 with a JSON-formatted error body, not the plain-text `"priority must be between 0 and 100"` message documented by the spec.

---

### UNDOC-2: Non-JSON Content-Type returns 415 (undocumented_behavior)

**Spec claim:** "Accepts only JSON request bodies."

**Actual behavior:** Axum's `Json` extractor rejects non-JSON `Content-Type` headers with HTTP 415 Unsupported Media Type. The spec only documents 200, 201, and 400 response codes. Status 415 is not covered.

---

### UNDOC-3: Float priority values produce serde deserialization errors (undocumented_behavior)

**Spec claim:** "Priority must be an integer within [0, 100]."'

**Actual behavior:** The `priority` field is `u32`. A JSON float value (e.g., `50.5`) fails serde deserialization into `u32`, producing a 400 with a JSON error body (serde error format) rather than a plain-text validation message. This case is not covered by the spec.

---

### UNDOC-4: Positive infinity weight passes validation (undocumented_behavior)

**Spec claim:** "Weight must be a positive floating-point number; zero and negative values are rejected."

**Actual behavior:** `f64::INFINITY` satisfies `> 0.0` and passes validation. The spec does not address whether infinite values are accepted or rejected. The registry would store `INFINITY` as the weight, which may cause undefined behavior downstream in scheduling calculations.

---

## "No How" Lint

The spec passes the "No How" constraint:

- No function or method names are used as concept identifiers.
- No data structures are described (no field lists, no internal types).
- No control flow is specified (the spec describes outcomes, not steps).
- No implementation-specific terminology appears in Purpose, Interface, Invariants, or Constraints.
- `src/api/flows.rs` is referenced only in the Related section, which is permitted.

**Result: PASS**

---

## Classification Summary

| # | Classification | Count |
|---|---|---|
| bug | NaN weight passes, TOCTOU race in register | 2 |
| spec_error | — | 0 |
| undocumented_behavior | Negative priority, 415 for non-JSON, float priority, infinity weight | 4 |
| missing_interface | — | 0 |
