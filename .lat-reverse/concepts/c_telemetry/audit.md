# Audit: [[c_telemetry]] — Telemetry (Cycle 3 — Final)

## Scope

Compares `.lat-reverse/concepts/c_telemetry/spec.md` against `src/telemetry/mod.rs`.
Only `init()` is `pub`; `init_otlp()` is private (`fn`, not `pub fn`).

## Previous Audit Resolution

Cycle 2 audit identified 3 contradictions. All have been resolved in this spec revision:

| # | Classification | Cycle-2 Issue | Status |
|---|---|---|---|
| 1 | `spec_error` | `init_otlp` listed as public entry point | **FIXED** — removed from Interface, noted only in Constraints as private scaffold |
| 2 | `undocumented_behavior` | Empty-string `RUST_LOG` not covered | **FIXED** — explicitly documented in Interface (Filter directive) and Invariants |
| 3 | `spec_error` | "Terminates the process" overstates panic semantics | **FIXED** — corrected to "causes a panic in the calling thread" in Interface and Constraints |

## Contradictions

**No contradictions found.** The spec is consistent with the implementation across all sections.

### Verification Matrix

| Spec Claim | Code Evidence | Verdict |
|---|---|---|
| `RUST_LOG` absent → default `info,llm_qdisc_proxy=debug` | Line 29: `unwrap_or_else` with default | Match |
| `RUST_LOG` present-but-empty → empty filter passed | Line 29: `std::env::var` returns `Ok("")`, `unwrap_or_else` not triggered | Match |
| `LLM_QDISC_LOG_JSON="1"` → JSON output | Lines 23-25: `.map(\|v\| v == "1").unwrap_or(false)` | Match |
| JSON mode uses flattened events | Line 35: `.flatten_event(true)` | Match |
| Human-readable is the default/non-JSON format | Lines 37-38: else-branch calls `.init()` (default fmt layer) | Match |
| Output goes to stderr | `tracing_subscriber::fmt()` defaults to stderr; no `.with_writer()` override | Match |
| Duplicate `init()` call → panic in calling thread | `tracing_subscriber::fmt().init()` calls `set_global_default()` which panics | Match |
| Only `init()` is public; `init_otlp` is private | Line 22: `pub fn init()`, line 81: `fn init_otlp()` | Match |
| `init_otlp` calls `init()` | Line 84: `init()` | Match |

## No-How Lint

**Result: PASS**

No violations detected:

- **Control flow:** None. The spec describes contractual behavior and failure modes, not execution sequences or branches.
- **Data structure details:** None. No internal types, collections, field lists, or structural details.
- **Function names as concept identifiers:** `init()` is referenced only in the Interface section and the Related section — both permitted locations. Not used as a concept identifier in Purpose, Invariants, or Constraints.
- **Implementation-specific terminology:** Environment variable names (`RUST_LOG`, `LLM_QDISC_LOG_JSON`) are configuration contracts per the interface-first principle. Terms like "tracing subscriber" and "JSON output" describe observable behavior, not internals.
- **Source code links:** `[[src/telemetry/mod.rs#init]]` appears exclusively in the Related section — compliant with scope restriction rule.

## Summary

| Category | Count |
|---|---|
| `bug` | 0 |
| `spec_error` | 0 |
| `undocumented_behavior` | 0 |
| `missing_interface` | 0 |
| **Total contradictions** | **0** |

**Clean sections:** Purpose, Non-goals, Interface, Invariants, Constraints, Rationale, Related.

**Verdict:** The spec accurately reflects the implementation. All cycle-2 corrections have been properly applied. No new contradictions were introduced. The spec is audit-clean and ready for integration.
