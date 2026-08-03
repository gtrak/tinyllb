# c_flow_identify — Audit

**Auditor:** Final-cycle Auditor (LAT reverse-engineering pipeline)
**Spec:** `.lat-reverse/concepts/c_flow_identify/spec.md` (twice-corrected)
**Source:** `src/flow/identify.rs`, `src/flow/mod.rs`
**Status:** PASS with findings

---

## "No How" Lint

**Result: PASS**

The spec contains no "No How" violations:
- No control flow descriptions (precedence order is a contract, not control flow).
- No data structure details exposed (internal representation of `FlowId` is not described).
- Function/method names appear only in the Interface section as API surface descriptors, not as concept identifiers.
- No implementation-specific terminology (e.g., "HashMap", "dashmap", "serde_json") leaks into the spec.

The spec correctly uses domain concepts: "resolution", "precedence", "ephemeral detection", "metric label derivation".

---

## Spec-to-Source Comparison

### Purpose — PASS

| Claim | Source Evidence | Verdict |
|---|---|---|
| Every request yields exactly one `FlowId` | `resolve()` always returns `FlowId` — no `Result`, no `Option`. Three branches: header, body, or ephemeral generation. | ✅ Verified |
| Three identification sources | Header (`extract_flow_id_from_header`), body (`extract_flow_id_from_body`), auto-generated (`generate_ephemeral_id`). | ✅ Verified |
| Fixed precedence order | Header checked first (early return), body second (early return), ephemeral last (unconditional). | ✅ Verified |
| Opaque identifier type distinguishes user vs. auto-generated | `FlowId(String)` is a newtype. `is_ephemeral()` checks prefix. | ✅ Verified |
| Ephemeral IDs collapse to single metric label | `metric_label()` returns `"ephemeral"` for `is_ephemeral() == true`. | ✅ Verified |

### Non-goals — PASS

No source evidence contradicts any non-goal. Resolution is purely extractive — no validation, no auth, no persistence, no non-JSON parsing.

### Interface — Resolution input — PASS

| Claim | Source Evidence | Verdict |
|---|---|---|
| Headers and body provided separately | `resolve(headers: &HeaderMap, body: &Bytes)` — two distinct parameters. | ✅ Verified |
| Both inputs optional; absence triggers fallback | Empty `HeaderMap` + empty `Bytes` → `generate_ephemeral_id()`. Test `empty_body_falls_through_to_ephemeral` confirms. | ✅ Verified |
| No error channel; always returns `FlowId` | Return type is `FlowId`, not `Result<FlowId, E>`. | ✅ Verified |

### Interface — Precedence contract — PASS

| Claim | Source Evidence | Verdict |
|---|---|---|
| Header (`X-LLM-Flow-ID`) is highest precedence | First `if let` in `resolve()`. Test `header_takes_precedence_over_metadata` confirms. | ✅ Verified |
| Body (`metadata.flow_id`) is second | Second `if let` in `resolve()`. Test `metadata_flow_id_is_extracted` confirms. | ✅ Verified |
| Auto-generated is third (fallback) | Unconditional final return. | ✅ Verified |

### Interface — Source acceptance criteria — PASS

| Claim | Source Evidence | Verdict |
|---|---|---|
| Header: present, valid UTF-8, non-empty | `to_str().ok()` rejects non-UTF-8. `.filter(\|s\| !s.is_empty())` rejects empty. | ✅ Verified |
| Body: non-empty, valid JSON, `metadata.flow_id` string non-empty | `body.is_empty()` early return. `serde_json::from_slice::<Value>` parses JSON. `.get("metadata")?.get("flow_id")?.as_str()?` navigates structure. `.is_empty()` check. | ✅ Verified |
| Non-object JSON top-level silently skipped | `.get("metadata")` on array/primitive/null `Value` returns `None`, falling through to ephemeral. | ✅ Verified |
| Auto-generated: distinct per invocation | `Uuid::new_v4()` produces random UUID each call. Test `ephemeral_ids_are_unique` confirms. | ✅ Verified |

### Interface — FlowId public surfaces — **FAIL** (2 findings)

| Claim | Source Evidence | Verdict |
|---|---|---|
| Constructor (`new`) accepts all strings | `pub fn new(id: impl Into<String>)` accepts any string-like input. | ✅ Verified |
| Caller-constructed `ephemeral-*` classified as ephemeral | `is_ephemeral()` checks `starts_with("ephemeral-")` — applies to any `FlowId`. | ✅ Verified |
| `Display` yields exact string | `impl Display` writes `self.0` directly. | ✅ Verified |
| Ephemeral test via prefix | `starts_with("ephemeral-")`. | ✅ Verified |
| Metric label: ephemeral → `"ephemeral"`, named → exact value | `metric_label()` returns `"ephemeral"` if ephemeral, else `&self.0`. | ✅ Verified |
| **`Clone` trait** | `#[derive(Clone)]` — **not documented in spec**. | ⚠️ Missing |
| **`PartialEq`, `Eq`, `Hash` traits** | `#[derive(PartialEq, Eq, Hash)]` — **not documented in spec**. | ⚠️ Missing |
| **`Debug` trait** | `impl Debug` formats as `FlowId("...")` — **not documented in spec**. | ⚠️ Missing |

