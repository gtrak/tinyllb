# c_gateway_stream — Spec

## Purpose

Gateway streaming wraps backend HTTP responses as client-facing byte streams, optionally instrumenting token accounting and request lifecycle tracking. The concept ensures that every in-flight streaming request is counted, that queue admission slots are held for the full duration of a stream, and that token generation metrics are extracted from payload content without interrupting the byte-passing contract.

- Active request count always reflects the number of live streaming responses.
- Backend bytes reach clients without transformation, truncation, or reordering.
- Token generation is accounted from the payload with best-effort parsing semantics.
- Queue admission slots are released only when a stream terminates by any path.
- Stream deadlines, when configured, cause the stream to produce an error when exceeded.

## Non-goals

Gateway streaming does not guarantee anything about payload correctness, JSON validity, or token-count precision; those are backend concerns. It is not a general-purpose middleware layer and does not expose configuration knobs beyond its construction-time inputs.

- No buffering, transformation, or payload inspection beyond token key scanning.
- No recovery of lost metrics on process crash or guard abandonment.
- No deduplication of token counts when multiple usage objects appear.
- No HTTP-level error semantics; backend errors surface as opaque I/O errors.
- No reconnection, retry, or request reshaping logic.

## Interface

The gateway stream exposes three construction-time contracts and one stream contract that consumers rely on. Each surface defines preconditions on inputs, postconditions on outputs, and the error semantics callers observe.

### Active-request guard

- Accepts a shared metrics handle; increments the active-request counter immediately on construction.
- Decrements the counter exactly once when the guard leaves scope; the counter value at any point equals the number of live guards.
- Never returns an error or exposes a failure mode to the caller.

### Passthrough stream

- Accepts a single backend HTTP response and produces a stream of bytes.
- Bytes are forwarded verbatim; chunking granularity is an implementation detail.
- Terminates with an I/O error wrapping the backend error message; the error preserves the backend error text but discards HTTP-specific semantics.
- Emits an error-level log entry whenever a backend error occurs before terminating; the log records the backend error for operational observability.

### Instrumented stream

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

The concept operates within fixed boundaries that limit what it can guarantee. These constraints are structural, not implementation details.

- Deadline enforcement granularity is bounded by stream polling frequency.
- Multiple `completion_tokens` values in one response are all summed; there is no deduplication mechanism.
- Process-level crashes discard in-flight guard state and unreported token counts; no compensation mechanism exists.
- Backend errors are rewrapped as opaque I/O errors; callers cannot distinguish connection failures, server errors, or protocol violations from the error type alone.
- Token parsing can extract values whose JSON representation spans multiple byte chunks; accumulated parse state persists across error boundaries.

## Rationale

Gateway streaming sits between upstream inference engines and downstream HTTP clients. The design prioritizes low-latency passthrough and safe lifecycle accounting over metric precision or error richness. Best-effort token counting reflects the trade-off between observing generation volume and avoiding payload parsing that would stall or corrupt byte delivery. Deadline enforcement via polling avoids background timers and cancellation channels at the cost of sub-poll-granularity deadlines.

- Passthrough fidelity ensures that downstream clients receive exactly what the backend produces, preventing subtle byte-level corruption that would break protocol parsing.
- Scope-bound guard semantics guarantee counter correctness without explicit cleanup paths; the counter always reflects live request count on scope exit.
- Best-effort metrics prevent a malformed token field from stalling or aborting an otherwise valid streaming response.
- Queue slot binding to stream lifetime prevents slot starvation by ensuring slots are freed exactly when the request is no longer consuming capacity.
- Opaque error wrapping simplifies the consumer interface; callers that need HTTP semantics are expected to observe status codes before the response enters the streaming phase.

## Related

- [[?c_prometheus_metrics]] — Metrics counters and gauges consumed by the metrics handle.
- [[?c_queue_admission]] — Queue admission system providing slot semantics.
- [[?c_request_lifecycle]] — Lifecycle guard receiving completion and token delivery signals.
- [[?c_stream_timeout]] — Deadline configuration and timeout semantics.
- [[src/gateway/stream.rs#RequestActiveGuard]] — Active-request guard implementation.
- [[src/gateway/stream.rs#PassthroughStream]] — Passthrough stream implementation.
- [[src/gateway/stream.rs#MetricStream]] — Instrumented stream implementation.
