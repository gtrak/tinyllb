# Audit: Gateway Proxy

**Spec version:** Twice-corrected (`.lat-reverse/concepts/c_gateway_proxy/spec.md`)
**Source files:** `src/gateway/proxy.rs`, `src/gateway/mod.rs`, `src/gateway/error.rs`, `src/gateway/stream.rs`, `src/flow/identify.rs`
**Date:** 2026-08-03

---

## "No How" Lint

### Violations found: none

The spec does not contain:
- Control flow descriptions
- Data structure internals or field lists
- Function/method names as concept identifiers (outside `Related`)
- Implementation-specific terminology

**Exception (acceptable):** Source code wiki links appear exclusively in the `Related` section, which is permitted by the scope restriction rule.

---

## Findings

### F-1: Undocumented request body mutation

**Severity:** Medium | **Classification:** `undocumented_behavior`

The proxy injects `stream_options.include_usage: true` into the JSON request body for streaming requests (function `inject_include_usage` in `src/gateway/proxy.rs`). This modifies the backend-facing request to ensure the backend emits a final usage chunk for token accounting.

The spec's Non-goals section states:
> The proxy does not mutate response bodies; it only strips response headers and injects the request ID.

This claim is technically correct about *response* bodies, but the spec is silent about *request* body mutation. The `inject_include_usage` behavior is a deliberate, documented-in-code transformation that affects what the backend receives. Callers whose requests are proxied will have their backend request modified without visibility. This behavior is not captured in any spec section.

**Impact:** A caller sending `"stream": true` without `stream_options` will have the backend request silently modified. The caller never sees this modification (it affects only the backend-facing request), but it is a contract-relevant fact: the proxy is not a pure forwarder for streaming requests.

---

### F-2: Timeout bound description is misleading

**Severity:** Low | **Classification:** `spec_error`

The spec states under Constraints § Size and Time Bounds:
> The HTTP client enforces a 300-second upper bound independently of any configured request timeout.

The `build_client` function (`src/gateway/mod.rs:47-50`) configures reqwest with `.timeout(300s)`. However, reqwest's `.timeout()` is a per-request timeout on the entire HTTP transaction, not a global ceiling. In the proxy handler, the configured `request_timeout` (if present) is applied separately to the send phase (`tokio::time::timeout(timeout, builder.send())`) and the body collection phase (`tokio::time::timeout(timeout, collect_response_body(...))`).

The phrase "upper bound" suggests a total-request ceiling. The actual behavior is: each forwarding phase is independently bounded by `min(request_timeout, 300s)` when `request_timeout` is set, or 300s when it is not set. The 300-second client timeout does **not** act as a global maximum on total elapsed time; it is a per-operation fallback.

**Verdict:** The statement is imprecise. "Upper bound" implies a hard cap on total duration, which is not what reqwest's timeout provides in this usage pattern.

---

### F-3: Body size boundary asymmetry in checks

**Severity:** Low | **Classification:** `spec_error`

The spec states:
> Requests with a body of 32 MiB or larger are rejected with `413 Payload Too Large`.

The implementation uses two checks with different comparison operators:

1. Content-Length header check (`proxy.rs:221-229`): `if size > MAX_BODY_SIZE` — strictly greater than 32 MiB.
2. Collected body check (`proxy.rs:248`): `if body_bytes.len() >= MAX_BODY_SIZE as usize` — greater-than-or-equal.

For requests with a Content-Length header of exactly 32 MiB: the first check passes (not strictly greater), the body is collected, the second check triggers, and `413` is returned. Correct.

For requests with a Content-Length header just above 32 MiB (e.g., 33 MiB): the first check triggers immediately and `413` is returned without collecting the body. Correct.

For chunked requests without Content-Length: the first check is skipped, the body is collected, the second check triggers at 32 MiB and `413` is returned. Correct.

**Verdict:** The overall behavior matches the spec (anything ≥ 32 MiB is rejected), but the dual-boundary operators are an implementation detail the spec abstracts away correctly. This is noted as `spec_error` only because the spec's singular statement "32 MiB or larger" does not match the two distinct comparison points in the code. The spec is functionally correct; the discrepancy is internal precision.

---

### F-4: `Content-Length` header not documented as stripped for modified bodies

**Severity:** Low | **Classification:** `undocumented_behavior`

