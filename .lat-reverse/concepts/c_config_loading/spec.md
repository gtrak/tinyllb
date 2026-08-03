# c_config_loading — Spec

## Purpose

This concept guarantees that every application start yields a complete, validated configuration through deterministic layering of built-in defaults, an optional YAML file, and environment variable overrides. Duration-typed settings accept and emit human-readable text values. A configuration that fails validation is never returned; the load operation returns either a fully valid result or an error.

### Configuration layering

The resolved configuration merges three sources in strict precedence: built-in defaults form the baseline, an optional YAML file overrides any subset, and environment variables override the final result. Precedence is defaults < file < environment.

### Duration text representation

Duration-typed settings accept human-readable text input (e.g. `"300s"`, `"5m"`) from configuration sources and serialize back to a human-readable text form.

## Non-goals

This concept does not cover dynamic reconfiguration, schema evolution, or runtime configuration mutation. It does not provide partial configuration, per-request overrides, or configuration validation beyond the startup boundary. It does not manage secrets, credentials, or credential rotation.

### Out of scope

- Hot-reloading or reconfiguration after startup.
- Configuration migration or versioned schema support.
- Per-environment configuration profiles or profile inheritance.
- Secrets management or external credential providers.

## Interface

The load entry point, configuration sourcing, and duration text format define the contractual surface that consumers depend on. Consumers receive either a complete validated configuration or an error — partial or invalid configurations are not returned.

### Load entry point

The loader produces a fully resolved configuration or an error with no preconditions: it operates on an empty environment and absent file without failure. The returned configuration always satisfies all validation constraints; invalid configurations cause load failure rather than silent degradation.

### Duration text deserialization

A human-readable duration string from any configuration source is parsed into a precise duration value. Any unparseable string yields a deserialization error.

### Optional-duration text deserialization

A duration source may be absent without error. When present, it must parse to a valid duration; unparseable text in a present field yields a deserialization error.

### Configuration sourcing

The configuration file path is controlled by the `CONFIG_PATH` environment variable, defaulting to `config.yaml`. Environment variables use the `LLM_QDISC` prefix with double-underscore separators to address nested configuration keys.

### Default configuration contract

Every configuration key has a built-in default, guaranteeing that a source providing no values still yields a complete, valid configuration. The default set covers:

