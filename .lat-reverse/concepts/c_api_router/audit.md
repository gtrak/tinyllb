# c_api_router — Audit (Cycle 2)

## Scope

Spec: `.lat-reverse/concepts/c_api_router/spec.md`
Implementation: `src/api/mod.rs`, `src/api/flows.rs`, `src/api/queue.rs`

## Verdict

The spec and implementation are largely aligned. Two contradictions found: one `undocumented_behavior` and one `missing_interface`. No bugs or spec errors detected. The spec passes the "No How" lint.

## Contradictions

### 1. Flow registration response body contains undocumented fields — undocumented_behavior

**Spec claim:** The flow registration endpoint response documents only the `status` field (`"created"` or `"updated"`). The spec states: "Returns `201 Created` with status `"created"`" and "Returns `200 OK` with status `"updated"`".

**Implementation reality:** `RegisterFlowResponse` (src/api/flows.rs:22-28) contains four fields: `id`, `weight`, `priority`, and `status`. The handler echoes back the request's `id`, `weight`, and `priority` in every success response.

**Evidence:** `flows.rs` lines 68-76 return the full struct. The spec Interface section only documents the `status` field.

**Classification:** `undocumented_behavior` — the implementation includes additional response fields (`id`, `weight`, `priority`) not described in the spec contract.

### 2. Error response body format is absent — missing_interface

**Spec claim:** The spec states "Returns `400 Bad Request` when weight or priority violate their domain bounds" and claims "Each public endpoint carries a precise input contract, output guarantee, and error classification."

**Implementation reality:** The handler returns `Err((StatusCode::BAD_REQUEST, String))` which produces a plain text error message (e.g., `"weight must be greater than 0"`). The error response body structure is not documented.

**Evidence:** `flows.rs` lines 41-43, 49-51 produce unstructured string error bodies. No error response schema is specified in the Interface section.

**Classification:** `missing_interface` — the spec omits the error response body format despite claiming precise error classification.

## "No How" Lint

**Result: PASS**

The spec contains no violations of the "No How" constraint:

- **Control flow:** Absent. The spec describes what each endpoint accepts and returns, not how it processes requests.
- **Data structure details:** Absent. No internal type shapes or field lists beyond what is necessary for the interface contract (input/output field names in domain terms).
- **Function/method names as concept identifiers:** Absent. The Purpose, Interface, Invariants, Constraints, and Rationale sections use domain concepts ("flow registration", "queue state", "scheduling weight") — never function names. Source code links appear only in Related.
- **Implementation-specific terminology:** Absent. No references to internal algorithms, routing mechanisms, or framework details.

## Section-by-section validation

| Section | Status | Notes |
|---|---|---|
| Purpose | Clean | Accurate high-level description. "Parameterized over application state" is consistent with `Router<AppState>`. |
| Non-goals | Clean | Matches: no inference, no auth, no scheduling logic, no deletion. |
| Interface — Flow registration | Partial | Status codes and `status` field documented; response echoes `id`, `weight`, `priority` undocumented. |
| Interface — Queue state | Clean | All response fields (`active`, `waiting`, `flows`) and 1-indexed positions documented. |
| Invariants | Clean | All five invariants verified against implementation. Weight > 0 enforced, priority [0, 100] enforced, status deterministic, positions 1-indexed, queue handler never errors. |
| Constraints | Clean | Exactly two endpoints verified. Upsert-only via POST verified. Immediate validation verified. Unsigned queue positions verified. |
| Rationale | Clean | Domain-level reasoning; no implementation leakage. |
| Related | Clean | All five source links verified as existing symbols. |

## Related link verification

| Link | Exists |
|---|---|
| `src/api/mod.rs#create_router` | Yes — `pub fn create_router() -> Router<AppState>` |
| `src/api/flows.rs#RegisterFlowRequest` | Yes — `pub struct RegisterFlowRequest` |
| `src/api/flows.rs#register_handler` | Yes — `pub async fn register_handler` |
| `src/api/queue.rs#QueueResponse` | Yes — `pub struct QueueResponse` |
| `src/api/queue.rs#queue_handler` | Yes — `pub async fn queue_handler` |
