# 03 — DRR cap integration (soft cap + snapshot wake)

- **Complexity:** M
- **Timebox:** 60 min
- **Depends on:** 02 (module), 01 (pressure source; not required for compile)

## Objective

Make the DRR admission loop respect a dynamic, pressure-derived active-flow
ceiling (soft cap: in-flight flows are never preempted) and wake on monitor
snapshot changes so pressure changes take effect within one
`metrics_interval` without requiring a completion. Wire
`scheduler.kv_pressure` through `Scheduler::new` to all 20 call sites.

## Files

| File | Change |
|------|--------|
| `src/scheduler/drr.rs` | `DrrState.max_permits` field; cap-aware permit checks in `admission_loop` + `try_select`; `select!` wake on snapshot changes with closed-channel fallback; `scheduler_effective_max_flows` gauge on cap change; new `new_with_policies`/`new_inner`/`admission_loop` params. |
| `src/backend/mod.rs` | `BackendMonitor::snapshot_receiver() -> watch::Receiver<BackendSnapshot>` accessor (clone of the internal receiver; doc: "clones the snapshot watch receiver. `changed()` returns `Err` immediately when the monitor has no live sender (e.g. `BackendMonitor::empty()`)"). |
| `src/scheduler/mod.rs` | `Scheduler::new` gains `kv_pressure: KvPressure` (last param); constructs `PressureCapHandle` (before `monitor` is moved into `KvPolicy::new`); passes to `DrrScheduler::new_with_policies`. `new_with_defaults` passes `KvPressure::default()`. |
| `src/metrics/mod.rs` | `scheduler_effective_max_flows` gauge (help: "Current effective max_active_flows ceiling (pressure-capped)"). |
| `src/main.rs` | Pass `cfg.scheduler.kv_pressure.clone()` (last arg) at the `Scheduler::new` call. |
| 19 test/bench call sites | Append `KvPressure::default()` as the last `Scheduler::new` argument. Sites (from grep — verify with `rg "Scheduler::new\(" src tests benches`): `benches/completion_bias.rs:82`, `tests/gateway.rs:843`, `tests/phase2_e2e.rs:125`, `tests/scheduler_policies.rs:41,114,171,245`, `tests/token_feedback.rs:538,613`, `tests/priority_live.rs:44`, `tests/kv_admission.rs:30,231,309,405,458,539,594,641,709`. Match the local import style each file already uses for `KvBias` (e.g. `tinyllb::config::KvPressure::default()`). |

## Context (verified facts — do not re-derive)

- Current permit model (src/scheduler/drr.rs): `DrrState.available_permits`
  starts at `max_active_flows` (line 149), decrements on each admit
  (`try_select` lines 325, 456), increments in the ticket disarm closure
  (line 228) and the send-error path (line 240). So
  `active = max_permits - available_permits` (invariant: never negative).
- The admission loop (lines 184-248): outer `state.notify.notified().await`,
  then an inner loop that checks `available_permits > 0` and
  `!rr_cursor.is_empty()` (lines 196-202), calls `try_select`, issues
  tickets.
- `try_select` (lines 256-481) early-returns `(None, false)` when
  `available_permits == 0` (lines 264-266) — this early return must become
  cap-aware so the **starvation force-admit path** (Phase 1) also respects
  the cap.
- `kv_bias.pressure()` pattern (src/scheduler/kv_bias.rs:56-61): read
  `monitor.snapshot().map(|s| s.kv_usage.clamp(0.0, 1.0)).unwrap_or(0.0)`.
  `PressureCapHandle` (task 02) already wraps this.
- `BackendMonitor::empty()` drops its watch senders; `changed()` on such a
  receiver returns `Err` immediately — the wake logic must fall back to
  notify-only or the loop busy-spins. All existing tests built on
  `new_with_defaults` exercise this path.
- `Scheduler::new` arg order today (src/scheduler/mod.rs:64-79):
  `(max_active_flows, metrics, registry, backpressure_mode, max_queue_depth,
  max_wait, retry_after_base, starvation_timeout, completion_bias,
  kv_config, monitor, priority_policy, priorities, kv_bias)`. The new
  `kv_pressure: KvPressure` param goes **last**. In the body,
  `monitor` is cloned for `KvBiasHandle` (line 94-98) and then **moved**
  into `KvPolicy::new` (line 100-108) — construct the `PressureCapHandle`
  with `monitor.clone()` next to the `KvBiasHandle` construction.
