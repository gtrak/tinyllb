# c_config_loading — Extraction

## Responsibilities

- Produce the resolved application configuration used at startup by layering, in order: built-in defaults, an optional YAML file, and environment overrides.
- Deserialize YAML + environment values into the typed configuration model and surface a typed result or a typed error.
- Apply a post-load validation pass that rejects configurations that would be operationally unsafe or meaningless (zero/inverted values, malformed URLs).
- Provide a text format for durations and optional durations used in config source data, and convert them losslessly to/from a duration value.

## Interface surfaces

### `load()` — entry point
- Inputs: ambient — the `CONFIG_PATH` environment variable (file path, defaults to `config.yaml`); optional YAML file at that path; `LLM_QDISC`-prefixed environment variables with `__` as section separator (e.g. `LLM_QDISC__BACKEND__URL`).
- Outputs: fully resolved `Config` — every key populated either from defaults, the YAML file, or environment overrides, in that precedence order (defaults < file < environment).
- Errors (all `anyhow`-style errors):
  - YAML file is present but unreadable/not valid YAML, or values fail deserialization (wrong type, unknown enum variant, invalid enum text).
  - Environment values that fail deserialization (wrong type, invalid enum text).
  - Post-load validation failure (see failure modes).
  - An absent file is not an error — defaults are used instead (the file source is non-required).
- Preconditions: none (works from empty environment).
- Postconditions: the returned value always satisfies validation (invalid configs are rejected, not returned).
- Code evidence: `src/config/loader.rs:62-97`; exposed via `pub use loader::load` in `src/config/mod.rs:3`.

### Duration text (de)serialization — `humantime_serde`
- Inputs (deserialize): a human-readable duration string (e.g. `"300s"`, `"5m"`) from a serialized config source.
- Outputs (serialize): the canonical human-readable text form of a duration (e.g. `"300s"`).
- Errors (deserialize): any string that the duration parser cannot interpret yields a deserialization error.
- Code evidence: `src/config/loader.rs:6-25`. Contractual coupling with duration-typed config fields declared with `with = "loader::humantime_serde"` (e.g. `starvation_timeout`, `max_wait`, `metrics_interval`).

### Optional-duration text (de)serialization — `humantime_serde_option`
- Inputs (deserialize): either a human-readable duration string or an absent/null value.
- Outputs (serialize): the duration text for a present value, or null/absent for a missing value.
- Errors (deserialize): a present but unparseable duration string yields a deserialization error; absent values never error.
- Code evidence: `src/config/loader.rs:28-55`. Used by the request-level timeout field (`src/config/mod.rs:28`).

### Configuration contract — default keys
The loader guarantees a complete, valid configuration even when no file and no overrides are present, via defaults for every key:
- `backend.url = http://localhost:8000` (loader.rs:66)
- `scheduler.algorithm = drr` (loader.rs:67)
- `scheduler.max_active_flows = 4` (loader.rs:68)
- `scheduler.starvation_timeout = 300s` (loader.rs:69)
- `flows.default_weight = 1.0` (loader.rs:70)
- `flows.default_priority = 50` (loader.rs:71)
- `priorities.interactive = 100` (loader.rs:72)
- `priorities.agent = 50` (loader.rs:73)
- `priorities.background = 10` (loader.rs:74)
- `backpressure.mode = blocking` (loader.rs:75)
- `backpressure.max_queue_depth = 100` (loader.rs:76)
- `backpressure.max_wait = 10s` (loader.rs:77)
- `backpressure.retry_after_base = 1s` (loader.rs:78)
- `backend.metrics_interval = 1s` (loader.rs:79)
- `metrics.endpoint = /metrics` (loader.rs:80)
- `server.bind = 0.0.0.0:8080` (loader.rs:81)
- `kv_policy.enabled = false` (loader.rs:82)
- `kv_policy.reject_threshold = 0.95` (loader.rs:83)
- `kv_policy.delay_threshold = 0.80` (loader.rs:84)

## Invariants

- A duration value and its human-readable text form round-trip losslessly through the serializer/deserializer pair (the serializer emits exactly what the parser accepts, loader.rs:14, 23).
- The resolved configuration is always internally consistent with the key relationships the validator enforces — e.g. delay threshold strictly below reject threshold — because validation runs before a value is returned (loader.rs:95, 158).
- An absent configuration file never changes the outcome; it behaves identically to defaults-only input (the file source is marked non-required, loader.rs:88).
- A `max_active_flows` of zero is never present in a returned configuration (rejected at loader.rs:100-102).
- Every duration-valued setting is guaranteed non-zero in a returned configuration (loader.rs:103-105, 122-124, 142-144).
- Defaults cover every key the model deserializes, so a source that provides nothing still yields a valid configuration (loader.rs:65-84, 92-93).

## Failure modes

- **Validation rejection**: a configuration that deserializes successfully but violates a constraint is rejected with a specific error:
  - `scheduler.max_active_flows == 0` (loader.rs:100-102)
  - `scheduler.starvation_timeout` zero (loader.rs:103-105)
  - `flows.default_weight <= 0` (loader.rs:106-108)
  - zero `backpressure.max_queue_depth` under `fail_fast` or `hybrid` mode (loader.rs:109-121)
  - zero `backpressure.max_wait` under `hybrid` mode (loader.rs:122-128)
  - `backend.url` that is not an absolute URL with a scheme (loader.rs:129-138)
  - zero `backend.metrics_interval` (loader.rs:142-144)
  - when `kv_policy.enabled`: `reject_threshold` outside `(0,1]` (loader.rs:148-151), `delay_threshold` outside `[0,1]` (loader.rs:153-157), or `delay_threshold >= reject_threshold` (loader.rs:158-162)
- **Deserialization failure**: unreadable/invalid YAML file, out-of-range or mistyped values, or unknown enum variants produce an error before validation is reached (loader.rs:92-93).
- **Silent behavioral trap (potential, guarded)**: a zero `backend.metrics_interval` would mean the metrics monitor never polls, silently disabling KV policy — this is why the validator rejects it (comment loader.rs:140-141, check 142-144).
- **Overrides may violate constraints**: environment/file values override defaults, so a "valid-looking" partial override can combine with another defaulted value to fail validation; such combinations fail loudly rather than being silently accepted (loader.rs:92-95).

## Related

- `[[src/config/mod.rs]]` — the typed configuration model (`Config`) that the loader deserializes into and validates.
- `[[?c_config_model]]` — the shape/domain meaning of the configuration sections the loader populates.