When `inject_include_usage` modifies the request body for streaming requests, the handler explicitly removes the `Content-Length` header from the backend-facing request (`proxy.rs:274`):
```rust
headers.remove(axum::http::header::CONTENT_LENGTH);
```

This is necessary because reqwest recomputes Content-Length from the new body. The spec does not mention that the proxy may drop the `Content-Length` header on the forwarded request when the body is modified.

This is an internal concern (the caller never observes this), but it is part of the proxy's contract with the backend, not with the caller.

---

### F-5: Error response body collection on backend 4xx/5xx may return 502

**Severity:** Low | **Classification:** `spec_error`

The spec states under Response Contracts:
> Backend error responses (4xx, 5xx) are returned verbatim with filtered headers; the proxy does not alter status codes or error bodies.

However, when the backend returns a 4xx or 5xx response, the handler calls `collect_response_body(response, "error-response")` which can fail with a network error (the `?` on line 388 propagates `ProxyError::Network`). In that case, the handler returns `502 Bad Gateway` instead of the original backend error body.

**Verdict:** The spec claims backend errors are returned "verbatim" but the implementation can substitute a 502 when body collection fails during error response handling. The spec should acknowledge this edge case.

---

### F-6: `inject_include_usage` silently skips non-JSON bodies for streaming requests

**Severity:** Informational | **Classification:** `undocumented_behavior`

If a streaming request (identified by `"stream": true`) has a non-parseable JSON body, `inject_include_usage` returns `None` and the body is forwarded verbatim. This means the backend will not receive `stream_options.include_usage` and may not emit a final usage chunk, leading to undercounted token metrics for streaming requests with unusual body formats.

This is consistent with best-effort behavior elsewhere in the proxy (e.g., `extract_completion_tokens` returns 0 on parse failure), but it is not documented.

---

## Summary Table

| Finding | Classification | Severity | Section |
|---------|---------------|----------|---------|
| F-1 | undocumented_behavior | Medium | Non-goals / Interface |
| F-2 | spec_error | Low | Constraints § Size and Time Bounds |
| F-3 | spec_error | Low | Interface § Admission and Sizing |
| F-4 | undocumented_behavior | Low | (internal contract) |
| F-5 | spec_error | Low | Interface § Response Contracts |
| F-6 | undocumented_behavior | Informational | Interface § Token Accounting |

## Section-by-Section Verification

| Spec Section | Status | Notes |
|-------------|--------|-------|
| Purpose | ✅ Match | All claims verified against implementation |
| Guarantees | ✅ Match | All five guarantees confirmed |
| Non-goals | ⚠️ F-1 | Request body mutation undocumented |
| Interface § Routes | ✅ Match | Three routes confirmed |
| Interface § Admission and Sizing | ⚠️ F-3 | Boundary precision issue |
| Interface § Streaming Detection | ✅ Match | Both signals confirmed |
| Interface § Token Accounting | ⚠️ F-6 | Parse failure behavior undocumented |
| Interface § Scheduling Defaults | ✅ Match | 1024 default confirmed |
| Interface § Flow Identity | ✅ Match | Header + body + ephemeral confirmed |
| Interface § Error Responses | ✅ Match | All five error codes confirmed |
| Interface § Response Contracts | ⚠️ F-5 | Body collection edge case missing |
| Interface § Observability Spans | ✅ Match | Span fields confirmed |
| Interface § Configuration | ✅ Match | Config surface confirmed |
| Invariants § Header Filtering | ✅ Match | All header stripping rules confirmed |
| Invariants § Request Identity | ✅ Match | X-Request-ID confirmed |
| Invariants § Admission Holding | ✅ Match | RAII lifecycle confirmed |
| Invariants § Active Request Tracking | ✅ Match | Both paths confirmed |
| Invariants § Error Counter Asymmetry | ✅ Match | Counter increment points confirmed |
| Invariants § Token Reporting | ✅ Match | Best-effort token extraction confirmed |
| Constraints § Backend Dependency | ✅ Match | Single backend, no recovery confirmed |
| Constraints § Size and Time Bounds | ⚠️ F-2 | Timeout description imprecise |
| Constraints § Scheduling | ✅ Match | Defaults and no-retry confirmed |
| Constraints § Error Passthrough | ✅ Match | Verbatim passthrough confirmed |
| Rationale | ✅ Match | Design decisions consistent with implementation |
| Related | ✅ Match | Wiki links point to correct files |
