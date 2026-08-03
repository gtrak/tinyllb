# Concept: c_config_schema — Extractor Report

Companion latent.md: `src/config/mod.rs` (with `src/config/loader.rs`).

## Responsibilities

- Declares the full, typed shape of the proxy's runtime configuration (`Config` and its nested sections: `Backend`, `Scheduler`, `CompletionBias`, `Flows`, `Priorities`, `Backpressure`, `Metrics`, `Server`, `KvPolicyConfig`, plus the `Algorithm` and `BackpressureMode` enumerations). Evidence: `src/config/mod.rs:10-33`, `36`,`41`,`96`,`143`,`173`,`182`,`210`,`245`,`299`,`320`,`26`,`29`.
- Produces a fully-resolved `Config` from layered sources: file, environment, and built-in defaults. Evidence: `src/config/loader.rs:65-93`.
- Rejects resolved configuration that is invalid by failing the load with an error. Evidence: `src/config/loader.rs:95`,`99-163`.
- Exposes human-readable duration (de)serialization for public duration fields. Evidence: `src/config/loader.rs:6-62`.
- Provides complete `Default` values for every configuration component, and encodes those same defaults in the loader. Evidence: `mod.rs:43,84,120,160,199,233,276,310,331`; `loader.rs:66-83`.

## Interface surfaces

- **`Config` (exported type)** — deserializable and serializable (human/JSON/YAML), cloneable, `PartialEq`, `Default`. All sub-components are optional at the document level except `backend`. `request_timeout` is optional and, when absent, the default is delegated to the HTTP client (300s). Evidence: `mod.rs:10-11`,`12`,`13-32`.
- **`load()` → `anyhow::Result<Config>` (exported fn)** — reads file at `$CONFIG_PATH` (default `config.yaml`), layers `LLM_QDISC__` environment overrides then built-in defaults, resolves, validates, returns the config or an error on any invalid value or unresolvable source. Evidence: `loader.rs:62-76,77-96`.
- **Backend service config** — a required backend URL plus a poll interval for metrics. Contracts that these defaults hold: `http://localhost:8000` / `1s`. Evidence: `mod.rs:37-56`.
- **Admission-related config** (`KvPolicyConfig`, `CompletionB`, `Scheduler`) — thresholds and toggles controlling behavior when features are enabled; defaults express a safe OFF baseline. Evidence: `mod.rs:65-72,96-108,131-152`.
- **Enumerations** (`Algorithm`, `BackpressureMode`) — serialized lower-case (`drr`/`fifo`/`wfq`; `blocking`/`failfast`/`hybrid`) with a designated default. Evidence: `mod.rs:171-177,288-295`.
- **Duration serde helpers** — durations round-trip between `Duration` and human strings (`"300s"`, `"5m"`); an optional duration also round-trips with an explicit `null`. Malformed strings fail deserialization. Evidence: `loader.rs:6-55`.

## Invariants

- Every load that succeeds returns a `Config` whose values are the validated result, so subsequent consumers observe only values that passed validation. Evidence: `loader.rs:82-93`.
- Missing config components resolve to the documented internal defaults (never absent). Evidence: `loader.rs:79-89`.
- Serialized names of enum variants are fixed lower-case tokens; only those tokens are accepted on input. Evidence: `mod.rs:169,287`.
- A `Config` value is immutable after creation (only sent combinations; `Clone`, `PartialEq` only). Evidence: `mod.rs:14,23).
- The configured backend URL, when the proxy operates on it, is always absolute with a scheme. Evidence: `loader.rs:125-125`.

## Failure modes

- **Invalid numeric/relations of limits** reject load: `max_active_flows == 0`, `starvation_timeout` zero, `default_weight <= 0`, `metrics_interval` zero. Evidence: `loader.rs:99-106,130-130`.
- **Backpressure-mode-specific requirements**: `failfast`/`hybrid` need `max_queue_depth > 0`; `hybrid` additionally needs `max_wait > 0s`. Evidence: `loader.rs:107-120`.
- **Kv policy cross-threshold constraint** — when enabled, reject threshold must be in `(0,1]`, delay in `[0,1]`, and delay `<` reject — otherwise rejected. Note: a zero poll interval would otherwise silently disable the policy. Evidence: `loader.rs:132-150`.
- Human duration strings that cannot be parsed fail deserialization. Evidence: `loader.rs:21-25,47-49`.
- Documenting the observable load contract via an absolute-URL requirement; mis-shaped backend URL is rejected (`backend.url`). Evidence: `loader.rs:125-131`.