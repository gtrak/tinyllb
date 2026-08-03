# Concept: c_config_schema — Configuration Schema

## Purpose

The configuration schema defines the complete, validated runtime contract for the vLLM proxy. It declares every tunable parameter, resolves those parameters from layered sources (file, environment, built-in defaults), and guarantees that only valid values reach consumers. Configuration is logically immutable after resolution — consumers are expected to treat it as a read-only contract snapshot.

## Non-goals

This concept does not cover:

- Dynamic reconfiguration or hot-reloading of parameters after initial load
- Per-request or per-session configuration overrides
- Schema versioning or migration between incompatible configurations
- Validation of backend server behavior or reachability beyond URL shape
- Enforcement of immutability at the type level; immutability is a convention, not a compile-time guarantee

## Interface

The configuration surface presents contractual guarantees about what values are accepted, how they are resolved, and what errors are possible.

### Resolution contract

- Configuration is resolved from layered sources: file document, environment overrides, and built-in defaults, with environment taking precedence over file and file over defaults.
- The configuration file path is controlled by the `CONFIG_PATH` environment variable, defaulting to `config.yaml`. A missing file is not an error — resolution proceeds with built-in defaults alone.
- The configuration file must be valid YAML; only YAML format is supported.
- A load that encounters invalid values or malformed input fails entirely — partial or degraded configurations are not delivered.

### Configuration components

- **Backend specification** requires an absolute URL with scheme and a metrics poll interval; defaults are `http://localhost:8000` and `1s`. The backend URL is provided by the loader's default during standard load but is a required field at the deserialization level — direct deserialization without the loader demands a value.
- **Scheduling algorithm** accepts three modes — `fifo`, `wfq`, `drr` — with `drr` as the default. Only these tokens are accepted as input.
- **Scheduler limits** declare `max_active_flows` (default `4`) and `starvation_timeout` (default `300s`).
- **Completion bias** controls admission gating for new flows: `enabled` (default `true`), `target_active_flows` (default `0`, meaning "use `max_active_flows`"), and `predictive_admit` (default `false`, meaning pre-admit when an active flow has delivered ≥90% of estimated tokens).
- **Flow defaults** declare `default_weight` (default `1.0`) and `default_priority` (default `50`).
- **Priority classes** declare `interactive` (default `100`), `agent` (default `50`), and `background` (default `10`).
- **Backpressure** declares a mode (`blocking`, `failfast`, `hybrid`; default `blocking`), `max_queue_depth` (default `100`), `max_wait` (default `10s`), and `retry_after_base` (default `1s`).
- **Metrics** declares the metrics endpoint path (default `/metrics`).
- **Server** declares the listener bind address (default `0.0.0.0:8080`).
- **KV-cache policy** declares `enabled` (default `false`), `reject_threshold` (default `0.95`), and `delay_threshold` (default `0.80`).

### Duration representation

- Human-readable duration strings (e.g., `"300s"`, `"5m"`) are accepted as input and round-trip to internal time values.
- The `request_timeout` component is explicitly optional; when absent, the HTTP client's own default timeout applies (300s). Its configuration default is absence (`null`), not a duration value.
- Malformed duration strings cause deserialization failure — no fallback or silent default is applied.

## Invariants

The following statements hold regardless of implementation details.

### Validation guarantee

- Every successfully loaded configuration passes all validation rules; consumers never observe values that failed validation.
- Every configuration component resolves to a concrete value after a successful load, except `request_timeout`, which resolves to an explicit optional absence when not provided.

### Structural constraints

- A resolved configuration carries exactly one backend URL and that URL is always absolute with a scheme.
- The configured backend URL is always well-formed; relative URLs, scheme-less hostnames, and malformed URIs are rejected at load time.

## Constraints

These limitations are inherent to the configuration model.

### Validation boundaries

- `max_active_flows` must be positive; zero is rejected.
- `starvation_timeout` must be positive; zero is rejected.
- `default_weight` must be positive; zero or negative values are rejected.
- `metrics_interval` must be positive; zero is rejected.
- When backpressure mode is `failfast` or `hybrid`, `max_queue_depth` must be positive; zero is rejected. The validation error message may use a different token representation for the mode name than the configuration input token.
- When backpressure mode is `hybrid`, `max_wait` must be positive; zero is rejected.
- When backpressure mode is `blocking`, no positive-threshold constraints apply; zero values for queue depth and wait time are valid.
- When `kv_policy.enabled` is `true`, thresholds must satisfy: `reject_threshold` in `(0, 1]`, `delay_threshold` in `[0, 1]`, and `delay_threshold` strictly less than `reject_threshold`. When `enabled` is `false`, these thresholds are not validated.

### Input format

- Duration strings follow a limited human-readable syntax; arbitrary or locale-specific formats are rejected.

## Rationale

The design decisions reflect the operational context of a long-running proxy managing request scheduling, backpressure, and knowledge-value policies.

### Why full validation at load time

- Runtime discovery of invalid configuration in a proxy would cause unbounded failure cascades; rejecting at load prevents partial-state operation.
- Some controls default to ON (`completion_bias`) because completing in-flight flows is the expected operational mode; other controls default to OFF (`kv_policy`) because KV-cache awareness is an opt-in optimization.

### Why layered resolution

- Environment overrides allow per-deployment tuning without modifying shared configuration files; built-in defaults eliminate boilerplate for simple deployments.
- An optional file source permits zero-configuration bootstrapping while still allowing full override via a config file.

### Why immutability is conventional

- A single resolved configuration prevents drift between components that depend on consistent settings; the convention is enforced by documentation, not type-level restrictions, to preserve flexibility for operational tooling.

### Why human-readable durations

- Operators author configuration files by hand; duration strings are more legible than raw millisecond integers and reduce transcription errors.

## Related

- `[[?c_backend]]` — backend server contract consumed by the resolved backend URL
- `[[?c_scheduler]]` — scheduling behavior governed by scheduler configuration parameters
- `[[?c_backpressure]]` — backpressure strategies selected by mode and threshold configuration
- `[[?c_admission]]` — admission control thresholds and toggles within the policy configuration
- `[[src/config/mod.rs#Config]]` — top-level configuration type and component definitions
- `[[src/config/mod.rs#Algorithm]]` — scheduling algorithm enumeration
- `[[src/config/mod.rs#BackpressureMode]]` — backpressure mode enumeration
- `[[src/config/mod.rs#CompletionBias]]` — completion bias sub-configuration
- `[[src/config/mod.rs#Priorities]]` — priority class values
- `[[src/config/mod.rs#Metrics]]` — metrics endpoint configuration
- `[[src/config/mod.rs#Server]]` — server bind address configuration
- `[[src/config/mod.rs#KvPolicyConfig]]` — KV-cache admission policy configuration
- `[[src/config/loader.rs]]` — layered resolution and validation
