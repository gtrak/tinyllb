# Gateway Proxy — Extraction

## Responsibilities

- Proxies inbound HTTP requests to a vLLM backend, returning backend responses verbatim (with header filtering).
- Enforces a request body size limit; rejects oversized requests before forwarding.
- Admits requests through a scheduler; backpressure rejects carry a `Retry-After` directive.
- Detects streaming vs. non-streaming responses and dispatches to the appropriate response path (SSE passthrough vs. body collection).
- Tracks per-request lifecycle (admit → forward → complete or cancel) and reports token accounting to the scheduler on completion.

## Interface Surfaces

### HTTP Routes

All three routes share the same handler (`proxy_handler`). No authentication is required beyond what the backend itself enforces; the proxy does not inspect or add auth headers.

| Method | Path |
|---|---|
| `POST` | `/v1/chat/completions` |
| `POST` | `/v1/completions` |
| `GET` | `/v1/models` |

**Evidence:** `src/gateway/mod.rs:38-43` — `Router::new()` mounts three routes, all bound to `proxy_handler`.

### Request Semantics

- **Body size limit**: Requests with a body larger than 32 MiB are rejected with `413 Payload Too Large`. The limit is enforced both via `Content-Length` header inspection and via a streaming body cap (`http_body_util::Limited`).

  **Evidence:** `src/gateway/proxy.rs:21` — `MAX_BODY_SIZE = 32 * 1024 * 1024`. `src/gateway/proxy.rs:186-194` — Content-Length guard. `src/gateway/proxy.rs:198-215` — Limited body collection.

- **Request routing**: The request path and query string are forwarded to the backend by joining them against the configured backend URL.

  **Evidence:** `src/gateway/proxy.rs:110-124` — `build_backend_url` joins path + query. `src/gateway/proxy.rs:229` — URL built from `original_path` and `query`.

- **Header forwarding**: Inbound request headers are forwarded after stripping hop-by-hop headers (RFC 7230 §6.1), headers named in the `Connection` header, and `Host`.

  **Evidence:** `src/gateway/proxy.rs:24-36` — `HOP_BY_HOP` and `EXCLUDE_HEADERS` constants. `src/gateway/proxy.rs:39-62` — `filter_headers` implementation including Connection header expansion.

### Response Semantics

- **Success (2xx)**: Backend body forwarded verbatim. Headers are filtered (hop-by-hop stripped). An `X-Request-ID` header is injected with a UUID generated at the start of the request.

  **Evidence:** `src/gateway/proxy.rs:345-353` (error-status path), `src/gateway/proxy.rs:427-439` (non-streaming success path), `src/gateway/proxy.rs:372-384` (streaming path).

- **Streaming path**: If the backend `Content-Type` starts with `text/event-stream`, or if the request body JSON contains `"stream": true`, the response is streamed via `MetricStream`. `Content-Length` is removed from the response; axum uses chunked transfer encoding.

  **Evidence:** `src/gateway/proxy.rs:332` — `is_sse` check. `src/gateway/proxy.rs:79-91` — `body_wants_streaming`. `src/gateway/proxy.rs:361-384` — streaming dispatch.

- **Error passthrough (4xx/5xx from backend)**: Backend error bodies are returned verbatim with filtered headers. The proxy does not modify the error status code or body.

  **Evidence:** `src/gateway/proxy.rs:336-353` — error-status handling.

### Status Codes Produced by the Proxy Itself

| Code | Condition | Evidence |
|---|---|---|
| `400` | (None produced; client errors are passthrough from backend) | — |
| `408` | Request exceeds configured timeout | `src/gateway/error.rs:87` |
| `413` | Request body exceeds 32 MiB | `src/gateway/error.rs:65` |
| `429` | Scheduler rejects request (backpressure); includes `Retry-After` header (integer seconds) and JSON body `{"error":"queue full"}` | `src/gateway/error.rs:67-83` |
| `502` | Network error to backend (connection refused, DNS failure, etc.) | `src/gateway/error.rs:57` |
| `500` | Internal proxy error (URL construction failure, etc.) | `src/gateway/error.rs:61` |

### Configuration Contract

The proxy requires these configuration values, provided via `AppState`:

