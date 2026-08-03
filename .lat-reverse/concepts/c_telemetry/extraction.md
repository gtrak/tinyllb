# Telemetry

## Responsibilities

- Initializes the global tracing subscriber for structured logging and span output.
- Selects between JSON and human-readable output formats based on environment configuration.

## Interface

### `init()` — Tracing subscriber initialization

- **Preconditions:** Must be called exactly once before any `tracing` spans are emitted.
- **Inputs:** Environment variables `RUST_LOG` and `LLM_QDISC_LOG_JSON` (no function parameters).
- **Postconditions:** Global tracing subscriber is active; subsequent `tracing` events are formatted and filtered according to the configured mode and filter directive.
- **Failure modes:** Calling after prior subscriber initialization causes a runtime panic (enforced by `tracing_subscriber`).

### Configuration contract

| Key | Default | Effect |
|---|---|---|
| `RUST_LOG` | `"info,llm_qdisc_proxy=debug"` | `tracing` filter directive controlling span/event verbosity. |
| `LLM_QDISC_LOG_JSON` | unset (false) | `"1"` selects JSON output; any other value or absence selects human-readable. |

## Invariants

- `RUST_LOG` is never left undefined by `init()`: missing env var resolves to `"info,llm_qdisc_proxy=debug"` (line 29).
- `LLM_QDISC_LOG_JSON` is truthy only when the value is exactly `"1"` (line 24).
- Output mode is binary: JSON with flattened events, or human-readable; no intermediate format exists (lines 32–39).
- The subscriber is registered as a global singleton; `init()` must not be called twice (lines 36, 39 call `.init()` on the subscriber builder).

## Failure Modes

- Calling `init()` twice panics at runtime due to `tracing_subscriber`'s single-init enforcement.
- Malformed `RUST_LOG` directive values may cause `tracing` to emit warnings or apply a fallback filter at runtime.
- `init_otlp()` (line 81) is a private, dead-code stub that delegates to `init()` (line 84); it has no active behavior.

## Related

- [[src/telemetry/mod.rs#init]]
- [[src/telemetry/mod.rs#init_otlp]]
