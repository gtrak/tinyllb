# 02 — Dynamic cap config + PressureCapHandle module

- **Complexity:** S
- **Timebox:** 45 min
- **Depends on:** 01 (for the monitor's `kv_usage` to be meaningful; the
  module itself only needs the monitor API that already exists)

## Objective

Add the `scheduler.kv_pressure` config (disabled by default) and the
`PressureCapHandle` that maps KV pressure to an effective
`max_active_flows` cap, ready to be consumed by the DRR scheduler in task
03. Mirrors the `KvBias` / `KvBiasHandle` pattern.

## Files

| File | Change |
|------|--------|
| `src/config/mod.rs` | `KvPressure` + `KvPressureThreshold` structs with serde defaults; `Scheduler.kv_pressure` field. |
| `src/config/loader.rs` | `set_default("scheduler.kv_pressure.enabled", false)`; validation in `validate()`. |
| `src/scheduler/pressure_cap.rs` (NEW) | `PressureCapHandle` with `pressure()` and pure `effective_max()`. |
| `src/scheduler/mod.rs` | `mod pressure_cap;` + `pub use pressure_cap::PressureCapHandle;`. No `Scheduler::new` change yet (that is task 03). |
| `tests/config.rs` | Config parse/validation tests (see Tests below). |

`src/metrics/mod.rs` is NOT touched here (the `scheduler_effective_max_flows`
gauge is wired where it is observed, in task 03).

## Context (verified facts — do not re-derive)

- `KvBias` (src/config/mod.rs:317-354) is the style template: serde
  defaults via associated `fn default_*()`, `Clone, Debug, PartialEq,
  Serialize, Deserialize`.
- `KvBiasHandle` (src/scheduler/kv_bias.rs) is the handle template:
  `config` + `Arc<BackendMonitor>` + `#[derive(Clone)]`;
  `pressure()` reads `self.monitor.snapshot().map(|s| s.kv_usage.clamp(0.0,
  1.0)).unwrap_or(0.0)`.
- Loader validation pattern: see the `kv_policy` threshold validation block
  in `validate()` (src/config/loader.rs:168-188).
- `Scheduler::new` signature change and all call-site updates happen in
  task 03, NOT here. Do not touch `Scheduler::new`.

## Steps

1. **Config structs** in `src/config/mod.rs` (next to `KvBias`):
   ```rust
   /// KV-pressure-driven dynamic concurrency cap.
   ///
   /// Maps backend KV pressure to an effective `max_active_flows` ceiling.
   /// For each threshold with `pressure >= at`, the effective cap is the
   /// minimum of `max_flows` across all matched thresholds (and
   /// `max_active_flows` itself). Disabled by default: when disabled or
   /// `thresholds` is empty, the cap is always `max_active_flows` (no
   /// behavioral change).
   #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
   pub struct KvPressure {
       #[serde(default)]
       pub enabled: bool,
       /// Ladder of (pressure, max_flows) pairs. `at` in [0,1]; matched
       /// when `pressure >= at`. Must be strictly ascending by `at`.
       #[serde(default)]
       pub thresholds: Vec<KvPressureThreshold>,
   }

   #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
   pub struct KvPressureThreshold {
       /// Pressure level (KV usage fraction) at which this entry activates.
       pub at: f64,
       /// Active-flow ceiling while this entry is the most stringent match.
       pub max_flows: u32,
   }
   ```
   `KvPressure::default()` → `{ enabled: false, thresholds: vec![] }`.
   Add `#[serde(default)] pub kv_pressure: KvPressure` to `Scheduler` and to
   its `Default` impl.
2. **Loader:** add `set_default("scheduler.kv_pressure.enabled", false)?`.
   Validation (in `validate()`, only when `cfg.scheduler.kv_pressure.enabled`):
   - `thresholds` non-empty → else error
     `"kv_pressure.thresholds must not be empty when kv_pressure is enabled"`
   - each `at` in [0,1] → else
     `"kv_pressure.thresholds[].at must be in [0, 1]"`
   - strictly ascending `at` → else
     `"kv_pressure.thresholds must be strictly ascending by 'at'"`
   - each `max_flows` in [1, `cfg.scheduler.max_active_flows`] → else
     `"kv_pressure.thresholds[].max_flows must be in [1, max_active_flows]"`
3. **`src/scheduler/pressure_cap.rs`** (new file):
   ```rust
   //! KV-pressure-driven dynamic concurrency cap.
   //!
   //! Maps the backend's KV-usage pressure to an effective
   //! `max_active_flows` ceiling. Soft cap: it only limits new admissions
   //! (the DRR scheduler stops granting permits at the cap); in-flight
   //! flows are never preempted. Disabled by default.
   // @lat: [[scheduler_policies#KV-Pressure Concurrency Cap]]
   #[derive(Clone)]
   pub struct PressureCapHandle {
       config: KvPressure,
       monitor: Arc<BackendMonitor>,
   }

   impl PressureCapHandle {
       pub fn new(config: KvPressure, monitor: Arc<BackendMonitor>) -> Self;

       /// Whether the cap is enabled.
       pub fn enabled(&self) -> bool;

       /// Current global KV pressure in [0,1] from the backend monitor
       /// (same source as the KV bias and the admission gate).
       pub fn pressure(&self) -> f64;

       /// Effective active-flow ceiling for the given pressure.
       /// Pure: `min(max_active_flows, min over thresholds with
       /// pressure >= at of max_flows)`; `max_active_flows` when disabled
       /// or no threshold matches.
       pub fn effective_max(&self, max_active_flows: u32, pressure: f64) -> u32;

       /// Convenience: read live pressure, return the effective cap.
       pub fn effective(&self, max_active_flows: u32) -> u32;
   }
   ```
   Keep `effective_max` pure (takes `pressure` as an argument) so the ladder
   is unit-testable without a monitor.
4. **Module declaration** in `src/scheduler/mod.rs`: `mod pressure_cap;` and
   `pub use pressure_cap::PressureCapHandle;` (alphabetical placement among
   the existing `mod`/`use` lines).

## Tests

- **In-file unit tests** in `pressure_cap.rs` (`#[cfg(test)] mod tests`):
  - `effective_max_disabled_returns_max` (enabled=false, pressure 0.99 → max).
  - `effective_max_empty_thresholds_returns_max` (enabled=true, empty list).
  - `effective_max_below_first_threshold_returns_max` (ladder
    [(0.5,3),(0.8,2),(0.95,1)], pressure 0.49, max 4 → 4).
  - `effective_max_bands`: 0.5 → 3, 0.79 → 3, 0.8 → 2, 0.949 → 2,
    0.95 → 1, 1.0 → 1 (boundary is inclusive: `pressure >= at`).
  - `effective_max_never_exceeds_max_active_flows` (ladder entry with
    max_flows 10, max_active_flows 4 → 4).
  - `pressure_clamped_and_absent_snapshot` (empty monitor → 0.0).
- **`tests/config.rs`**:
  - default config → `kv_pressure.enabled == false`, empty thresholds.
  - yaml with the full ladder parses (all fields).
  - validation errors: enabled + empty thresholds; unsorted `at`;
    `at: 1.5`; `max_flows: 0`; `max_flows: 8` with `max_active_flows: 4`.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```

All must pass; no behavioral change yet (nothing consumes the handle).
