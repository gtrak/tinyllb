# c_backend_monitor — Cycle 3 (Final) Audit

## Scope

Monitor contract: parsing, observation publication, conditional blocking, construction, and metrics reporting.

## Contradictions and Remaining Issues

### 1. No-How Lint — Implementation Terminology in Spec

**Classification**: spec_error

**Findings**: The spec contains six instances of implementation-specific terminology that violate the "No How" constraint. These terms reference Tokio types and internal constructs rather than domain concepts.

| Location (section) | Violating term | Domain replacement |
|---|---|---|
| Purpose, bullet 2 | "watch channel" | "observation channel" |
| Interface, Observation publication | "watch channel" | "observation channel" |
| Interface, Observation publication | "channel closure" | "stream termination" |
| Interface, Conditional blocking | "the channel is closed" | "the stream terminates" |
| Interface, Construction | "watch receiver" | "reader handle" |
| Interface, Construction | "task handle" | "background operation handle" |

**Evidence**: `spec.md` lines 8, 28, 29, 30, 30. Source uses `tokio::sync::watch::Receiver`, `tokio::sync::watch::Sender`, `tokio::task::JoinHandle`.

**Impact**: The spec should survive a complete implementation rewrite (e.g., swapping Tokio for async-std, or using a different channel type). These terms lock the spec to the current Tokio implementation.

---

### 2. Constructor Dependency Not Documented

**Classification**: missing_interface

**Spec claim**: "The standard constructor returns the monitor alongside an optional background task handle; a missing handle indicates monitoring is disabled." (Interface: Construction)

**Omission**: The spec does not document that the standard constructor requires a metrics reporting dependency (`Arc<Metrics>`) and an HTTP client. The "Metrics reporting" bullet describes gauge writes as a behavioral outcome but does not connect it to construction prerequisites. A consumer reading only the Interface section cannot determine what inputs are needed to instantiate a fully functional monitor.

**Evidence**: `src/backend/mod.rs` lines 195–198 — `new(config: &BackendConfig, metrics: Arc<Metrics>, client: reqwest::Client)`. Lines 249–250 depend on the `metrics` parameter for gauge updates.

**Impact**: A rewrite attempt cannot reproduce the construction contract without examining source code.

---

### 3. Predicate Trait Bounds Not Documented

**Classification**: missing_interface

**Spec claim**: "Callers may suspend until an observation satisfies a caller-supplied predicate." (Interface: Conditional blocking)

**Omission**: The spec does not document that the predicate must satisfy `Send + Sync` trait bounds. This is a precondition on what predicates are acceptable.

**Evidence**: `src/backend/mod.rs` line 276 — `impl Fn(&BackendSnapshot) -> bool + Send + Sync`.

**Impact**: A caller constructing an inline closure with non-Send state will get a compile error with no spec-level guidance on the constraint.

---

### 4. Parsing Available as Standalone Function

**Classification**: undocumented_behavior

**Spec claim**: "Parsing: Accepts raw Prometheus text bodies and returns a typed observation..." (Interface: Parsing)

**Omission**: The spec does not clarify that parsing is exposed as a standalone public function (`pub fn parse_snapshot`), independently callable without a monitor instance. The current description reads as an internal capability of the monitor rather than a directly accessible API surface.

**Evidence**: `src/backend/mod.rs` line 123 — `pub fn parse_snapshot(body: &str) -> ParseSnapshotResult`. Used by both `poll_loop` and integration tests (per doc comment).

**Impact**: Callers needing one-shot metric parsing (e.g., deployment validation) cannot discover this capability from the spec alone.

## Resolved Issues (from Cycle 2)

- **Cycle 2 finding #1** (`snapshot()` never returns `None`): The spec was corrected to say "always returns the latest observation — including after channel closure." This now matches implementation behavior. **Resolved.**
- **Cycle 2 finding #2** (`wait_for` doc comment claims `bool` return): The spec was already correct ("caller cannot distinguish"). This was a code doc comment inaccuracy, not a spec error. The code comment remains inaccurate but is outside the audit scope. **No spec change needed.**

## No-How Lint

| Check | Status |
|---|---|
| Control flow descriptions in spec | **FAIL** — No-How violations listed above (§1) |
| Data structure details in spec | Pass |
| Function/method names as concept identifiers | Pass |
| Implementation-specific terminology in spec | **FAIL** — "watch channel", "watch receiver", "channel closure", "task handle" (§1) |

## Summary

| # | Finding | Classification |
|---|---|---|
| 1 | Six No-How violations: implementation terminology leaks into Purpose and Interface sections | spec_error |
| 2 | Constructor requires `Arc<Metrics>` and `reqwest::Client` — not documented | missing_interface |
| 3 | Predicate `Send + Sync` bounds omitted from contract | missing_interface |
| 4 | `parse_snapshot` callable independently of monitor — not documented | undocumented_behavior |