- `backend_url` (URL) — base URL of the vLLM backend.
- `request_timeout` (optional Duration) — if set, cancels any forwarded request exceeding this duration, including streaming.
- `scheduler` — admission control; requests block until admitted or rejected.
- `backpressure.mode` — label for metrics; drives the metric label on rejections.

**Evidence:** `src/gateway/mod.rs:17-28` — `AppState` fields.

## Invariants

- **Hop-by-hop headers are never forwarded to the backend.** The proxy strips all RFC 7230 §6.1 hop-by-hop headers plus `Host` from both request and response header sets. Connection-header-named headers are additionally stripped.

  **Evidence:** `src/gateway/proxy.rs:39-62` — `filter_headers`. `src/gateway/proxy.rs:65-75` — response filtering reuses same logic; streaming path additionally strips `Content-Length`.

- **Every proxied response includes an `X-Request-ID` header** containing a v4 UUID generated at request start. This ID is consistent for the entire request lifecycle (logging, streaming, non-streaming).

  **Evidence:** `src/gateway/proxy.rs:175` — UUID generation. `src/gateway/proxy.rs:348-352`, `379-382`, `435-438` — injected in all three response paths.

- **Admission slots are held until the request fully completes or is cancelled.** For streaming, the slot is held by `MetricStream` (via `_queue_ticket` field) until the stream ends or client disconnects. For non-streaming, the slot is held until the body is fully collected.

  **Evidence:** `src/gateway/proxy.rs:236-249` — ticket acquisition. `src/gateway/proxy.rs:364` — ticket passed into `MetricStream`. `src/gateway/stream.rs:130-146` — `MetricStream` owns `_queue_ticket`.

- **Requests exceeding the configured timeout are cancelled and the lifecycle guard drops as cancelled.** This applies to connect phase, send phase, body collection, and streaming.

  **Evidence:** `src/gateway/proxy.rs:286-306` — send timeout. `src/gateway/proxy.rs:392-408` — body collection timeout. `src/gateway/stream.rs:175-181` — stream deadline check.

- **Token counts are extracted from both streaming and non-streaming paths** via best-effort JSON parsing. Failures to parse are silent (stream never breaks due to metrics collection).

  **Evidence:** `src/gateway/proxy.rs:411-418` — non-streaming token extraction. `src/gateway/stream.rs:67-121` — `TokenAccumulator` for streaming. `src/gateway/stream.rs:184-191` — streaming token reporting.

## Failure Modes

- **Body size exceeded**: Requests larger than 32 MiB are rejected with `413`. This occurs both at the Content-Length check and the streaming body cap.

  **Evidence:** `src/gateway/proxy.rs:186-215`.

- **Scheduler rejection**: If the scheduler cannot admit the request, a `429` response is returned with `Retry-After` header. The proxy does not retry; the client must retry.

  **Evidence:** `src/gateway/proxy.rs:236-248` — `admit` failure path. `src/gateway/error.rs:67-83` — response construction.

- **Backend unreachable**: Network errors (connection refused, DNS failure, TLS errors) produce `502 Bad Gateway`.

  **Evidence:** `src/gateway/proxy.rs:291`, `299-301`. `src/gateway/error.rs:57`.

- **Request timeout**: If the configured timeout is exceeded at any phase (connect, send, body collection, or streaming), the request is aborted and the client receives `408 Request Timeout`.

  **Evidence:** `src/gateway/proxy.rs:296`, `402`. `src/gateway/error.rs:86-88`.

- **URL construction failure**: If the backend URL + request path cannot be joined into a valid URL, a `500 Internal Server Error` is returned.

  **Evidence:** `src/gateway/proxy.rs:116-118` — `ProxyError::Internal` on join failure. `src/gateway/error.rs:61`.

- **Backend error status**: Backend 4xx/5xx responses are returned to the client verbatim. The proxy does not distinguish or modify them beyond header filtering.

  **Evidence:** `src/gateway/proxy.rs:336-353`.

## Related

- `[[?c_flow_identity]]` — flow ID resolution via `identify::resolve`.
- `[[?c_scheduler]]` — admission control via `scheduler.admit`.
- `[[?c_metrics]]` — Prometheus counters for requests active, tokens generated, backpressure rejections, errors.
