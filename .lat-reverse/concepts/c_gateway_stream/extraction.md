# c_gateway_stream — Extraction

## Responsibilities

- **Active-request counting** — RAII guard increments `requests_active` on creation and decrements on drop, ensuring the counter reflects live request count regardless of stream lifetime or early handler return (`src/gateway/stream.rs:16-31`).
- **Streaming passthrough** — Wraps a `reqwest` response bytes stream and yields each chunk immediately without buffering, producing `Result<Bytes, std::io::Error>` items suitable for `axum::body::Body::from_stream` (`src/gateway/stream.rs:36-63`).
- **Token accounting from stream payload** — Scans streaming SSE chunks for `"completion_tokens"` JSON keys, extracts the integer value, and increments a Prometheus counter; parsing failures are silently ignored (`src/gateway/stream.rs:67-121`, `src/gateway/stream.rs:185-191`).
- **Queue slot and lifecycle binding** — Holds a `QueueTicket` for the stream lifetime (released on stream end or client disconnect) and reports delivered token counts to a `LifecycleGuard` on completion or credit restoration on cancellation (`src/gateway/stream.rs:130-136`, `src/gateway/stream.rs:196-198`).
- **Deadline enforcement** — Optional timeout checked before each poll; when the deadline passes, the stream returns an `io::Error` and the lifecycle guard drops in the cancelled state (`src/gateway/stream.rs:146`, `src/gateway/stream.rs:175-181`).

## Interface Surfaces

### `RequestActiveGuard`

- **Constructor** — `new(metrics: Arc<Metrics>) -> Self` — accepts a shared metrics handle; side-effect increments `requests_active` immediately. No return error (`src/gateway/stream.rs:21-25`).
- **Drop contract** — `drop()` decrements `requests_active` exactly once. No failure mode exposed to caller; decrement always occurs on scope exit (`src/gateway/stream.rs:27-31`).

### `PassthroughStream`

- **Constructor** — `new(response: reqwest::Response) -> Self` — accepts an HTTP response owning a bytes stream; no error return (`src/gateway/stream.rs:44-48`).
- **`Stream` trait** — `Item = Result<Bytes, std::io::Error>` — yields backend bytes as they arrive; maps `reqwest::Error` to `std::io::Error::other` with error logged via `tracing::error` (`src/gateway/stream.rs:51-63`).
- **Passthrough invariant** — chunks are forwarded without buffering or transformation; each `Ok(Bytes)` from the backend becomes one `Ok(Bytes)` for the consumer (`src/gateway/stream.rs:56`).
- **Error propagation** — any backend error terminates the stream with `Err(io::Error)` containing the `reqwest` error message (`src/gateway/stream.rs:57-60`).

### `MetricStream`

- **Constructor** — `new(response, metrics, queue_ticket, lifecycle_guard, deadline: Option<Instant>) -> Self` — accepts response, shared metrics, an admission slot, a lifecycle tracker, and an optional timeout; no error return (`src/gateway/stream.rs:150-167`).
- **`Stream` trait** — `Item = Result<Bytes, std::io::Error>` — identical item type to `PassthroughStream`; consumers of either are interchangeable at the stream interface (`src/gateway/stream.rs:169-201`).
- **Token counter update** — Prometheus `tokens_generated_total` incremented by parsed `completion_tokens` values per chunk; silent on parse failure (`src/gateway/stream.rs:125-128`, `src/gateway/stream.rs:186-191`).
- **Lifecycle reporting** — `lifecycle_guard.add_delivered_tokens()` and `record_token()` called for each chunk yielding positive tokens; `mark_completed()` called once on normal stream end; cancellation path relies on RAII drop (`src/gateway/stream.rs:134-136`, `src/gateway/stream.rs:188-191`, `src/gateway/stream.rs:196-198`).
- **Timeout error** — when `deadline.is_some()` and `now >= deadline`, stream emits `Err(io::Error::other("request timeout while streaming"))` on that poll; subsequent polls are not reached because the stream terminates (`src/gateway/stream.rs:175-181`).

### `TokenAccumulator` (internal — no public surface)

- Used only within `MetricStream`. Parses `completion_tokens` from accumulated JSON bytes via substring search and numeric extraction. Does not expose a public API (`src/gateway/stream.rs:67-121`).

## Invariants

- **Active counter balance** — `requests_active` is incremented exactly once at guard construction and decremented exactly once at guard destruction; the counter value at any time equals the number of live guards (`src/gateway/stream.rs:22-23`, `src/gateway/stream.rs:28-30`).
- **Chunk fidelity** — `PassthroughStream` and `MetricStream` yield the same `Bytes` values produced by the backend; neither truncates, reorders, or transforms chunk payloads (`src/gateway/stream.rs:56`, `src/gateway/stream.rs:192`).
- **Metrics are best-effort** — token parsing failures silently skip the count; the stream never emits an error due to malformed or unparseable JSON in the payload (`src/gateway/stream.rs:127-128`).
- **Queue slot duration** — the `QueueTicket` is held from stream construction until stream drop; it is released on normal completion, client disconnect, or timeout error (`src/gateway/stream.rs:130-132`).
- **Lifecycle guard completion signal** — `mark_completed()` is called exactly once when the backend stream yields `None` (normal termination); it is not called on error or timeout paths (`src/gateway/stream.rs:196-198`).
- **Deadline check before poll** — the deadline is evaluated before each backend poll; if elapsed, the timeout error is returned without consuming a backend chunk (`src/gateway/stream.rs:175-181`).

## Failure Modes

- **Backend connection failure** — `reqwest::Error` from the inner stream is logged and wrapped as `std::io::Error::other`; the consumer sees the error string but loses HTTP-specific error semantics (`src/gateway/stream.rs:57-60`, `src/gateway/stream.rs:194`).
- **Token parsing mismatch** — if `completion_tokens` appears in non-JSON context or with unexpected formatting, the substring scan may miss or misread the value; no error is raised (`src/gateway/stream.rs:82-120`).
- **Incomplete JSON at chunk boundary** — the accumulator carries state across chunks; if `"completion_tokens"` spans two chunks, the scan may miss it until the full key appears in the buffer window (`src/gateway/stream.rs:85-88`).
- **Timeout races with active poll** — if the deadline expires while `poll_next` is blocked waiting for the backend, the error is detected only on the next poll invocation; the guard is not dropped until the poll returns (`src/gateway/stream.rs:175-181`).
- **Process crash** — if the process exits before a guard is dropped, `requests_active` is not decremented and `LifecycleGuard` accounting is lost; no compensation mechanism exists (`src/gateway/stream.rs:27-31`, `src/gateway/stream.rs:134-136`).
- **Double-counting risk** — if multiple `usage` JSON objects appear in a single response with `completion_tokens`, each is summed into the counter; there is no deduplication guard (`src/gateway/stream.rs:109`).
