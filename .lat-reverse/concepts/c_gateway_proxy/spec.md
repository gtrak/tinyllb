# Gateway Proxy

## Purpose

The gateway proxy forwards inbound HTTP requests to a vLLM backend and returns backend responses to callers with controlled header filtering. It enforces admission, body-size, and timeout boundaries so that the backend is never overloaded. Every proxied interaction is traceable via a unique request identifier and structured observability spans.

### Guarantees

- Backpressure is enforced through an external scheduler before any work reaches the backend.
- Request body size is bounded to prevent unbounded resource consumption.
- Hop-by-hop headers are stripped from both request and response paths.
- A single request identifier is attached to the full lifecycle of each proxied exchange.
- Active requests are tracked regardless of response mode.

## Non-goals

The gateway proxy does not participate in authentication, authorization, content transformation, or backend discovery.

### Out of scope

- The proxy does not inspect, validate, or modify authentication headers; any auth is enforced by the backend.
- The proxy does not load balance, perform health checks, or fail over across backends.
- The proxy does not cache responses or retry failed backend requests.
- The proxy does not validate vLLM API schema semantics; it forwards request bodies verbatim to the backend.
- The proxy does not mutate response bodies; it only strips response headers and injects the request ID.

## Interface

The gateway exposes three HTTP routes that share a uniform proxying contract. Callers depend on deterministic forwarding, bounded admission, and observable responses.

### Routes

- `POST /v1/chat/completions` — chat completion requests forwarded to the backend.
- `POST /v1/completions` — generation completion requests forwarded to the backend.
- `GET /v1/models` — model listing forwarded to the backend.

### Admission and Sizing

- Requests with a body of 32 MiB or larger are rejected with `413 Payload Too Large` before reaching the scheduler or backend.
- Requests blocked by the scheduler receive `429 Too Many Requests` with a `Retry-After` header (integer seconds), a JSON body `{"error":"queue full"}`, and `Content-Type: application/json`.
- Requests exceeding the configured timeout receive `408 Request Timeout`.

### Streaming Detection

- Streaming mode is triggered if the backend response `Content-Type` starts with `text/event-stream`.
- Streaming mode is additionally triggered if the request body contains `"stream": true`.
- Either signal is sufficient to activate the streaming response path.

### Token Accounting

- Completed non-streaming responses have their completion token count extracted when available; parsing failures are silent and never interrupt the response stream.
- When `completion_tokens` is absent from the response, `total_tokens` is used as a fallback and may include prompt tokens.
- Streaming response token accounting is handled by the streaming response path ([[?c_stream_metrics]]).

### Scheduling Defaults

- Requests without a `max_tokens` field in the body default to a work unit of 1024 tokens for scheduler admission.

### Flow Identity

- A flow identity is resolved from request headers, with body content inspected as a fallback ([[?c_flow_identity]]).

### Error Responses Produced by the Proxy

- `413` — request body is 32 MiB or larger.
- `429` — scheduler rejects the request under backpressure; includes `Retry-After` header, JSON body `{"error":"queue full"}`, and `Content-Type: application/json`.
- `408` — request exceeds the configured timeout.
- `502` — network error communicating with the backend.
- `500` — internal proxy error, such as backend URL construction failure.

### Response Contracts

- Successful backend responses are returned verbatim with hop-by-hop headers stripped and an `X-Request-ID` header injected.
- Streaming responses omit `Content-Length` so the body can be delivered incrementally.
- Backend error responses (4xx, 5xx) are returned verbatim with filtered headers; the proxy does not alter status codes or error bodies.

### Observability Spans

- Each proxied request emits a structured tracing span with the fields: `flow_id`, `request_id`, `method`, `path`, `stream`. Backend forwarding produces a nested span recording `status`, `duration_ms`, and `tokens`.

### Configuration

- A backend base URL is required; all request paths are joined against it.
- An optional request timeout bounds the entire forwarding lifecycle, including streaming.
- A scheduler reference is required for admission control; its rejection mode is labeled for metrics.

## Invariants

Structural properties of the gateway proxy hold regardless of implementation details.

### Header Filtering

- Hop-by-hop headers, any header named in the `Connection` header, and `Host` are always stripped from both request and response header sets.
- Streaming responses additionally omit `Content-Length`.

### Request Identity

- Every proxied response carries an `X-Request-ID` header containing a unique identifier, consistent across the entire request lifecycle.

### Admission Holding

- An admission slot is retained from the moment the scheduler admits the request until the response body is fully delivered to the client or the request is cancelled.

### Active Request Tracking

- Active request counts are maintained for all requests, regardless of whether the response is streaming or non-streaming.

### Error Counter Asymmetry

- The error counter is incremented for network failures during the send phase but not for network failures during body collection. Timeouts are not counted as errors in either path.

### Token Reporting

- Token counts from non-streaming responses are reported to metrics upon completion; extraction failures never break the response stream.

## Constraints

Operational boundaries limit what the proxy can do and how callers must behave.

### Backend Dependency

- A single backend URL is configured; the proxy cannot distribute across multiple backends.
- If the backend is unreachable, the proxy returns `502` and does not attempt recovery.

### Size and Time Bounds

- Request bodies of 32 MiB or larger are rejected before forwarding.
- The HTTP client enforces a 300-second upper bound independently of any configured request timeout.
- Streaming responses are bounded by a single deadline equal to the configured request timeout. Non-streaming responses apply the timeout duration independently to each forwarding phase, so the effective bound may exceed the configured value.

### Scheduling

- Requests without an explicit `max_tokens` field default to 1024 tokens for work-unit calculation.
- The proxy never retries a failed backend request; clients must implement their own retry logic.

### Error Passthrough

- Backend error bodies and status codes are returned to callers unmodified; the proxy does not distinguish among backend error types.

## Rationale

The gateway proxy exists to shield the backend from unbounded load while remaining transparent to the API contract callers depend on.

### Design Decisions

- Admission control is placed before backend contact to prevent resource exhaustion at the backend.
- Hop-by-hop header stripping preserves HTTP correctness; forwarding these headers would cause protocol violations between proxy and backend.
- A universal request identifier enables correlation across logs, metrics, and observability without requiring caller-side tracking.
- Silent metric failures prevent token accounting from breaking user-visible response streams.
- Error passthrough avoids introducing proxy-specific error semantics that would require callers to distinguish proxy-originated from backend-originated errors.
- The 429 response includes a structured JSON body to allow callers to programmatically identify backpressure rejections without parsing HTTP status codes alone.

## Related

- [[?c_scheduler]] — admission control and backpressure enforcement.
- [[?c_flow_identity]] — flow ID resolution from headers and body.
- [[?c_stream_metrics]] — streaming response token accumulation and request tracking.
- [[src/gateway/mod.rs#AppState]] — configuration surface for backend URL, timeout, and scheduler binding.
- [[src/gateway/mod.rs#build_client]] — HTTP client construction with default timeout.
- [[src/gateway/proxy.rs#proxy_handler]] — main request handler.
- [[src/gateway/proxy.rs#MAX_BODY_SIZE]] — body size limit constant.
- [[src/gateway/proxy.rs#HOP_BY_HOP]] — hop-by-hop and excluded header definitions.
- [[src/gateway/error.rs#ProxyError]] — proxy error types and status code mapping.