- `DrrScheduler::new_with_policies` is `pub(crate)`; verify with grep that
  `Scheduler::new` is its only caller before changing its signature.

## Steps

1. **`src/backend/mod.rs`:** add
   ```rust
   pub fn snapshot_receiver(&self) -> tokio::sync::watch::Receiver<BackendSnapshot> {
       self.receiver.clone()
   }
   ```
   next to `stall_receiver()`.
2. **`DrrState`:** add `max_permits: u32` (doc: "Static permit budget
   (`max_active_flows`); `active = max_permits - available_permits`").
   Initialize from `max_active_flows` in `new_inner`.
3. **`new_with_policies` / `new_inner`:** add
   `kv_pressure: Arc<super::pressure_cap::PressureCapHandle>` param
   (place after `kv_bias`); thread into the `admission_loop` spawn and
   pass `max_active_flows` into the loop.
4. **`admission_loop`:**
   ```rust
   async fn admission_loop(
       state: Arc<SharedState>,
       metrics: Arc<Metrics>,
       registry: Arc<FlowRegistry>,
       starvation_timeout: Duration,
       gate: Arc<CompletionBiasGate>,
       kv_bias: Arc<super::kv_bias::KvBiasHandle>,
       kv_pressure: Arc<super::pressure_cap::PressureCapHandle>,
       max_active_flows: u32,
       snapshot_rx: tokio::sync::watch::Receiver<BackendSnapshot>,
   )
   ```
   - `let mut monitor_alive = true;`
   - Outer wait:
     ```rust
     if monitor_alive {
         tokio::select! {
             _ = state.notify.notified() => {}
             res = snapshot_rx.changed() => { if res.is_err() { monitor_alive = false; } }
         }
     } else {
         state.notify.notified().await;
     }
     ```
   - Inner loop, replacing the current permit check:
     ```rust
     let cap = kv_pressure.effective(max_active_flows);
     let (active, has_waiting) = {
         let s = state.inner.lock().unwrap();
         (s.max_permits - s.available_permits, !s.rr_cursor.is_empty())
     };
     if active >= cap || !has_waiting { break; }
     ```
     (Read `cap` once per inner round — same staleness contract as
     `kv_bias.pressure()`.)
   - Gauge: keep `let mut last_cap = u32::MAX;` outside the inner loop;
     after computing `cap`, `if cap != last_cap {
     metrics.scheduler_effective_max_flows.set(cap as f64); last_cap = cap; }`
   - Pass `cap` into `try_select`.
5. **`try_select`:** add `cap: u32` param; replace the
   `available_permits == 0` early return with
   ```rust
   if s.max_permits - s.available_permits >= cap {
       return (None, false);
   }
   ```
6. **`src/scheduler/mod.rs`:** `Scheduler::new` — new last param
   `kv_pressure: KvPressure`; construct
   `let pressure_cap_handle = Arc::new(pressure_cap::PressureCapHandle::new(kv_pressure, monitor.clone()));`
   next to the `KvBiasHandle` construction; pass the handle to
   `DrrScheduler::new_with_policies`. `new_with_defaults` appends
   `KvPressure::default()`.
7. **`src/main.rs`:** append `cfg.scheduler.kv_pressure.clone()` to the
   `Scheduler::new` call.
8. **Metrics:** register `scheduler_effective_max_flows` gauge in
   `create_metrics` + struct field (pattern: `vllm_kv_cache_usage`).
9. **Call sites:** append `KvPressure::default()` at all 19 test/bench
   `Scheduler::new` call sites (list above).

## Invariants to preserve (reviewer will check)

- With `kv_pressure.enabled: false` (default), `cap` is always
  `max_active_flows` → `active >= cap` ⇔ `available_permits == 0` →
  behavior identical to today. **All existing tests must pass unchanged**
  (except the mechanical call-site arg).
- A ticket's disarm closure and the send-error increment are untouched.
- Starvation force-admit (Phase 1 of `try_select`) cannot fire when
  `active >= cap` (the early return precedes it).
- No new locks; the `select!` arm adds no state mutation besides
  `monitor_alive`.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```

All must pass (existing suite is the regression gate for the
disabled-by-default path). Integration tests for the enabled path land in
task 04.
