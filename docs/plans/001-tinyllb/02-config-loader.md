# 02 — Config Schema + Loader (YAML + Env)

**Phase:** 0 (Foundation)
**Depends on:** `01`
**Blocks:** `05`, `08`, `10`, `11`, `12`, `15`.

## Objective

Define the single source of truth for proxy configuration, fully matching the
shape PRD §7 specifies, plus a typed loader that merges a YAML file with
environment overrides.  Later issues consume `Config` rather than reading env
inline, so this lands before any scheduling logic exists.

## Files

| File | Change |
| --- | --- |
| `src/config/mod.rs` | New: `Config` struct hierarchy with serde. |
| `src/config/loader.rs` | New: merge YAML file -> env overrides -> defaults; validation. |
| `config.example.yaml` | New: documented example matching PRD §7 verbatim (see below). |
| `src/main.rs` | Edit: load `Config` at startup, print resolved config at `INFO`. |
| `tests/config.rs` | New: unit tests for loading, env override, validation failures. |

## Steps

1. Define `Config` with nested structs:
   * `Backend { url: Url }`
   * `Scheduler { algorithm: Algorithm, max_active_flows: u32, starvation_timeout: Duration }`
   * `Flows { default_weight: f64, default_priority: u32 }`
   * `Priorities { interactive: u32, agent: u32, background: u32 }`
   * `Backpressure { mode: BackpressureMode }`
   * `Metrics { endpoint: String }`
   * `Server { bind: SocketAddr }`
   * `Algorithm` enum: `Fifo`, `Wfq`, `Drr` (serde lowercase).
   * `BackpressureMode` enum: `Blocking`, `FailFast`, `Hybrid`.
2. `loader`: read `$CONFIG_PATH` (default `config.yaml`); if absent, use
   defaults; then apply `TINYLLB__*` env overrides (double-underscore path,
   e.g. `TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS=4`).  Validate: `max_active_flows > 0`,
   `starvation_timeout > 0s`, `default_weight > 0`, `backend.url` is absolute.
3. `config.example.yaml` — mirror PRD §7 exactly with comments:
   ```yaml
   backend:
     url: http://localhost:8000
   scheduler:
     algorithm: drr
     max_active_flows: 4
     starvation_timeout: 300s
   flows:
     default_weight: 1
   priorities:
     interactive: 100
     agent: 50
     background: 10
   ```
4. `main.rs`: `let cfg = config::load()?; tracing::info!(?cfg, "config loaded");`
5. Tests: load example yaml, env override of `max_active_flows`, invalid
   config returns a typed error, defaults applied when field omitted.

## Verification

* `cargo test --all` covers loader + env override + validation errors.
* `cargo run` with `config.example.yaml` boots and logs the resolved config.
* `TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS=8 cargo run` shows `8` in the log.
* Invalid `backend.url` fails to load with a clear error.
