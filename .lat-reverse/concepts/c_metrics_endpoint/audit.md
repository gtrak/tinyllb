# Audit: Metrics Endpoint (`c_metrics_endpoint`)

## Audit scope

- **Spec**: `.lat-reverse/concepts/c_metrics_endpoint/spec.md` (twice-corrected)
- **Source**: `src/metrics/endpoint.rs`
- **Cross-reference**: `src/metrics/mod.rs`, `src/gateway/mod.rs`, `src/main.rs`

## "No How" lint

**Result: Clean.** The spec contains no control flow descriptions, data structure details, function/method names as concept identifiers, or implementation-specific terminology. All statements are phrased as domain contracts.

---

## Findings

### 1. spec_error — Unverifiable Content-Type error response claim

**Location**: Interface, bullet 2 and Invariants, bullet 3

**Spec claim**: *"Returns `500 Internal Server Error` with empty body only when metric serialization fails"* and *"error responses do not include a `Content-Type` header"*

**Source evidence**: `endpoint.rs` line 19:
```rust
return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
```

**Analysis**: The spec asserts two sub-claims about the 500 response: (a) empty body, (b) no Content-Type header. Both are true for axum's `StatusCode::into_response()` on the current version of axum. However, these are properties of the axum library's internal conversion, not contractual guarantees of the endpoint. The body being empty and headers being absent depend on axum's implementation details of `StatusCode::into_response()`. A library upgrade could change the behavior (e.g., axum could add default headers). The spec should either:
- Weaken to "Returns `500 Internal Server Error`" (omitting body/header specifics), or
- Explicitly state these as implementation-dependent axum behaviors, not endpoint invariants.

**Severity**: Low — the claims are currently accurate but tie the spec to axum internals.

---

### 2. undocumented_behavior — Method rejection behavior not specified

**Location**: Interface section

**Source evidence**: `main.rs` line 16:
```rust
.route("/metrics", get(metrics::endpoint::metrics_handler))
```

**Analysis**: The spec documents only `GET /metrics` behavior. It does not specify what occurs when non-GET methods (POST, PUT, DELETE, etc.) target `/metrics`. Axum's `get()` route matcher returns `405 Method Not Allowed` for unsupported methods. This is a standard HTTP behavior, but the spec neither documents nor denies it. For completeness, the Interface section should note that only GET is supported and other methods are rejected with `405`.

**Severity**: Low — standard HTTP behavior, but absent from the contract.

---

### 3. undocumented_behavior — "OpenMetrics" vs. "Prometheus" nomenclature mismatch in source

**Location**: `endpoint.rs` line 10 (source doc comment)

**Source evidence**:
```rust
/// Serve Prometheus-format metrics at `GET /metrics`.
///
/// Returns `200 OK` with `text/plain; version=0.0.4` per the OpenMetrics
/// specification.
```

**Analysis**: The source doc comment on line 10 references the "OpenMetrics specification," but the endpoint serves Prometheus text exposition format (not OpenMetrics). OpenMetrics is a distinct format with additional headers (`Content-Type: application/openmetrics-text`) and `# EOF` terminators. The spec correctly identifies the format as "Prometheus text exposition format version 0.0.4." The source comment is inaccurate, but this is a source documentation issue, not a spec error. Flagged here for awareness that the source and spec use different terminology for the same observable behavior.

**Severity**: Informational — no spec impact; source comment should be corrected independently.

---

### 4. undocumented_behavior — `metrics_handler` is `pub` but not documented as a function interface

**Location**: `endpoint.rs` line 12

**Source evidence**:
```rust
pub async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
```

**Analysis**: The handler function has `pub` visibility within the crate. It is consumed by `main.rs` via `metrics::endpoint::metrics_handler`. The spec documents only the HTTP endpoint contract (`GET /metrics`). The function-level interface (async, takes `State<AppState>`, returns `impl IntoResponse`) is not captured. This is acceptable if the function is considered purely internal routing glue with no external consumers beyond the router. However, if other modules import and invoke `metrics_handler` directly, the spec would be missing a function interface. Current usage is limited to `main.rs` routing, so this is borderline.

**Severity**: Informational — function is crate-internal; HTTP contract is the relevant public surface.

---

## Verified claims (no issues)

| Spec Claim | Status | Source |
|---|---|---|
| Single HTTP endpoint at `GET /metrics` | ✅ Accurate | `main.rs:16` |
| No request parameters, headers, or body accepted | ✅ Accurate | Only extracts `State<AppState>` |
| Returns `200` with metric families on success | ✅ Accurate | `endpoint.rs:23-29` |
| Content-Type: `text/plain; version=0.0.4` on success | ✅ Accurate | `endpoint.rs:26-27` |
| No authentication/authorization | ✅ Accurate | No middleware guards in `main.rs` |
| ERROR-level log on serialization failure | ✅ Accurate | `endpoint.rs:18` |
| `500` on serialization failure | ✅ Accurate | `endpoint.rs:19` |
| All registered metric families in response | ✅ Accurate | `endpoint.rs:14` uses `registry.gather()` |
| No rate limiting or caching | ✅ Accurate | No middleware applied |
| Async operation | ✅ Accurate | `pub async fn` handler |

## Summary

| Category | Count |
|---|---|
| `bug` | 0 |
| `spec_error` | 1 |
| `undocumented_behavior` | 3 |
| `missing_interface` | 0 |

The spec is substantially accurate. The single spec error ties endpoint invariants to axum library internals (body/header behavior on 500 responses). The three undocumented behaviors are minor: method rejection semantics, source nomenclature drift, and internal function visibility. No blocking issues found.
