# Tracing Conventions

## Initialization

The global tracing subscriber is initialized via `tinyllb_proxy::telemetry::init()`
at the start of `main()`. It is configured by two environment variables:

| Env Var | Default | Description |
| --- | --- | --- |
| `RUST_LOG` | `info,tinyllb_proxy=debug` | Standard tracing filter directive. |
| `TINYLLB_LOG_JSON` | unset | Set to `1` for JSON output (one JSON object per line). |

When `TINYLLB_LOG_JSON=1`, the subscriber emits JSON-formatted log lines
suitable for ingestion by Loki, Datadog, or similar aggregators. The JSON
formatter uses `.flatten_event(true)` so span fields are flattened into each
event object rather than nested under a separate key.

### OpenTelemetry Export (Scaffolded)

OpenTelemetry OTLP export is **not active** but scaffolded in
`src/telemetry/mod.rs` (`init_otlp()`). All tracing calls use the `tracing`
crate API, so wiring an OTLP exporter later requires only dependency additions
and uncommenting the stub — no call-site rewrites.

See the `init_otlp()` documentation for instructions on enabling OTLP.

## Spans

The proxy uses structured `tracing` **spans** (not flat events) for
request-level context. Spans are Send-safe — they use `#[tracing::instrument]`
or `info_span!` / `Future::instrument()`. Only `.entered()` guards are
non-Send and must never cross `.await` points.

### Span Nesting

```
request  (proxy_handler)
├── admit       (Scheduler::admit — all algorithms)
│   └── "admit decision" event (terminal accept/reject)
└── backend_forward  (send to backend)
```

### `request` span

Created by `#[tracing::instrument]` on the `proxy_handler` in
`src/gateway/proxy.rs`. `flow_id` and `stream` are late-bound fields recorded
via `Span::current().record()` after resolution inside the handler body.

| Field | Type | Description |
| --- | --- | --- |
| `flow_id` | String | Resolved flow identifier — `X-LLM-Flow-ID`, harness session headers
|           |        | (`x-claude-code-session-id`, `x-session-id`, `x-session-affinity`,
|           |        | `x-client-request-id`, `session_id`), `metadata.flow_id`, or ephemeral UUID. Late-bound. |
| `request_id` | String | UUID v4 generated per-request; echoed in `X-Request-ID` header. |
| `method` | String | HTTP method (e.g. `POST`). |
| `path` | String | Request path (e.g. `/v1/chat/completions`). |
| `stream` | bool | Whether the request requested streaming (`"stream": true`). Late-bound. |

### `admit` span

Created by `#[tracing::instrument]` on the shared `Scheduler::admit` in
`src/scheduler/mod.rs`. This single instrument covers **all algorithms**
(FIFO, WFQ, DRR) — no per-algorithm instrumentation needed. A terminal
`"admit decision"` event is emitted inside `admit()` with the final outcome.

| Span field | Type | Description |
| --- | --- | --- |
| `flow_id` | String | Flow identifier being admitted. |
| `queue_depth_before` | u32 | Total queue depth at admit entry (recorded via `Span::current().record()`). |
| `algorithm` | String | `fifo`, `wfq`, or `drr`. |

| Terminal event field | Description |
| --- | --- |
| `decision` | `accept` or `reject`. |
| `wait_seconds` | Wall-clock seconds from admit entry to permit grant (or rejection). |

### `backend_forward` span

Created via `info_span!` in `proxy.rs`, wrapping the backend send operation.
`status` and `duration_ms` are recorded after the response is received.
`tokens` is recorded in the non-streaming path after body collection.

| Field | Type | Description |
| --- | --- | --- |
| `flow_id` | String | Flow identifier (inherited from request context). |
| `request_id` | String | Request ID (inherited from request context). |
| `status` | u16 | Backend HTTP status code (recorded after response). |
| `duration_ms` | u128 | Round-trip duration in milliseconds (recorded after response). |
| `tokens` | i64 | Completion tokens from `usage.completion_tokens` (non-streaming only; absent for streaming). |

## X-Request-ID

Every proxied request generates a UUID v4 `request_id` that is:
1. Recorded in the `request` span.
2. Echoed back in the `X-Request-ID` response header on success paths
   (backend error passthrough, streaming, and non-streaming responses).
3. Not echoed on backpressure-reject responses (429) since those terminate
   before reaching the backend.

## No PII

The tracing layer is configured to log **only structural fields**:
- Flow IDs, request IDs, HTTP method/path, status codes, durations, counts.
- **Never**: prompt bodies, messages, token content, user data, or secrets.

If you discover a log line that leaks prompt content, it is a bug — file an
issue. Existing `tracing::warn!` calls in `lifecycle.rs` only reference
`flow_id`, `delivered_tokens`, `estimated_cost`, and `overrun` — all structural.

## Adding New Spans or Events

When adding tracing in later issues, follow these conventions:

1. Use `tracing::info!` (not `debug!`) for events that aid incident diagnosis.
2. Name the event string after the operation (e.g. `"admit decision"`,
   `"backend_forward"`).
3. All structural fields as named key=value pairs.
4. No `.entered()` span guards across `.await` points — use spans with
   `#[tracing::instrument]`, `info_span!` + `Future::instrument()`, or
   `Span::current().record()` for late-bound fields.
5. `#[tracing::instrument]` works correctly on axum 0.8 handlers — spans
   are Send-safe and satisfy the `Handler` trait.