### Invariants — PASS

| Invariant | Source Evidence | Verdict |
|---|---|---|
| Total resolution | `resolve()` has no code path that returns `None` or `Err`. | ✅ Verified |
| Header exclusivity | Header branch uses `return` — body is never consulted if header succeeds. | ✅ Verified |
| Fallback guarantee | `generate_ephemeral_id()` is the unconditional final expression in `resolve()`. | ✅ Verified |
| Statistical distinctness of ephemeral IDs | `Uuid::new_v4()` provides 122 bits of randomness. Practical collision freedom achieved. | ✅ Verified |
| Ephemeral detection via prefix | `is_ephemeral()` returns `self.0.starts_with("ephemeral-")`. | ✅ Verified |

### Constraints — PASS

| Constraint | Source Evidence | Verdict |
|---|---|---|
| Non-empty requirement | Empty strings filtered at every source (header `.filter(\|s\| !s.is_empty())`, body `if flow_id.is_empty()`). | ✅ Verified |
| JSON body envelope | Specific structure required: `metadata.flow_id` string. Deviations skipped via `?` operator chain. | ✅ Verified |
| Silent degradation | All fallible operations use `.ok()?` or `?` patterns — no `log::warn`, no error propagation. | ✅ Verified |
| No cross-request persistence | `resolve()` is purely functional — no global state, no caching. | ✅ Verified |
| Prefix collision | `is_ephemeral()` uses `starts_with("ephemeral-")` — user-supplied `"ephemeral-foo"` is classified as ephemeral. | ✅ Verified |

### Rationale — PASS

Rationale is explanatory text. No claim contradicts the implementation. The design choices (fixed precedence, guaranteed resolution, empty-string rejection, silent skip, metric cardinality control) are all reflected in the code.

### Related — PASS

| Link | Verdict |
|---|---|
| `[[?c_flow_context]]` | Unresolved placeholder — appropriate for draft spec. |
| `[[?c_flow_metric]]` | Unresolved placeholder — appropriate for draft spec. |
| `[[src/flow/identify.rs]]` | File exists at repo root path. | ✅ Verified |
| `[[src/flow/mod.rs]]` | File exists at repo root path. | ✅ Verified |

---

## Findings

### F1: missing_interface — Trait derivates not documented

**Severity:** Medium

`FlowId` derives `Clone`, `PartialEq`, `Eq`, and `Hash`, none of which are documented in the Interface section. These are public trait implementations that affect how consumers interact with `FlowId`:

- `PartialEq` / `Eq` — callers can compare flow identifiers for equality (used in tests: `assert_ne!(id1, id2)`).
- `Hash` — enables use of `FlowId` as a map/set key (used by `FlowRegistry` internally, but the trait is public).
- `Clone` — enables value duplication without reference semantics.

These traits are part of the public contract and should be documented, especially since equality semantics are relevant to consumers who may deduplicate or cache identifiers.

### F2: undocumented_behavior — `Debug` format differs from `Display`

**Severity:** Low

`FlowId` implements `std::fmt::Debug` with format `FlowId("value")` and `std::fmt::Display` with format `"value"`. The spec documents `Display` but not `Debug`. The different output shapes between these two formatting traits are a consumer-facing behavior. For logging and debugging pipelines that rely on `Debug` output, the `FlowId(...)` wrapper is a structural difference from the raw string produced by `Display`.

### F3: spec_error — Minor header name casing note

**Severity:** Informational

The spec references the header as `X-LLM-Flow-ID` (canonical HTTP casing). The implementation uses `headers.get("x-llm-flow-id")` (lowercase). `HeaderMap::get()` performs case-insensitive lookup, so there is no behavioral discrepancy. However, the spec could clarify that the header match is case-insensitive, matching standard HTTP header semantics. This is a documentation completeness note, not a correctness issue.

---

## Summary

| Category | Count |
|---|---|
| Pass (verified claims) | 29 |
| missing_interface | 1 |
| undocumented_behavior | 1 |
| spec_error | 0 |
| bug | 0 |
| missing_interface | 1 |

**Overall verdict: PASS with 2 minor findings.** The spec is well-aligned with the implementation. The two findings concern undocumented public trait implementations on `FlowId` — these are interface surfaces that external consumers may depend on, particularly `Eq` and `Hash` for deduplication and `Clone` for value semantics. No correctness bugs, no invariant violations, no "No How" lint violations detected.
