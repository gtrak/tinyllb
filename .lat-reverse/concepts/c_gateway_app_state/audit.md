# Audit: c_gateway_app_state

**Scope**: `.lat-reverse/concepts/c_gateway_app_state/spec.md` against `src/gateway/mod.rs`
**Auditor role**: Final-cycle Auditor (report only — no rewriting, no fixing)
**Date**: 2026-08-03

---

## Findings

### 1. Public Write Access Understated in Spec — `spec_error`

**Location**: Interface → State object — "Provides read access to the HTTP client, backend URL, metrics, scheduler, flow registry, backpressure configuration, and an optional per-request timeout."

**Issue**: The spec states the state object "provides read access" to its fields. All seven fields of `AppState` are declared `pub`, which grants read-**write** access. Any caller with a clone can mutate `backend_url`, `backpressure`, `request_timeout`, or any other field. The spec understates the interface contract.

**Source evidence**: `src/gateway/mod.rs` lines 18–28 — every field of `AppState` is `pub`.

**Impact**: Callers can mutate shared state fields without restriction, which may break the "stable access" guarantee stated in the Purpose. The spec should either (a) document write access as part of the contract or (b) flag the mismatch if read-only was intended.

---

### 2. Shallow-Clone Invariant Rests on Library Implementation Detail — `undocumented_behavior`

**Location**: Invariants → Shareability — "Cloning the state is shallow — references to heavyweight resources are not duplicated."

**Issue**: Five fields use explicit `Arc<T>` wrappers (`backend_url`, `metrics`, `scheduler`, `flow_registry`, and indirectly `Metrics`/`Scheduler`/`FlowRegistry`), making the shallow-reference pattern structurally visible. However, `client: reqwest::Client` is a direct value — not `Arc<reqwest::Client>`. The shallow-cloning guarantee for the HTTP client depends entirely on reqwest's internal Arc-based sharing, which is invisible in the AppState struct. If reqwest changes its clone semantics, this invariant silently breaks without any structural signal in AppState.

The spec does not document this asymmetry: five fields achieve shallow cloning by explicit Arc, one achieves it by opaque library behavior.

**Source evidence**: `src/gateway/mod.rs` line 19 — `client: reqwest::Client` (no Arc wrapper).

**Impact**: The invariant "cloning is shallow" is not uniformly enforceable by the struct's own type signature for the client field.

---

### 3. Rationale Violates "No How" Constraint — `spec_error`

**Location**: Rationale — "A mix of shared references and direct values keeps cloning lightweight: heavyweight resources are shared, and lightweight configuration is copied directly."

**Issue**: This sentence discloses data structure details ("shared references", "direct values", "heavyweight resources", "lightweight configuration") that the "No How" constraint explicitly forbids. The constraint states: "Reject outputs that include: Control flow descriptions, Data structure details, Function/method names as concept identifiers, Implementation-specific terminology."

The Rationale section is not exempt from "No How." Design rationale should be expressed in terms of domain properties (e.g., "clone cost is bounded independently of payload size"), not internal memory layout.

**Spec text**: "A mix of shared references and direct values keeps cloning lightweight..."

**Impact**: The spec leaks implementation details that would become invalid if the underlying types changed.

---

### 4. State Ownership Contract Not Specified — `missing_interface`

**Location**: Interface → Router factory — "Returns a router typed for `AppState` shared state; the caller must supply the state after construction before serving."

**Issue**: The spec omits how the caller supplies state to the router. The source returns `Router<AppState>`, which in Axum requires `.with_state(app_state)` to be called with a bare `AppState` value. The spec should document that the state is supplied directly (not wrapped in `Arc`, `Box`, or any other container), since this is a caller-facing contract.

**Source evidence**: `src/gateway/mod.rs` line 38 — `pub fn create_router() -> Router<AppState>`.

**Impact**: A reader reconstructing the integration code from spec alone would not know the state is provided unwrapped.

---

### 5. Sub-module Interfaces Not Specified — `missing_interface`

**Location**: Interface → Sub-modules — "The `error` sub-module defines gateway-specific error types...", "The `proxy` sub-module contains the unified request proxying logic...", "The `stream` sub-module defines streaming support..."

**Issue**: The spec describes three public sub-modules but provides no interface contracts for any of them. The description uses implementation-level language ("defines", "contains", "defines") rather than contractual language ("exposes", "guarantees", "accepts", "returns"). For consumers of the gateway module, knowing what these sub-modules export is part of the interface surface.

**Source evidence**: `src/gateway/mod.rs` lines 1–3 — `pub mod error; pub mod proxy; pub mod stream;`.

**Impact**: Without sub-module contracts, a reader cannot determine what is importable from `gateway::error`, `gateway::proxy`, or `gateway::stream` without reading source.

---

## "No How" Lint

| Check | Result |
|---|---|
| Control flow descriptions | Clean — no step-by-step process descriptions found |
| Data structure details | **VIOLATION** — Rationale bullet: "A mix of shared references and direct values..." discloses memory layout |
| Function/method names as concept identifiers | Clean — uses "router factory", "client factory", "State object" as concept names |
| Implementation-specific terminology | **VIOLATION** — Rationale uses "shared references" and "direct values" as structural descriptions rather than domain concepts |

---

## Section-by-Section Verification Summary

| Spec Section | Status | Notes |
|---|---|---|
| Purpose | Consistent | All claims match the AppState struct and module role |
| Non-goals | Consistent | No validation, lifecycle, proxy semantics, backpressure policy, or TLS negotiation present in source |
| Interface → State object | **Mismatch** | Understates field access level (see Finding 1) |
| Interface → Router factory | Mostly consistent | Missing state ownership detail (see Finding 4) |
| Interface → Client factory | Consistent | 300s timeout, no-arg, panic-on-fail all match source |
| Interface → Sub-modules | **Gap** | No contracts provided (see Finding 5) |
| Invariants → Shareability | **Partial** | Asymmetric Arc usage undocumented (see Finding 2) |
| Invariants → Timeout semantics | Consistent | Optional, uniform across streaming/non-streaming matches source comment |
| Invariants → Endpoint delegation | Consistent | All three routes bind to `proxy_handler` |
| Constraints | Consistent | All five constraints match the source |
| Rationale | **Violation** | "No How" violations (see Finding 3) |
| Related | Consistent | Source code links verified; file paths correct |

---

## Classification Summary

| # | Finding | Classification |
|---|---|---|
| 1 | Public write access understated | `spec_error` |
| 2 | Non-Arc client field hides shallow-clone dependency | `undocumented_behavior` |
| 3 | Rationale contains data structure details | `spec_error` |
| 4 | State ownership contract not specified | `missing_interface` |
| 5 | Sub-module interfaces not specified | `missing_interface` |

**Total**: 5 findings — 2 `spec_error`, 1 `undocumented_behavior`, 2 `missing_interface`, 0 `bug`.