- Backend addressing (`url = http://localhost:8000`, `metrics_interval = 1s`).
- Scheduling policy (`algorithm = drr`, `max_active_flows = 4`, `starvation_timeout = 300s`).
- Scheduling algorithms accept three values serialized as lowercase text: `"fifo"`, `"wfq"`, `"drr"`.
- Flow defaults (`default_weight = 1.0`, `default_priority = 50`).
- Priority levels (`interactive = 100`, `agent = 50`, `background = 10`).
- Backpressure behavior (`mode = blocking`, `max_queue_depth = 100`, `max_wait = 10s`, `retry_after_base = 1s`).
- Metrics cadence (`endpoint = /metrics`).
- Server binding (`bind = 0.0.0.0:8080`).
- Key-value policy thresholds (`enabled = false`, `reject_threshold = 0.95`, `delay_threshold = 0.80`).
- Completion bias (`enabled = true`, `target_active_flows = 0` meaning "use max_active_flows", `predictive_admit = false`).
- Request timeout (`None` — absent timeout defers to the HTTP client's built-in timeout, 300s in reqwest).

### Error contract

The load operation returns a result yielding Config on success or an unstructured error on failure. Errors carry no programmatically inspectable taxonomy — consumers distinguish conditions by message content rather than error variants or codes.

### File error modes

- An absent configuration file is not an error; built-in defaults are used instead.
- A present but unreadable file (e.g. permission denied) yields an error.
- A file containing invalid YAML syntax yields a deserialization error.
- A file containing values that do not match the expected schema (wrong types, unknown enum variants) yields a deserialization error.
- A non-YAML file at the configured path produces a YAML parse error, which does not distinguish wrong format from malformed YAML.

## Invariants

The resolved configuration satisfies specific value and structural constraints that prevent meaningless or unsafe settings from being returned. These invariants hold regardless of how the underlying implementation changes.

### Value constraints

- `max_active_flows` is always greater than zero in a returned configuration.
- `starvation_timeout` is strictly positive in a returned configuration.
- `metrics_interval` is strictly positive in a returned configuration.
- `default_weight` is strictly positive in a returned configuration.
- `max_wait` is strictly positive in a returned configuration when backpressure mode is hybrid.
- `max_queue_depth` is strictly positive in a returned configuration when backpressure mode is fail-fast or hybrid.
- The non-optional duration field `retry_after_base` is not validated for positivity; zero values pass validation.
- The optional duration field `request_timeout` is not validated for positivity; an absent value (`None`) is accepted, and a present zero-duration value also passes validation.
- When key-value policy is enabled, `delay_threshold` is strictly less than `reject_threshold`, `reject_threshold` is within (0, 1], and `delay_threshold` is within [0, 1].

### Structural guarantees

- An absent configuration file is semantically equivalent to defaults-only input.
- The returned configuration is internally consistent with all cross-key constraints enforced by validation.
- Duration text values produced by the serializer are always parseable by the deserializer; lossless round-trip precision is guaranteed for values derived from text input but not for programmatically constructed durations that may contain sub-second precision lost during text formatting.

## Constraints

Configuration loading operates within specific boundaries on sourcing, format, and error behavior. Overrides from any source are subject to the same validation — combining a valid-looking partial override with defaults can fail validation.

### Source constraints

- The configuration file is optional; its absence is not an error condition.
- Environment variables use the `LLM_QDISC` prefix with double-underscore section separators to address nested configuration keys.
- The `CONFIG_PATH` environment variable controls the configuration file path; its default value is `config.yaml`.
- YAML is the only structured file format accepted; no JSON, TOML, or other formats. A non-YAML file produces a misleading YAML parse error rather than a format-specific error.

### Validation constraints

- A backend URL must be absolute and include a scheme; relative URLs or scheme-less values are rejected.
- Under hybrid backpressure mode, both `max_queue_depth` and `max_wait` must be positive; under fail-fast mode, only `max_queue_depth` must be positive. Under blocking mode, neither is validated.
- `retry_after_base` and `request_timeout` are not validated for positivity; zero (for `retry_after_base`) or absent values (for `request_timeout`) are accepted.
- A zero `metrics_interval` is rejected because it silently disables the metrics monitor, blocking key-value policy enforcement.

## Rationale

The configuration system enforces safety over convenience. Validation rejects invalid values rather than falling back to defaults, because silently substituting defaults for an explicit override would mask misconfiguration. Duration text format is used because operators configure these values manually and prefer human-readable units over raw numeric types.

### Fail-loud validation

An invalid configuration is surfaced as an error rather than silently corrected, because a silently corrected value may cause hard-to-diagnose operational issues. The operator must see and fix the invalid input explicitly.

### Layered defaults with optional file

Built-in defaults cover every key so the application can start without any configuration source. The optional file and environment overrides allow operational customization without requiring boilerplate repetition of unchanged defaults.

### Selective duration validation

Only durations whose zero value causes silent operational degradation — `starvation_timeout`, `metrics_interval`, and `max_wait` (under hybrid mode) — are validated for positivity. `retry_after_base` and `request_timeout` are not validated because a zero or absent value has an explicit behavioral meaning rather than silently disabling a control.

### Unstructured error reporting

The load operation uses unstructured errors rather than a structured error type because configuration errors are terminal startup failures, not recoverable runtime conditions. Consumers handle errors by logging and aborting; no error recovery or error branching is required.

## Related

- [[?c_config_schema]] — configuration surfaces and their domain semantics (CompletionBias, request_timeout, Algorithm, etc.).
- [[?c_config_model]] — the domain structure of the configuration sections the loader populates.
- [[src/config/loader.rs#load]] — load entry point, sourcing, and validation.
- [[src/config/loader.rs#humantime_serde]] — duration text serialization helpers.
- [[src/config/mod.rs#Config]] — typed configuration model.
