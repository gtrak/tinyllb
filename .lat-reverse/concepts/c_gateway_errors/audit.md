# Audit: c_gateway_errors

**Auditor:** Cycle 2  
**Spec:** `.lat-reverse/concepts/c_gateway_errors/spec.md`  
**Source:** `src/gateway/error.rs`

---

## "No How" Lint

| Section | Violation | Status |
|---|---|---|
| Interface → Automatic HTTP-client normalization | References `reqwest` library name | spec_error |
| Constraints → Classification boundaries | References `reqwest` implicitly via "HTTP-client errors" (acceptable as interface-level) | pass |
| All sections | No control flow descriptions | pass |
| All sections | No data structure details | pass |
| All sections | No function/method names as concept identifiers | pass |

### Detail: Implementation-specific terminology

The spec says: *"This covers all `reqwest` errors, not only transport errors."* (Interface → Automatic HTTP-client normalization). The name `reqwest` is an implementation library, not a domain concept. A rewritten implementation might use `hyper`, `ureq`, or any HTTP client — the invariant is "every HTTP-client error becomes a Network failure," not "every `reqwest::Error` becomes a Network failure." This violates the "No How" constraint (implementation-specific terminology).

---

## Spec vs. Source Findings

### 1. spec_error — Retry-After "near-zero" constraint is inaccurate

**Spec claim** (Constraints → Response construction): *"A zero or near-zero retry duration yields a `Retry-After` value of `0`."*

**Source behavior:** `retry_secs = (retry_after.as_secs_f64().ceil()) as u64`. The `ceil()` function rounds up, so only an exactly-zero duration produces `0`. Any positive duration — even one nanosecond — produces at least `1`.

**Verdict:** The spec's use of "near-zero" implies a range around zero that all map to `0`, but `ceil()` maps every positive value to ≥ 1. The constraint text is either inaccurate or the implementation does not satisfy it. If "near-zero" means "exactly zero," the phrasing is misleading. If it means "small but positive," the implementation contradicts the spec.

---

### 2. undocumented_behavior — Incomplete Debug rendering specification

**Spec claim** (Invariants → Debug rendering boundaries): Describes Debug output for three variants:
- Backend error omits response body
- Internal exposes full message string
- Rejected exposes retry-after duration

**Source behavior:** The `impl fmt::Debug` covers all six variants:
- `Network` → exposes the inner `reqwest::Error` via `{:?}`
- `TooLarge` → outputs `"TooLarge"`
- `Timeout` → outputs `"Timeout"`

**Verdict:** Network, TooLarge, and Timeout Debug rendering are not documented in the spec. These are observable interface surfaces (Debug output is consumed by logging and diagnostics). The missing documentation means a rewritten implementation could produce different Debug output for these three variants without violating the spec.

---

### 3. undocumented_behavior — BackendError Debug omits headers

**Spec claim** (Invariants → Debug rendering boundaries): *"Backend error debug rendering omits the response body to avoid leaking payload data in diagnostic logs."*

**Source behavior:** `write!(f, "BackendError {{ status: {:?}, .. }}", status)` — omits both `body` AND `headers`. The spec only mentions body omission.

**Verdict:** The spec states that the body is omitted but is silent on headers. The implementation also omits headers, which is a stronger privacy guarantee than the spec requires. This omission of headers from Debug output is undocumented.

---

### 4. Verified: Correct match items

The following spec claims are verified as correct against the source:

| Claim | Source Evidence |
|---|---|
| Backend error echoes status, headers, body unchanged | `BackendError { status, headers, body }` assigned directly to response |
| Network → 502 Bad Gateway | `StatusCode::BAD_GATEWAY` |
| Internal → 500 with message in body and error log | `StatusCode::INTERNAL_SERVER_ERROR`, `tracing::error!(...)`, `msg` in response |
| TooLarge → 413 "Request body too large" | `StatusCode::PAYLOAD_TOO_LARGE` |
| Rejected → 429 with Retry-After + JSON body | `StatusCode::TOO_MANY_REQUESTS`, `{"error":"queue full"}` body, headers set |
| Timeout → 408 with warn log | `StatusCode::REQUEST_TIMEOUT`, `tracing::warn!(...)` |
| Logging: Network=error, Timeout=warn, Internal=error, others=not logged | Confirmed in `into_response` arms |
| `From<reqwest::Error>` always produces Network | `impl From<reqwest::Error> for ProxyError` → `ProxyError::Network(err)` |
| Exhaustive match — all variants produce exactly one response | `match self` covers all six variants |
| Retry-After is non-negative integer seconds | `(as_secs_f64().ceil()) as u64` guarantees non-negative integer |
| Backpressure sets `Content-Type: application/json` | `HeaderValue::from_static("application/json")` |
| Non-Rejected responses omit Content-Type | All non-Rejected arms use `(StatusCode, &str)` tuple; no explicit Content-Type |
| Internal Debug exposes full message | `write!(f, "Internal({})", msg)` |
| Rejected Debug exposes retry-after | `write!(f, "Rejected {{ retry_after: {:?} }}", retry_after)` |

---

## Summary

| Classification | Count | Items |
|---|---|---|
| spec_error | 2 | `reqwest` library reference; "near-zero" Retry-After constraint |
| undocumented_behavior | 2 | Incomplete Debug rendering spec; BackendError header omission |
| bug | 0 | — |
| missing_interface | 0 | — |

No blocking issues. The spec is substantially accurate; findings are corrective rather than structural.
