# Configuration Contract

The configuration schema defines the complete validated runtime contract for the vLLM proxy, declaring every tunable parameter and guaranteeing only valid values reach consumers.

## Purpose

The configuration schema defines the validated runtime contract for the vLLM proxy. Resolution layers file, environment, and built-in defaults with strict precedence. Only valid, fully-resolved configurations reach consumers.

## Non-goals

This concept explicitly excludes the following concerns.

- Dynamic reconfiguration or hot-reloading of parameters after initial load.
- Per-request or per-session configuration overrides.
- Schema versioning or migration between incompatible configurations.
- Validation of backend server behavior or reachability beyond URL shape.
- Enforcement of immutability at the type level; immutability is a convention, not a compile-time guarantee.

## Interface

The configuration surface presents contractual guarantees about what values are accepted, how they are resolved, and what errors are possible.

**Resolution contract**

- Configuration is resolved from layered sources: file document, environment overrides, and built-in defaults, with environment taking precedence over file and file over defaults.
- The configuration file path is controlled by the `CONFIG_PATH` environment variable, defaulting to `config.yaml`. A missing file is not an error — resolution proceeds with built-in defaults alone.
- The configuration file must be valid YAML; only YAML format is supported.
- A load that encounters invalid values or malformed input fails entirely — partial or degraded configurations are not delivered.

**Configuration components**

