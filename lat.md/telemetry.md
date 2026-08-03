# Telemetry Initialization

Telemetry initializes the global tracing subscriber so that all structured log events and span data produced by the proxy are collected, filtered, and emitted in a consistent format.

## Purpose

Provides a single bootstrap surface that the application calls once at startup before any diagnostic output is generated.

- Registers a global tracing subscriber that captures all structured events and spans emitted by the proxy.
- Selects output format (structured JSON or human-readable) based on deployment configuration.
- Applies verbosity filtering via a standard log-level directive, with a sensible default for proxy-level debugging.
- Establishes the observability foundation that all other components depend on for diagnostic output.

## Non-goals

Telemetry is a bootstrap configuration surface, not a data pipeline or a runtime observability framework.

- Does not define span naming conventions, event payload shapes, or instrumentation guidelines — those belong to individual components.
- Does not export traces to external backends such as OpenTelemetry collectors; OTLP export is a planned but unimplemented extension.
- Does not support dynamic reconfiguration of log levels or output format after startup.
- Does not aggregate, route, or transform log data beyond format selection.
- Does not expose a metrics collection surface; metrics instrumentation is a separate concern.

## Interface

The telemetry concept exposes a single public initialization surface and a configuration surface read entirely from environment variables. All output is written to stderr.

- **Subscriber initialization** — activates the global tracing subscriber. Must be invoked exactly once before any diagnostic events are emitted. No parameters; all configuration is read from environment variables.
- **Filter directive** — the `RUST_LOG` environment variable controls per-module verbosity. Defaults to `info,llm_qdisc_proxy=debug` when absent. If `RUST_LOG` is present but empty, the empty string is passed to the tracing system rather than falling back to the default.
- **Output format selection** — the `LLM_QDISC_LOG_JSON` environment variable selects between structured JSON (value `"1"`) and human-readable output (any other value or absence). JSON mode produces flattened event entries suitable for log aggregators.
- **Single-call constraint** — only one initialization is permitted per process lifetime. A second initialization attempt causes a panic in the calling thread.

## Invariants

The following statements hold regardless of implementation details.

- When `RUST_LOG` is absent, the filter directive resolves to `info,llm_qdisc_proxy=debug`. When present but empty, the empty string is used — distinct from the absent case.
- JSON mode is a binary toggle: only the exact string `"1"` enables JSON output; every other value, including empty string or absence, yields human-readable format.
- Output format is strictly binary: either flattened JSON or human-readable with no intermediate or hybrid mode.
- Only one initialization is permitted per process lifetime; repeated initialization is not allowed.
- All tracing events emitted after initialization respect the configured filter and output format; events emitted before initialization are lost.

## Constraints

These limitations are inherent to the telemetry initialization model.

- Initialization must precede all diagnostic output; any events emitted before the subscriber is active are silently dropped.
- No runtime reconfiguration is supported; filter directives and output format are fixed for the lifetime of the process.
- A duplicate initialization call causes a panic in the calling thread; the caller bears responsibility for single-invocation discipline.
- Malformed `RUST_LOG` values are not rejected at configuration time; the tracing system may emit warnings or apply its own fallback at runtime.
- OTLP export is inactive; the internal `init_otlp` scaffold exists as private code only and is not part of the public interface.

## Rationale

Telemetry is designed as a minimal bootstrap layer so that the proxy emits structured diagnostics from its first line of execution.

- Environment-variable configuration avoids requiring a configuration file for basic observability; the proxy can boot with zero explicit setup.
- A binary JSON toggle keeps the deployment decision simple — operators either ship structured logs to an aggregator or read human-readable output locally.
- Default debug-level output for the proxy crate ensures that operational issues are visible without explicit configuration.
- Panicking on duplicate initialization prevents subtle bugs where a second registration silently suppresses output or produces duplicate entries.
- The OTLP scaffold preserves instrumentation call sites for future export without requiring code changes; instrumentation is exporter-agnostic.

## Related

Cross-references to related concepts and source code.

- `[[metrics#Metrics Registry]]` — separate concern: metrics collection is not part of telemetry initialization
- `[[gateway#Reverse Proxy Request Handling]]` — the proxy application invokes telemetry initialization at startup before serving requests
- `[[src/telemetry/mod.rs#init]]` — public initialization entry point and environment variable configuration
