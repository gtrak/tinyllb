# c_gateway_errors

Gateway proxy errors classify every failure condition between the gateway and backend, then map each to a single, deterministic HTTP response. The mapping is exhaustive: no condition falls through unhandled.

## Purpose

This concept guarantees that every backend-proxy failure is observable as a well-defined HTTP response. Each failure class — backend error, transport failure, backpressure, size limit, timeout — has a unique status code and response semantics so clients can distinguish root causes without inspecting internals. Failures are logged at structured severity levels, and HTTP-client errors are automatically normalized into the error taxonomy.

## Non-goals

Gateway proxy errors handle only response construction for proxy failures. They do not govern retry strategies, health aggregation, body validation, or diagnostic enrichment.

- **Client-facing retry logic.** The error response provides `Retry-After` for backpressure rejections, but does not define retry backoff strategies or client reconnection policies.
- **Backend health aggregation.** Individual proxy failures are not collected into health dashboards or circuit-breaker signals within this concept; that belongs to [[?c_backend_health]].
- **Request-body validation.** The too-large condition only triggers when the upstream proxy layer has already detected an oversized body; schema or semantic validation is outside scope.
- **Debug information propagation.** Backend error debug rendering omits both body and headers from diagnostic output; structured diagnostic payloads for downstream tooling are not provided.

## Interface

The gateway exposes a uniform contract: every proxy failure becomes an HTTP response with a status code, body, and optional headers. Callers deliver an error value and receive a complete HTTP response.

### Error taxonomy

- **Backend error.** The backend returned an HTTP response indicating failure (4xx or 5xx). The gateway echoes the original status, headers, and body without modification.
- **Network failure.** Communication with the backend failed. Maps to 502 Bad Gateway. Response body is plain text `"Bad Gateway"`.
- **Internal error.** A non-transport, non-backend failure within the proxy path. Maps to 500 Internal Server Error. The diagnostic message string appears both in structured error-level logs and verbatim in the HTTP response body.
- **Payload too large.** The request body exceeded the configured size limit. Maps to 413 Payload Too Large. Response body is plain text `"Request body too large"`.
- **Backpressure rejection.** The backend queue was full and rejected the request. Maps to 429 Too Many Requests with a `Retry-After` header and JSON body `{"error":"queue full"}`.
- **Timeout.** The request exceeded the configured duration. Maps to 408 Request Timeout. Logs at warn level. Response body is plain text `"Request timed out"`.

### Automatic HTTP-client normalization

- Every HTTP-client error automatically becomes a Network failure; callers cannot produce HTTP-client errors outside this taxonomy. All client-side error conditions — transport, TLS, DNS, and protocol-level — are covered.

### Response construction guarantees

- Every error variant produces exactly one HTTP response; there are no uncovered cases.
- Backpressure rejections always include a `Retry-After` header whose value is a non-negative integer of seconds and `Content-Type: application/json`.
- Network failures log at error level; timeouts log at warn level; Internal errors log at error level; Backend error, TooLarge, and Rejected are not logged on their own.
- Non-Rejected error responses do not set `Content-Type`; clients infer content type from the body bytes.

## Invariants

The error taxonomy maintains a stable, exhaustive mapping between failure conditions and HTTP responses.

### Exhaustive coverage

- Every possible proxy error variant maps to exactly one HTTP status code; no condition lacks a response.
- The mapping from error to response is total and unambiguous — the same error variant always produces the same status code.

### Backend fidelity

- Backend errors are a faithful pass-through: the original status code, headers, and body propagate unchanged through the gateway. Header completeness depends on the caller supplying a complete header map.
- The gateway does not inspect, modify, or augment backend error payloads.

### Header correctness

- Backpressure rejections always carry `Retry-After` as a non-negative integer number of seconds, never fractional.
- Backpressure rejections always carry `Content-Type: application/json`.

### Transport classification

- Every HTTP-client error is classified exclusively as a Network failure; it cannot appear as Internal or any other variant.

### Size distinction

- A too-large payload is a distinct condition from an internal error, so it always maps to a distinct status code (413 vs 500).

### Debug rendering boundaries

- Backend error debug rendering exposes only the status code; both body and headers are omitted to avoid leaking payload data and request metadata in diagnostic logs.
- Network error debug rendering exposes the inner transport error.
- Internal error debug rendering exposes the full message string.
- TooLarge and Timeout debug rendering output only the variant name with no additional data.
- Rejected error debug rendering exposes the retry-after duration.

## Constraints

The error system operates under tight boundaries around response construction and observability.

### Response construction

- Backpressure rejection responses carry a `Retry-After` header value that is always a string of ASCII digits; it never contains non-ASCII or control characters.
- Only an exactly-zero retry duration yields a `Retry-After` value of `0`; any positive duration — however small — produces at least `1` second due to ceiling rounding.

### Observability limits

- Timeout errors carry no backend identifier or elapsed-duration data; they are observationally opaque.
- Log severity differs across conditions (error vs. warn), so uniform log filtering by severity conflates distinct failure classes.

### Classification boundaries

- Automatic conversion from HTTP-client errors covers all client-side error conditions, not only transport-level failures. Other sources of I/O errors require explicit classification into a specific variant.

## Rationale

The error taxonomy separates observable failure modes into distinct status codes so clients and intermediate proxies can route retries, circuit breaks, and user-facing messages without inspecting bodies or headers.

### Why exhaustive mapping

An exhaustive error-to-response mapping eliminates gaps where an unexpected condition silently drops the response or produces an unhandled panic.

### Why backend fidelity

The gateway is transparent for backend-originated errors; modifying status, headers, or body would obscure the backend's intent and break client-side error handlers.

### Why `Retry-After` for backpressure

A structured retry signal lets clients throttle themselves during congestion rather than hammering the gateway. Integer seconds comply with RFC 7231 and keep header parsing simple.

### Why HTTP-client error normalization

Automatic normalization of HTTP-client errors into a single Network variant prevents callers from mixing raw HTTP-client errors with the gateway's error taxonomy, which would duplicate error-handling logic across modules.

### Why Internal message dual exposure

The Internal error message appears in both logs and the HTTP response body to provide consistent diagnostic context across structured logging and client-facing responses. Whether this leaks internal details to the client is a deliberate trade-off.

## Related

- [[?c_request_proxy]] — the proxy layer that produces these error conditions when forwarding requests to backends.
- [[?c_backpressure]] — the backpressure mechanism whose queue-full signal becomes a Rejection error.
- [[?c_timeout]] — timeout configuration whose expiration produces a Timeout error.
- [[src/gateway/error.rs#ProxyError]] — error type and response construction.