- **Backend specification** requires an absolute URL with scheme and a metrics poll interval; defaults are `http://localhost:8000` and `1s`.
- **Scheduling algorithm** accepts three modes — `fifo`, `wfq`, `drr` — with `drr` as the default.
- **Scheduler limits** declare `max_active_flows` (default `4`) and `starvation_timeout` (default `300s`).
- **Completion bias** controls admission gating for new flows: `enabled` (default `true`), `target_active_flows` (default `0`, meaning "use `max_active_flows`"), and `predictive_admit` (default `false`).
- **Flow defaults** declare `default_weight` (default `1.0`) and `default_priority` (default `50`).
- **Priority classes** declare `interactive` (default `100`), `agent` (default `50`), and `background` (default `10`).
- **Backpressure** declares a mode (`blocking`, `failfast`, `hybrid`; default `blocking`), `max_queue_depth` (default `100`), `max_wait` (default `10s`), and `retry_after_base` (default `1s`).
- **Metrics** declares the metrics endpoint path (default `/metrics`).
- **Server** declares the listener bind address (default `0.0.0.0:8080`).
- **KV-cache policy** declares `enabled` (default `false`), `reject_threshold` (default `0.95`), and `delay_threshold` (default `0.80`).
- **Context policy** declares compression settings: `enabled` (default `false`), `compress_threshold` (default `100000`), `head_keep_turns` (default `3`), `live_keep_turns` (default `6`), `compress_chunk_turns` (default `8`), `summary_max_tokens` (default `2048`), `store_path`, `tokenizer_path`, `sidecar_request_timeout` (default `60s`), `compression_retries` (default `3`), and `prompt_template_path`. See [[context#Context Compression]].

**Duration representation**

- Human-readable duration strings (e.g., `"300s"`, `"5m"`) are accepted as input and round-trip to internal time values.
- The `request_timeout` component is explicitly optional; when absent, the HTTP client's own default timeout applies (300s). Its configuration default is absence (`null`), not a duration value.
- Malformed duration strings cause deserialization failure — no fallback or silent default is applied.

## Invariants

The following statements hold regardless of implementation details.

**Validation guarantee**

- Every successfully loaded configuration passes all validation rules; consumers never observe values that failed validation.
- Every configuration component resolves to a concrete value after a successful load, except `request_timeout`, which resolves to an explicit optional absence when not provided.

**Structural constraints**

- A resolved configuration carries exactly one backend URL and that URL is always absolute with a scheme.
- The configured backend URL is always well-formed; relative URLs, scheme-less hostnames, and malformed URIs are rejected at load time.

## Constraints

These limitations are inherent to the configuration model.

**Validation boundaries**

- `max_active_flows` must be positive; zero is rejected.
- `starvation_timeout` must be positive; zero is rejected.
- `default_weight` must be positive; zero or negative values are rejected.
- `metrics_interval` must be positive; zero is rejected.
- When backpressure mode is `failfast` or `hybrid`, `max_queue_depth` must be positive; zero is rejected.
- When backpressure mode is `hybrid`, `max_wait` must be positive; zero is rejected.
- When backpressure mode is `blocking`, no positive-threshold constraints apply; zero values for queue depth and wait time are valid.
- When `kv_policy.enabled` is `true`, thresholds must satisfy: `reject_threshold` in `(0, 1]`, `delay_threshold` in `[0, 1]`, and `delay_threshold` strictly less than `reject_threshold`. When `enabled` is `false`, these thresholds are not validated.
- When `context_policy.enabled` is `true`, `compress_threshold`, `head_keep_turns`, `live_keep_turns`, `compress_chunk_turns`, `summary_max_tokens`, and `compression_retries` must all be positive, and `store_path` must be non-empty. When `enabled` is `false`, these fields are not validated. A leading `~` in `store_path`, `tokenizer_path`, and `prompt_template_path` is expanded to the user's home directory.

**Input format**

- Duration strings follow a limited human-readable syntax; arbitrary or locale-specific formats are rejected.

## Rationale

The design decisions reflect the operational context of a long-running proxy managing request scheduling, backpressure, and key-value cache policies.

**Why full validation at load time**

- Runtime discovery of invalid configuration in a proxy would cause unbounded failure cascades; rejecting at load prevents partial-state operation.
- Some controls default to ON (`completion_bias`) because completing in-flight flows is the expected operational mode; other controls default to OFF (`kv_policy`) because KV-cache awareness is an opt-in optimization.

**Why layered resolution**

- Environment overrides allow per-deployment tuning without modifying shared configuration files; built-in defaults eliminate boilerplate for simple deployments.
- An optional file source permits zero-configuration bootstrapping while still allowing full override via a config file.

**Why immutability is conventional**

- A single resolved configuration prevents drift between components that depend on consistent settings; the convention is enforced by documentation, not type-level restrictions, to preserve flexibility for operational tooling.

**Why human-readable durations**

- Operators author configuration files by hand; duration strings are more legible than raw millisecond integers and reduce transcription errors.

## Related

This section lists related concepts and source references for the configuration contract.

- backend server contract consumed by the resolved backend URL
- [[scheduler#Scheduler Facade and Policy Selection]] — scheduling behavior governed by scheduler configuration parameters
- [[admission#Backpressure and Admission Rejection]] — backpressure strategies selected by mode and threshold configuration
- [[admission#KV-Cache-Aware Admission Gate]] — admission control thresholds and toggles within the policy configuration
- [[src/config/mod.rs#Config]] — top-level configuration type and component definitions
- [[src/config/mod.rs#Algorithm]] — scheduling algorithm enumeration
- [[src/config/mod.rs#BackpressureMode]] — backpressure mode enumeration
- [[src/config/mod.rs#CompletionBias]] — completion bias sub-configuration
- [[src/config/mod.rs#Priorities]] — priority class values
- [[src/config/mod.rs#Metrics]] — metrics endpoint configuration
- [[src/config/mod.rs#Server]] — server bind address configuration
- [[src/config/mod.rs#KvPolicyConfig]] — KV-cache admission policy configuration
- [[src/config/mod.rs#ContextPolicy]] — context compression policy configuration
- [[context#Context Compression]] — domain semantics of context policy fields
- [[src/config/loader.rs]] — layered resolution and validation

# Configuration Loading and Validation

This concept guarantees that every application start yields a complete, validated configuration through deterministic layering of built-in defaults, an optional YAML file, and environment variable overrides.

## Purpose

This concept guarantees that every application start yields a complete, validated configuration through deterministic layering of built-in defaults, an optional YAML file, and environment variable overrides.

Duration-typed settings accept and emit human-readable text values. A configuration that fails validation is never returned; the load operation returns either a fully valid result or an error.

**Configuration layering**

The resolved configuration merges three sources in strict precedence: built-in defaults form the baseline, an optional YAML file overrides any subset, and environment variables override the final result. Precedence is defaults < file < environment.

**Duration text representation**

Duration-typed settings accept human-readable text input (e.g. `"300s"`, `"5m"`) from configuration sources and serialize back to a human-readable text form.

## Non-goals

This concept does not cover dynamic reconfiguration, schema evolution, or runtime configuration mutation.

- Hot-reloading or reconfiguration after startup.
- Configuration migration or versioned schema support.
- Per-environment configuration profiles or profile inheritance.
- Secrets management or external credential providers.

## Interface

The load entry point, configuration sourcing, and duration format define the contractual surface that consumers depend on. Consumers receive either a complete validated configuration or an error — partial or invalid configurations are not returned.

**Load entry point**

The loader produces a fully resolved configuration or an error with no preconditions: it operates on an empty environment and absent file without failure. The returned configuration always satisfies all validation constraints; invalid configurations cause load failure rather than silent degradation.

**Duration text deserialization**

A human-readable duration string from any configuration source is parsed into a precise duration value. Any unparseable string yields a deserialization error.

**Optional-duration text deserialization**

A duration source may be absent without error. When present, it must parse to a valid duration; unparseable text in a present field yields a deserialization error.

**Configuration sourcing**

The configuration file path is controlled by the `CONFIG_PATH` environment variable, defaulting to `config.yaml`. Environment variables use the `LLM_QDISC` prefix with double-underscore separators to address nested configuration keys.

**Default configuration contract**

Every configuration key has a built-in default, guaranteeing that a source providing no values still yields a complete, valid configuration. The default set covers backend addressing, scheduling policy, flow defaults, priority levels, backpressure behavior, metrics cadence, server binding, key-value policy thresholds, completion bias, and request timeout (`None`).

**Error contract**

The load operation returns a result yielding Config on success or an unstructured error on failure. Errors carry no programmatically inspectable taxonomy — consumers distinguish conditions by message content rather than error variants or codes.

**File error modes**

- An absent configuration file is not an error; built-in defaults are used instead.
- A present but unreadable file (e.g. permission denied) yields an error.
- A file containing invalid YAML syntax yields a deserialization error.
- A file containing values that do not match the expected schema (wrong types, unknown enum variants) yields a deserialization error.
- A non-YAML file at the configured path produces a YAML parse error, which does not distinguish wrong format from malformed YAML.

## Invariants

The resolved configuration satisfies specific value and structural constraints that prevent meaningless or unsafe settings from being returned.

**Value constraints**

- `max_active_flows` is always greater than zero in a returned configuration.
- `starvation_timeout` is strictly positive in a returned configuration.
- `metrics_interval` is strictly positive in a returned configuration.
- `default_weight` is strictly positive in a returned configuration.
- `max_wait` is strictly positive in a returned configuration when backpressure mode is hybrid.
- `max_queue_depth` is strictly positive in a returned configuration when backpressure mode is fail-fast or hybrid.
- The non-optional duration field `retry_after_base` is not validated for positivity; zero values pass validation.
- The optional duration field `request_timeout` is not validated for positivity; an absent value (`None`) is accepted, and a present zero-duration value also passes validation.
- When key-value policy is enabled, `delay_threshold` is strictly less than `reject_threshold`, `reject_threshold` is within (0, 1], and `delay_threshold` is within [0, 1].

**Structural guarantees**

- An absent configuration file is semantically equivalent to defaults-only input.
- The returned configuration is internally consistent with all cross-key constraints enforced by validation.
- Duration text values produced by the serializer are always parseable by the deserializer; lossless round-trip precision is guaranteed for values derived from text input but not for programmatically constructed durations that may contain sub-second precision lost during text formatting.

## Constraints

Configuration loading operates within specific boundaries on sourcing, format, and error behavior. Overrides from any source are subject to the same validation — combining a valid-looking partial override with defaults can fail validation.

**Source constraints**

- The configuration file is optional; its absence is not an error condition.
- Environment variables use the `LLM_QDISC` prefix with double-underscore section separators to address nested configuration keys.
- The `CONFIG_PATH` environment variable controls the configuration file path; its default value is `config.yaml`.
- YAML is the only structured file format accepted; no JSON, TOML, or other formats. A non-YAML file produces a misleading YAML parse error rather than a format-specific error.

**Validation constraints**

- A backend URL must be absolute and include a scheme; relative URLs or scheme-less values are rejected.
- Under hybrid backpressure mode, both `max_queue_depth` and `max_wait` must be positive; under fail-fast mode, only `max_queue_depth` must be positive. Under blocking mode, neither is validated.
- `retry_after_base` and `request_timeout` are not validated for positivity; zero (for `retry_after_base`) or absent values (for `request_timeout`) are accepted.
- A zero `metrics_interval` is rejected because it silently disables the metrics monitor, blocking key-value policy enforcement.

## Rationale

The configuration system enforces safety over convenience. Validation rejects invalid values rather than falling back to defaults, because silently substituting defaults for an explicit override would mask misconfiguration.

**Fail-loud validation**

An invalid configuration is surfaced as an error rather than silently corrected, because a silently corrected value may cause hard-to-diagnose operational issues. The operator must see and fix the invalid input explicitly.

**Layered defaults with optional file**

Built-in defaults cover every key so the application can start without any configuration source. The optional file and environment overrides allow operational customization without requiring boilerplate repetition of unchanged defaults.

**Selective duration validation**

Only durations whose zero value causes silent operational degradation — `starvation_timeout`, `metrics_interval`, and `max_wait` (under hybrid mode) — are validated for positivity. `retry_after_base` and `request_timeout` are not validated because a zero or absent value has an explicit behavioral meaning rather than silently disabling a control.

**Unstructured error reporting**

The load operation uses unstructured errors rather than a structured error type because configuration errors are terminal startup failures, not recoverable runtime conditions. Consumers handle errors by logging and aborting; no error recovery or error branching is required.

## Related

This section lists related concepts and source references for configuration loading.
- [[config#Configuration Contract]] — configuration surfaces and their domain semantics (CompletionBias, request_timeout, Algorithm, etc.).
- the domain structure of the configuration sections the loader populates.
- [[src/config/loader.rs#load]] — load entry point, sourcing, and validation.
- `humantime_serde` and `humantime_serde_option` modules in loader.rs provide duration text serialization.
- [[src/config/mod.rs#Config]] — typed configuration model.
