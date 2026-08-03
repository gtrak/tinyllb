# c_gateway_errors — Extraction

## Concept ID
[[c_gateway_errors]]

## Source
- [[src/gateway/error.rs]]

---

## Responsibilities (observable behavior)

- Encapsulates every condition under which a request proxied from the gateway to a backend fails.
- Maps each failure condition to a standardized HTTP response (status code, and for some conditions a body/headers) delivered to the client.
- Records each failure through a structured tracing log (error vs. warn level differs by condition).
- Provides a conversion so communication failures with the backend are automatically normalized into the error domain.

## Interface surfaces

### ProxyError — error contract type
- Exported error type for all backend-proxy failure conditions. Public, used by callers that proxy requests.
- Distinguishable variants, each with observable data:
  - Backend failure carrying the backend's status, headers, and raw body.
  - Network/communication failure carrying the underlying transport error.
  - Generic internal failure carrying a message.
  - Request body too large (no data).
  - Backpressure rejection carrying a retry delay.
  - Request timeout (no data).
- Evidence: `src/gateway/error.rs:7`.
- `Debug` output is defined manually per variant; for the backend-failure variant the body is not included in the debug rendering (`src/gateway/error.rs:26`).

### Conversion to HTTP responses
- Every condition converts to an HTTP response; behavior per condition (observed, from `src/gateway/error.rs:43`):
  - Backend failure → response echoes the backend's original status, headers, and body unchanged.
  - Network/communication failure → 502 Bad Gateway with body text "Bad Gateway"; logs at error level.
  - Generic internal failure → 500 Internal Server Error with the message as body; logs at error level.
  - Too large → 413 Payload Too Large with body "Request body too large".
  - Backpressure rejection → 429 Too Many Requests with JSON body `{"error":"queue full"}`; a `Retry-After` header set to the retry-after duration rounded up to whole seconds (per RFC 7231); `Content-Type: application/json`.
  - Timeout → 408 Request Timeout with body "Request timed out"; logs at warn level.

### Status codes delivered
- Backend failure: carries the backend-supplied code (variable).
- Network failure: 502.
- Internal: 500.
- Too large: 413.
- Rejected: 429.
- Timeout: 504.
- Evidence: `src/gateway/error.rs:43-90`.

### Automatic construction from transport errors
- A communication error with a backend automatically converts into the Network failure condition. Evidence: `src/gateway/error.rs:93`.

## Interface surfaces exposed to consumers
- Error-carrying app-wide responses obtain an HTTP response via the standardized conversion.
- The Debug rendering is used for logs/tagging.
- Automatic conversion allows callers to propagate transport failures without ad-hoc mapping.

## Invariants (with evidence)

1. Every condition produces exactly one HTTP response; no condition falls through without a response mapping. Evidence: exhaustive match in `src/gateway/error.rs:45`.
2. Backend failure responses are a faithful pass-through (same status, headers, body). Evidence: `src/gateway/error.rs:51-54`.
3. Rejection responses always carry a valid integer `Retry-After` header (seconds, ceiling-rounded) and JSON content type. Evidence: `src/gateway/error.rs:72-82`.
4. A transport error is always classified as Network (never any other variant). Evidence: `src/gateway/error.rs:95`.
5. The payload-too-large condition is represented as a distinct condition from internal errors, so it maps to a distinct status code. Evidence: variants `TooLarge` vs `Internal`, `src/gateway/error.rs:17,19`.

## Failure modes (observable risk spots)

- The `Retry-After` header construction uses a panicking conversion with the assumption that the operator value is a valid-header character; if the ceil of a duration `as_secs_f64()` ever yielded a non-ASCII or invalid header value, the process could abort. Evidence: `src/gateway/error.rs:74-78`.
- The timeout condition does not signal which backend or how long elapsed; observationally a timeout carries no debug context. Evidence: `src/gateway/error.rs:85`.
- Log verbosity is inconsistent across conditions (error vs. warn), which can make uniform mis-analysis of failures unreliable. Evidence: error traces at `src/gateway/error.rs:57,61`; warn at `src/gateway/error.rs:86`.