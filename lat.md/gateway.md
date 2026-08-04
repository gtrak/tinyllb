# Gateway Application State

The gateway application state aggregates all runtime dependencies required by the proxy layer to forward API requests to a vLLM backend. Every handler reads the same state instance, shareable across concurrent invocations.

## Purpose

The gateway state aggregates all runtime dependencies for the proxy layer. It must be shareable across concurrent invocations and provide stable access to the HTTP client, backend target, observability, scheduling, and flow control.

## Non-goals

The application state is not responsible for request processing logic, backend discovery, or dynamic reconfiguration.

- Does not validate the backend target; validation is deferred to the caller or downstream layers.
- Does not define proxy semantics; that belongs to [[gateway#Reverse Proxy Request Handling]].
- Does not own lifecycle management; the state is assembled externally and injected into the router.
- Does not govern backpressure policy; it only carries backpressure configuration by value.
- Does not negotiate TLS; the default client uses the platform TLS store without customization.

## Interface

The gateway module exposes the application state struct, a router factory, a client factory, and three public sub-modules defining error types, proxy logic, and streaming support.

**State object.** The state object provides read-only access to shared resources and cloned-by-value configuration.

- Provides read access to the HTTP client, backend URL, metrics, scheduler, flow registry, backpressure configuration, an optional per-request timeout, and an optional context-compression state (`Option<Arc<ContextState>>`).
- The context-compression state is `None` when context compression is disabled or failed to initialize, so the proxy serves without compression. See [[context#Context Compression]].
- Is publicly cloneable, so that each clone provides equivalent read access to the same logical resources.
- Has no public constructor; callers construct it incrementally, which permits partial initialization before injection.

**Router factory.** The router factory builds a ready-to-serve router for the three OpenAI-compatible endpoints.

- Takes no arguments and produces a router exposing three OpenAI-compatible endpoints: `POST /v1/chat/completions`, `POST /v1/completions`, and `GET /v1/models`.
- Binds all three endpoints to a single proxy delegation point.
- Returns a router typed for `AppState` shared state; the caller must supply the state after construction before serving.

**Client factory.** The client factory produces a default HTTP client with fixed global configuration.

- Produces an HTTP client with a fixed global timeout and platform-default TLS configuration.
- Accepts no parameters; all configuration is baked into the factory.
- Panics if the client builder fails for any reason; this is an unrecoverable startup error.

**Sub-modules.** The gateway exposes three sub-modules for error types, proxy logic, and streaming.

- The `error` sub-module defines gateway-specific error types used by proxy handlers.
- The `proxy` sub-module contains the unified request proxying logic shared across all routes.
- The `stream` sub-module defines streaming support for SSE-based response forwarding.

## Invariants

The following properties hold for any conformant implementation.

**Shareability.** The state object is designed for concurrent access by multiple handlers.

- The state object is always cloneable so that concurrent handler invocations each carry their own instance.
- Cloning the state is shallow — references to heavyweight resources are not duplicated.

**Timeout semantics.** The per-request timeout applies uniformly to all response modes.

- The per-request timeout is optional; when absent, no timeout is enforced at the proxy layer beyond whatever bound the HTTP client itself enforces.
- The per-request timeout, when present, applies uniformly to both streaming and non-streaming responses.

**Endpoint delegation.** All OpenAI-compatible routes use a single handler to avoid contract divergence.

- All mounted OpenAI-compatible routes delegate to a single handler; divergent routing implies a contract violation.

## Constraints

The design has several hard boundaries that limit flexibility.

- State assembly is caller-controlled; no constructor enforces completeness, allowing inconsistent or partially initialized states to propagate into handlers.
- The HTTP client timeout is fixed at 300 seconds; callers cannot adjust it through the factory.
- The backend URL carries no built-in reachability check; unreachable backends surface as downstream proxy errors rather than startup failures.
- Backpressure configuration is cloned by value with every state clone; it is not an indirection point for live policy updates.
- System TLS is the only TLS configuration; callers cannot supply custom certificates or disable verification through the factory.

## Rationale

The shared-state model centralizes cross-cutting concerns behind a single object so that handlers do not need per-request dependency lookups.

- A mix of shared references and direct values keeps cloning lightweight: heavyweight resources are shared, and lightweight configuration is copied directly.
- No enforced construction order avoids forcing a single initialization site, enabling tests and integration points to construct only the subset of state they exercise.
- Optional per-request timeout permits callers to distinguish between bounded workloads and long-running inference tasks without altering the client-level safety net.
- A single delegation target for all routes simplifies proxy logic and reduces the surface area for handler divergence.

## Related

Concepts and source artifacts associated with gateway application state.

- [[gateway#Reverse Proxy Request Handling]] — the unified handler that all routes delegate to
- [[scheduler#Scheduler Facade and Policy Selection]] — request scheduling decisions referenced by state
- [[metrics#Metrics Registry]] — observability counters accessed through shared ownership
- [[flow#Flow Registry and State]] — flow control registry carried in state
- [[admission#Backpressure and Admission Rejection]] — backpressure configuration held by value in state
- [[src/gateway/mod.rs#AppState]] — exported state struct definition
- [[src/gateway/mod.rs#create_router]] — router factory function
- [[src/gateway/mod.rs#build_client]] — HTTP client factory function

# Reverse Proxy Request Handling

The gateway proxy forwards inbound HTTP requests to a vLLM backend and returns responses with controlled header filtering, enforcing admission, body-size, and timeout boundaries so the backend is never overloaded.

## Purpose

The gateway proxy forwards requests to a vLLM backend with controlled header filtering, admission checks, and timeout boundaries to prevent backend overload. Every proxied interaction is traceable via a unique request identifier.

**Guarantees.**

- Backpressure is enforced through an external scheduler before any work reaches the backend.
- Request body size is bounded to prevent unbounded resource consumption.
- Hop-by-hop headers are stripped from both request and response paths.
- A single request identifier is attached to the full lifecycle of each proxied exchange.
- Active requests are tracked regardless of response mode.

## Non-goals

The gateway proxy does not participate in authentication, authorization, content transformation, or backend discovery.

- The proxy does not inspect, validate, or modify authentication headers; any auth is enforced by the backend.
- The proxy does not load balance, perform health checks, or fail over across backends.
- The proxy does not cache responses or retry failed backend requests.
- The proxy does not validate vLLM API schema semantics; it forwards request bodies verbatim to the backend.
- The proxy does not mutate response bodies; it only strips response headers and injects the request ID.

## Interface

The gateway exposes three HTTP routes that share a uniform proxying contract. Callers depend on deterministic forwarding, bounded admission, and observable responses.

**Routes.**

- `POST /v1/chat/completions` — chat completion requests forwarded to the backend.
- `POST /v1/completions` — generation completion requests forwarded to the backend.
- `GET /v1/models` — model listing forwarded to the backend.

**Admission and Sizing.**

- Requests with a body of 32 MiB or larger are rejected with `413 Payload Too Large` before reaching the scheduler or backend.
- Requests blocked by the scheduler receive `429 Too Many Requests` with a `Retry-After` header (integer seconds), a JSON body `{"error":"queue full"}`, and `Content-Type: application/json`.
- Requests exceeding the configured timeout receive `408 Request Timeout`.

**Streaming Detection.**

- Streaming mode is triggered if the backend response `Content-Type` starts with `text/event-stream`.
- Streaming mode is additionally triggered if the request body contains `"stream": true`.
- Either signal is sufficient to activate the streaming response path.

**Token Accounting.**

- Completed non-streaming responses have their completion token count extracted when available; parsing failures are silent and never interrupt the response stream.
- When `completion_tokens` is absent from the response, `total_tokens` is used as a fallback and may include prompt tokens.
- Streaming response token accounting is handled by [[gateway#Streaming Passthrough and Token Accounting]].

**Scheduling Defaults.**

- Requests without a `max_tokens` field in the body default to a work unit of 1024 tokens for scheduler admission.

**Flow Identity.**

- A flow identity is resolved from request headers, with body content inspected as a fallback ([[flow#Flow Identification]]).

**Error Responses Produced by the Proxy.**

- `413` — request body is 32 MiB or larger.
- `429` — scheduler rejects the request under backpressure; includes `Retry-After` header, JSON body `{"error":"queue full"}`, and `Content-Type: application/json`.
- `408` — request exceeds the configured timeout.
- `502` — network error communicating with the backend.
- `500` — internal proxy error, such as backend URL construction failure.

**Response Contracts.**

- Successful backend responses are returned verbatim with hop-by-hop headers stripped and an `X-Request-ID` header injected.
- Streaming responses omit `Content-Length` so the body can be delivered incrementally.
- Backend error responses (4xx, 5xx) are returned verbatim with filtered headers; the proxy does not alter status codes or error bodies.

**Observability Spans.**

- Each proxied request emits a structured tracing span with the fields: `flow_id`, `request_id`, `method`, `path`, `stream`. Backend forwarding produces a nested span recording `status`, `duration_ms`, and `tokens`.

**Configuration.**

- A backend base URL is required; all request paths are joined against it.
- An optional request timeout bounds the entire forwarding lifecycle, including streaming.
- A scheduler reference is required for admission control; its rejection mode is labeled for metrics.

## Invariants

Structural properties of the gateway proxy hold regardless of implementation details.

**Header Filtering.** Hop-by-hop headers and related headers are always stripped from request and response paths.

- Hop-by-hop headers, any header named in the `Connection` header, and `Host` are always stripped from both request and response header sets.
- Streaming responses additionally omit `Content-Length`.

**Request Identity.** Every proxied response carries a unique, stable request identifier.

- Every proxied response carries an `X-Request-ID` header containing a unique identifier, consistent across the entire request lifecycle.

**Admission Holding.** An admission slot is retained for the full lifetime of a request.

- An admission slot is retained from the moment the scheduler admits the request until the response body is fully delivered to the client or the request is cancelled.

**Active Request Tracking.**

- Active request counts are maintained for all requests, regardless of whether the response is streaming or non-streaming.

**Error Counter Asymmetry.**

- The error counter is incremented for network failures during the send phase but not for network failures during body collection. Timeouts are not counted as errors in either path.

**Token Reporting.**

- Token counts from non-streaming responses are reported to metrics upon completion; extraction failures never break the response stream.

## Constraints

Operational boundaries limit what the proxy can do and how callers must behave.

**Backend Dependency.**

- A single backend URL is configured; the proxy cannot distribute across multiple backends.
- If the backend is unreachable, the proxy returns `502` and does not attempt recovery.

**Size and Time Bounds.**

- Request bodies of 32 MiB or larger are rejected before forwarding.
- The HTTP client enforces a 300-second upper bound independently of any configured request timeout.
- Streaming responses are bounded by a single deadline equal to the configured request timeout. Non-streaming responses apply the timeout duration independently to each forwarding phase, so the effective bound may exceed the configured value.

**Scheduling.**

- Requests without an explicit `max_tokens` field default to 1024 tokens for work-unit calculation.
- The proxy never retries a failed backend request; clients must implement their own retry logic.

**Error Passthrough.**

- Backend error bodies and status codes are returned to callers unmodified; the proxy does not distinguish among backend error types.

## Rationale

The gateway proxy exists to shield the backend from unbounded load while remaining transparent to the API contract callers depend on.

- Admission control is placed before backend contact to prevent resource exhaustion at the backend.
- Hop-by-hop header stripping preserves HTTP correctness; forwarding these headers would cause protocol violations between proxy and backend.
- A universal request identifier enables correlation across logs, metrics, and observability without requiring caller-side tracking.
- Silent metric failures prevent token accounting from breaking user-visible response streams.
- Error passthrough avoids introducing proxy-specific error semantics that would require callers to distinguish proxy-originated from backend-originated errors.
- The 429 response includes a structured JSON body to allow callers to programmatically identify backpressure rejections without parsing HTTP status codes alone.

## Related

Concepts and source artifacts associated with reverse proxy request handling.

- [[scheduler#Scheduler Facade and Policy Selection]] — admission control and backpressure enforcement.
- [[flow#Flow Identification]] — flow ID resolution from headers and body.
- [[gateway#Streaming Passthrough and Token Accounting]] — streaming response token accumulation and request tracking.
- [[gateway#Gateway Application State]] — configuration surface for backend URL, timeout, and scheduler binding.
- [[src/gateway/proxy.rs#proxy_handler]] — main request handler.
- [[src/gateway/proxy.rs#MAX_BODY_SIZE]] — body size limit constant.
- [[src/gateway/proxy.rs#HOP_BY_HOP]] — hop-by-hop and excluded header definitions.
- [[src/gateway/error.rs#ProxyError]] — proxy error types and status code mapping.

# Streaming Passthrough and Token Accounting

Gateway streaming wraps backend HTTP responses as client-facing byte streams, optionally instrumenting token accounting and request lifecycle tracking.

## Purpose

Gateway streaming wraps backend HTTP responses as client-facing byte streams, optionally instrumenting token accounting and lifecycle tracking. In-flight requests are counted and queue slots held throughout the stream.

- Active request count always reflects the number of live streaming responses.
- Backend bytes reach clients without transformation, truncation, or reordering.
- Token generation is accounted from the payload with best-effort parsing semantics.
- Queue admission slots are released only when a stream terminates by any path.
- Stream deadlines, when configured, cause the stream to produce an error when exceeded.

## Non-goals

Gateway streaming does not guarantee anything about payload correctness, JSON validity, or token-count precision; those are backend concerns.

- No buffering, transformation, or payload inspection beyond token key scanning.
- No recovery of lost metrics on process crash or guard abandonment.
- No deduplication of token counts when multiple usage objects appear.
- No HTTP-level error semantics; backend errors surface as opaque I/O errors.
- No reconnection, retry, or request reshaping logic.

## Interface

The gateway stream exposes three construction-time contracts and one stream contract that consumers rely on.

**Active-request guard.** The active-request guard tracks the count of live streaming requests.

- Accepts a shared metrics handle; increments the active-request counter immediately on construction.
- Decrements the counter exactly once when the guard leaves scope; the counter value at any point equals the number of live guards.
- Never returns an error or exposes a failure mode to the caller.

**Passthrough stream.** The passthrough stream forwards backend response bytes without inspection or transformation.

- Accepts a single backend HTTP response and produces a stream of bytes.
- Bytes are forwarded verbatim; chunking granularity is an implementation detail.
- Terminates with an I/O error wrapping the backend error message; the error preserves the backend error text but discards HTTP-specific semantics.
- Emits an error-level log entry whenever a backend error occurs before terminating; the log records the backend error for operational observability.

**Instrumented stream.** The instrumented stream wraps passthrough with token accounting, queue slot management, and optional deadline enforcement.

- Accepts a backend response, shared metrics, a queue admission slot, a lifecycle guard, and an optional deadline; produces a byte stream indistinguishable at the interface from the passthrough variant.
- Increments a token-generation counter for each parseable `completion_tokens` value greater than zero in the payload; parsing failures and non-positive values are silently skipped.
- Reports delivered token counts to the lifecycle guard on each positive token parse and releases the queue slot on any termination path.
- Stream errors when the deadline is exceeded; repeated polls after the deadline elapse produce repeated errors (termination is consumer-driven).

## Invariants

The following statements hold regardless of implementation details. They define what must remain true across any rewrite.

- The active-request counter is incremented exactly once per guard construction and decremented exactly once per guard destruction; intermediate value equals the number of live guards.
- Byte payloads are forwarded verbatim; neither passthrough nor instrumented streams truncate, reorder, transform, or merge bytes.
- Token metric updates are strictly best-effort: parse failures or ambiguous payloads never produce errors or alter stream delivery.
- Queue admission slots are bound from stream construction to stream drop; they are released on normal completion, client disconnect, and timeout error.
- A completion signal is emitted to the lifecycle guard each time the stream yields `None` (exhaustion); it is not emitted on error or timeout paths.

## Constraints

The concept operates within fixed boundaries that limit what it can guarantee.

- Deadline enforcement granularity is bounded by stream polling frequency.
- Multiple `completion_tokens` values in one response are all summed; there is no deduplication mechanism.
- Process-level crashes discard in-flight guard state and unreported token counts; no compensation mechanism exists.
- Backend errors are rewrapped as opaque I/O errors; callers cannot distinguish connection failures, server errors, or protocol violations from the error type alone.
- Token parsing can extract values whose JSON representation spans multiple byte chunks; accumulated parse state persists across error boundaries.

## Rationale

Gateway streaming sits between upstream inference engines and downstream HTTP clients. The design prioritizes low-latency passthrough and safe lifecycle accounting over metric precision or error richness.

- Passthrough fidelity ensures that downstream clients receive exactly what the backend produces, preventing subtle byte-level corruption that would break protocol parsing.
- Scope-bound guard semantics guarantee counter correctness without explicit cleanup paths; the counter always reflects live request count on scope exit.
- Best-effort metrics prevent a malformed token field from stalling or aborting an otherwise valid streaming response.
- Queue slot binding to stream lifetime prevents slot starvation by ensuring slots are freed exactly when the request is no longer consuming capacity.
- Opaque error wrapping simplifies the consumer interface; callers that need HTTP semantics are expected to observe status codes before the response enters the streaming phase.

## Related

Concepts and source artifacts associated with streaming passthrough and token accounting.

- [[metrics#Metrics Registry]] — Metrics counters and gauges consumed by the metrics handle.
- [[admission#Backpressure and Admission Rejection]] — Queue admission system providing slot semantics.
- [[scheduler_policies#Request Lifecycle and Credit Restoration]] — Lifecycle guard receiving completion and token delivery signals.
- [[gateway#Gateway Application State]] — Deadline configuration and timeout semantics.
- [[gateway#Reverse Proxy Request Handling]] — upstream proxy that triggers streaming mode.
- [[src/gateway/stream.rs#RequestActiveGuard]] — Active-request guard implementation.
- [[src/gateway/stream.rs#PassthroughStream]] — Passthrough stream implementation.
- [[src/gateway/stream.rs#MetricStream]] — Instrumented stream implementation.

# Proxy Error Model

Gateway proxy errors classify every failure condition between the gateway and backend, then map each to a single, deterministic HTTP response. The mapping is exhaustive: no condition falls through unhandled.

## Purpose

This concept guarantees that every backend-proxy failure is observable as a well-defined HTTP response. Each failure class has a unique status code and response semantics. Failures are logged and HTTP-client errors are normalized.

## Non-goals

Gateway proxy errors handle only response construction for proxy failures. They do not govern retry strategies, health aggregation, body validation, or diagnostic enrichment.

- **Client-facing retry logic.** The error response provides `Retry-After` for backpressure rejections, but does not define retry backoff strategies or client reconnection policies.
- **Backend health aggregation.** Individual proxy failures are not collected into health dashboards or circuit-breaker signals within this concept; that belongs to external observability systems.
- **Request-body validation.** The too-large condition only triggers when the upstream proxy layer has already detected an oversized body; schema or semantic validation is outside scope.
- **Debug information propagation.** Backend error debug rendering omits both body and headers from diagnostic output; structured diagnostic payloads for downstream tooling are not provided.

## Interface

The gateway exposes a uniform contract: every proxy failure becomes an HTTP response with a status code, body, and optional headers.

**Error taxonomy.**

- **Backend error.** The backend returned an HTTP response indicating failure (4xx or 5xx). The gateway echoes the original status, headers, and body without modification.
- **Network failure.** Communication with the backend failed. Maps to 502 Bad Gateway. Response body is plain text `"Bad Gateway"`.
- **Internal error.** A non-transport, non-backend failure within the proxy path. Maps to 500 Internal Server Error. The diagnostic message string appears both in structured error-level logs and verbatim in the HTTP response body.
- **Payload too large.** The request body exceeded the configured size limit. Maps to 413 Payload Too Large. Response body is plain text `"Request body too large"`.
- **Backpressure rejection.** The backend queue was full and rejected the request. Maps to 429 Too Many Requests with a `Retry-After` header and JSON body `{"error":"queue full"}`.
- **Timeout.** The request exceeded the configured duration. Maps to 408 Request Timeout. Logs at warn level. Response body is plain text `"Request timed out"`.

**Automatic HTTP-client normalization.**

- Every HTTP-client error automatically becomes a Network failure; callers cannot produce HTTP-client errors outside this taxonomy. All client-side error conditions — transport, TLS, DNS, and protocol-level — are covered.

**Response construction guarantees.**

- Every error variant produces exactly one HTTP response; there are no uncovered cases.
- Backpressure rejections always include a `Retry-After` header whose value is a non-negative integer of seconds and `Content-Type: application/json`.
- Network failures log at error level; timeouts log at warn level; Internal errors log at error level; Backend error, TooLarge, and Rejected are not logged on their own.
- Non-Rejected error responses do not set `Content-Type`; clients infer content type from the body bytes.

## Invariants

The error taxonomy maintains a stable, exhaustive mapping between failure conditions and HTTP responses.

**Exhaustive coverage.** Every proxy error variant maps to exactly one HTTP status code.

- Every possible proxy error variant maps to exactly one HTTP status code; no condition lacks a response.
- The mapping from error to response is total and unambiguous — the same error variant always produces the same status code.

**Backend fidelity.** Backend errors pass through the gateway without modification.

- Backend errors are a faithful pass-through: the original status code, headers, and body propagate unchanged through the gateway. Header completeness depends on the caller supplying a complete header map.
- The gateway does not inspect, modify, or augment backend error payloads.

**Header correctness.** Backpressure rejections carry required headers on every invocation.

- Backpressure rejections always carry `Retry-After` as a non-negative integer number of seconds, never fractional.
- Backpressure rejections always carry `Content-Type: application/json`.

**Transport classification.**

- Every HTTP-client error is classified exclusively as a Network failure; it cannot appear as Internal or any other variant.

**Size distinction.**

- A too-large payload is a distinct condition from an internal error, so it always maps to a distinct status code (413 vs 500).

**Debug rendering boundaries.**

- Backend error debug rendering exposes only the status code; both body and headers are omitted to avoid leaking payload data and request metadata in diagnostic logs.
- Network error debug rendering exposes the inner transport error.
- Internal error debug rendering exposes the full message string.
- TooLarge and Timeout debug rendering output only the variant name with no additional data.
- Rejected error debug rendering exposes the retry-after duration.

## Constraints

The error system operates under tight boundaries around response construction and observability.

**Response construction.**

- Backpressure rejection responses carry a `Retry-After` header value that is always a string of ASCII digits; it never contains non-ASCII or control characters.
- Only an exactly-zero retry duration yields a `Retry-After` value of `0`; any positive duration — however small — produces at least `1` second due to ceiling rounding.

**Observability limits.**

- Timeout errors carry no backend identifier or elapsed-duration data; they are observationally opaque.
- Log severity differs across conditions (error vs. warn), so uniform log filtering by severity conflates distinct failure classes.

**Classification boundaries.**

- Automatic conversion from HTTP-client errors covers all client-side error conditions, not only transport-level failures. Other sources of I/O errors require explicit classification into a specific variant.

## Rationale

The error taxonomy separates observable failure modes into distinct status codes so clients and intermediate proxies can route retries, circuit breaks, and user-facing messages without inspecting bodies or headers.

**Why exhaustive mapping.** An exhaustive error-to-response mapping eliminates gaps where an unexpected condition silently drops the response or produces an unhandled panic.

**Why backend fidelity.** The gateway is transparent for backend-originated errors; modifying status, headers, or body would obscure the backend's intent and break client-side error handlers.

**Why `Retry-After` for backpressure.** A structured retry signal lets clients throttle themselves during congestion rather than hammering the gateway. Integer seconds comply with RFC 7231 and keep header parsing simple.

**Why HTTP-client error normalization.** Automatic normalization of HTTP-client errors into a single Network variant prevents callers from mixing raw HTTP-client errors with the gateway's error taxonomy, which would duplicate error-handling logic across modules.

**Why Internal message dual exposure.** The Internal error message appears in both logs and the HTTP response body to provide consistent diagnostic context across structured logging and client-facing responses. Whether this leaks internal details to the client is a deliberate trade-off.

## Related

Concepts and source artifacts associated with the proxy error model.

- [[gateway#Reverse Proxy Request Handling]] — the proxy layer that produces these error conditions when forwarding requests to backends.
- [[admission#Backpressure and Admission Rejection]] — the backpressure mechanism whose queue-full signal becomes a Rejection error.
- [[gateway#Gateway Application State]] — timeout configuration whose expiration produces a Timeout error.
- [[gateway#Reverse Proxy Request Handling]] — upstream proxy handler that produces error conditions.
- [[gateway#Gateway Application State]] — application state that configures error-related settings.
- [[src/gateway/error.rs#ProxyError]] — error type and response construction.
