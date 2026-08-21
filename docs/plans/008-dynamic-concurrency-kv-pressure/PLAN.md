# Plan 008 — Dynamic Concurrency Under KV Pressure

Resolves `issues/03-dynamic-concurrency-kv-pressure.md`.

## Why

Under high KV pressure tinyllb never reduces active-slot concurrency, and on
llama.cpp backends every KV-aware feature (`kv_policy` gate, `kv_bias`, and
any pressure-driven behavior) is blind: **the proxy has no KV pressure signal
for llama.cpp at all.** This plan (a) derives a real pressure signal from
llama-server's `/slots` endpoint and (b) uses it to dynamically cap
`max_active_flows` via a configurable threshold ladder.

## Verified facts (root cause — do not re-derive)

1. **`/metrics` has no usable live KV signal for llama.cpp.**
   `llamacpp:n_tokens_max` is a monotonic high-watermark
   (`metrics.n_tokens_max = std::max(metrics.n_tokens_max, ...)` in
   llama.cpp `tools/server/server-context.cpp`; README: "High watermark of
   the context size observed"). It never decreases → useless as live pressure.
2. **`/slots` is the only real-time source** and requires the `--slots` flag
   on llama-server (absent from `~/opt/llama.cpp/start-server.sh` today).
   Per slot: `n_ctx`, and `n_prompt_tokens` = `prompt.tokens.size()` — current
   resident tokens incl. generated. Idle slots keep reporting it after
   completion (KV stays resident) and report 0 after `prompt_clear()`
   reclaims it, so **Σ `n_prompt_tokens` over all slots == resident KV in the
   pool**. The default response omits `prompt`/`generated` text
   (`slots_debug == 0` → `only_metrics = true`), so bodies are small.
3. **`-kvu` pool semantics** (llama.cpp `llama-context.cpp`): unified pool =
   `n_ctx` shared by all slots; **each slot reports `n_ctx` = the full pool
   size**. So utilization = Σ tokens ÷ single `n_ctx`, NOT ÷ Σ `n_ctx`
   (that understates pressure by the parallelism factor). The issue's numbers
   confirm aggregate pool utilization: 168K÷180K = 93%, 168K÷340K = 49%.
4. **The issue's kv_bias premise is inverted.** `bias_weight` is 0 *below*
   `pressure_below` (0.5) and 1.0 at/above `bias_full_at` (0.9) — the bias is
   maximally active under high pressure
   (src/scheduler/kv_bias.rs:67-77). The observed "bias disabled itself"
   happened because llama.cpp pressure was always 0.0 (no signal), not because
   of the gate. **No gate change**; fixing the signal fixes the bias.
5. Plan 007 explicitly deferred this work ("/slots-derived KV-pressure
   substitute for kv_policy/kv_bias … once the value of the signal is
   proven").

## What

### A. /slots-derived KV pressure (backend monitor)

- In `poll_loop`, after a successful `/metrics` scrape with `found_llamacpp`,
  also GET `{base}/slots` (JSON via `serde_json`).
- `kv_usage = clamp(Σ n_prompt_tokens ÷ pool, 0..1)`;
  `pool = kv_unified ? slot_n_ctx : Σ n_ctx`. New `backend.kv_unified: bool`
  config (default `false`) mirrors llama-server's `-kvu` flag.
- Written into `snapshot.kv_usage` before publish → automatically feeds
  `KvPolicy` (admission gate), `KvBias`, and the new cap.
- `/slots` unavailable (flag off / HTTP error / malformed / empty / pool 0)
  → `kv_usage` stays 0.0, everything inert (today's behavior); warn once on
  the good→bad transition.
- New gauge `llm_backend_kv_pressure` set on every publish (both flavors).

### B. Dynamic cap config + module

- New `scheduler.kv_pressure` config (default **disabled**, zero behavior
  change):
  ```yaml
  scheduler:
    kv_pressure:
      enabled: false
      thresholds:            # pressure >= at → cap to max_flows (min over matched)
        - { at: 0.50, max_flows: 3 }
        - { at: 0.80, max_flows: 2 }
        - { at: 0.95, max_flows: 1 }
  ```
- New `src/scheduler/pressure_cap.rs`: `PressureCapHandle`
  (config + `Arc<BackendMonitor>`, mirrors `KvBiasHandle`):
  - `pressure() -> f64` — latest snapshot `kv_usage`, clamped to [0,1]
  - `effective_max(max_active_flows, pressure) -> u32` — pure function: min of
    `max_flows` over thresholds with `pressure >= at`, capped by
    `max_active_flows`; disabled → `max_active_flows`
- Loader validation (when enabled): thresholds non-empty; `at` strictly
  ascending in [0,1]; each `max_flows` in [1, `max_active_flows`].

### C. DRR cap integration (soft cap, no preemption — confirmed with user)

- `DrrState` gains `max_permits: u32` so `active = max_permits −
  available_permits` is derivable.
- Permit condition becomes `active < effective_cap` (replacing
  `available_permits > 0` in the inner-loop gate; same check in
  `try_select`'s early return, which also covers the starvation force-admit
  path).
- Wake sources: the admission loop's outer wait becomes `select!` over
  `state.notify` **and** the monitor snapshot `changed()` — a pressure *drop*
  reopens admissions within one `metrics_interval` without a completion; a
  pressure *rise* stops new admits on the next tick. Closed channel
  (`BackendMonitor::empty()` in tests) → notify-only. Requires a
  `BackendMonitor::snapshot_receiver()` accessor (clone of the watch
  receiver, like `stall_receiver()`).
- New gauge `scheduler_effective_max_flows`.
- Semantics: in-flight requests are never aborted; when the cap drops below
  the active count the system drains to the cap as requests complete; cap is
  evaluated once per admission round (same pattern as the kv_bias pressure
  read).
- `Scheduler::new` gains a `kv_pressure: KvPressure` parameter — 20
  mechanical call-site updates (`KvPressure::default()` in tests/benches;
  `cfg.scheduler.kv_pressure.clone()` in main.rs), same pattern as the
  kv_bias addition (plan 006).

### D. kv_bias

No code change. Tests pin the ramp: `bias_weight(0.95) == 1.0`,
`bias_weight(0.7) == 0.5` (default ramp), `bias_weight(0.3) == 0.0`.

## Success criteria

- [ ] On a llama.cpp backend with `--slots`, `llm_backend_kv_pressure`
      tracks live pool utilization (busy slot 168K/340K → ≈ 0.494) and
      decays when slots free/clear.
- [ ] Without `--slots` (or on vLLM), behavior is byte-identical to today
      (kv_usage 0.0 on llama.cpp; vLLM gauge values unchanged).
- [ ] With the ladder enabled, peak active flows never exceed the cap for the
      current pressure band; pressure changes take effect within ~1
      `metrics_interval` without requiring a completion; in-flight requests
      are never aborted.
- [ ] Disabled by default; `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass; `lat check`
      passes.
- [ ] Issue scenario: 168K request at ctx-size 180K → pressure 0.93 → cap 2
      (per the example ladder) and bias weight 1.0; at ctx-size 340K → 0.49
      → full concurrency and bias weight 0.

## Scope

### In scope

- `src/backend/mod.rs` — slots JSON parser, poll-loop `/slots` fetch,
  derived `kv_usage`, `snapshot_receiver()` accessor, warn-once, gauge.
- `src/config/mod.rs`, `src/config/loader.rs` — `Backend.kv_unified`,
  `Scheduler.kv_pressure` + defaults + validation.
- `src/scheduler/pressure_cap.rs` (new), `src/scheduler/drr.rs`,
  `src/scheduler/mod.rs` — cap module, cap-aware DRR permits, snapshot wake,
  `Scheduler::new` param.
- `src/main.rs` + 19 test/bench `Scheduler::new` call sites.
- `src/metrics/mod.rs` — `llm_backend_kv_pressure`,
  `scheduler_effective_max_flows` gauges.
- `config.example.yaml`, `README.md` (llama.cpp quickstart), `lat.md/`,
  `tests/pressure_cap.rs` (new), `tests/config.rs`.

### Out of scope

- Modifying llama.cpp or its launch script (user-side ops: add `--slots`).
- Hard cap / preemption of in-flight flows (confirmed not wanted).
- kv_bias gate changes (signal fix is sufficient; confirmed).
- Per-flow KV block counts (vLLM exposes only a global gauge; llama.cpp only
  per-slot; the delivered-token footprint proxy stands).
- Session resume / KV persistence in llama.cpp (upstream limitation).

## Task order

```
01 (backend: /slots parser + poll fetch + derived kv_usage + kv_unified cfg)
 → 02 (config: KvPressure + validation; pressure_cap.rs module)
 → 03 (DRR: cap-aware permits + snapshot wake + Scheduler::new wiring)
 → 04 (tests: pressure_cap integration + kv_bias pinning + config)
 → 05 (docs: lat.md + config.example + README + lat check + issue archive)
```

- 01 is foundational (pressure signal).
- 02 is independent of 01's runtime but needed by 03.
- 03 touches the admission loop and `Scheduler::new` signature (20 call
  sites).
- 04 depends on 01+02+03.
- 05 is last (docs reflect final behavior).

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| `/slots` schema drift across llama.cpp versions | Lenient parser (unknown fields ignored, missing fields tolerated); failure → inert + one warn. Realistic-body unit test pins the format. |
| Stale pressure (1s scrape granularity) | Cap is advisory and soft; worst case is one extra admit per band crossing — same staleness as existing kv_policy/kv_bias. |
| `Scheduler::new` signature churn | Mechanical 20-site update, pattern proven in plan 006; compile-gated. |
| `kv_policy` gate suddenly live on llama.cpp (was inert at 0.0) | Only when user enables `backpressure.kv_policy.enabled` (default false); interaction documented in lat.md + config example. |
| Busy-loop on closed monitor channel in the new `select!` | Closed-channel fallback to notify-only; covered by existing `new_with_defaults`-based tests. |

## Ops follow-ups (user-side, outside this repo)

1. Add `--slots` to `~/opt/llama.cpp/start-server.sh` (monitoring endpoint
   only; no serving behavior change).
2. `config.yaml`: set `backend.kv_unified: true` (mirrors `-kvu`) and enable
   `scheduler.kv_pressure` with the desired ladder.
