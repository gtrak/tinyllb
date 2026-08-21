# 04 — Integration tests (pressure cap end-to-end + kv_bias pinning)

- **Complexity:** M
- **Timebox:** 60 min
- **Depends on:** 01, 02, 03

## Objective

Prove the enabled-path behavior: the cap holds new admits at the ladder's
ceiling, pressure changes take effect within one snapshot tick (both
directions, without completions), and in-flight requests are never aborted.
Also pin the kv_bias ramp so the "bias is fully active under high pressure"
contract cannot regress silently.

## Files

| File | Change |
|------|--------|
| `tests/pressure_cap.rs` (NEW) | End-to-end tests through `Scheduler::new` with an injected monitor and a stub backend that holds requests until released. |
| `tests/scheduler_policies.rs` (or `tests/pressure_cap.rs`) | kv_bias ramp pinning tests (co-locate with the other kv_bias-adjacent tests). |
| `tests/config.rs` | (Only if task 02 left any config test out — verify first.) |

## Context (verified facts — do not re-derive)

- **Snapshot injection pattern** (tests/kv_admission.rs:80 et al.):
  ```rust
  let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot::default());
  let monitor = Arc::new(BackendMonitor::from_receiver(rx));
  // ... build scheduler with this monitor ...
  let _ = tx.send(BackendSnapshot { kv_usage: 0.9, ..Default::default() });
  ```
  `BackendMonitor::from_receiver` creates a closed stall channel (fine —
  stall gate stays off).
- **Scheduler construction:** `Scheduler::new(...)` with all 14 args
  (13 + `kv_pressure: KvPressure` from task 03). Use
  `BackpressureMode::Blocking` for these tests.
- **Stub backend pattern:** see `tests/phase2_e2e.rs` /
  `tests/gateway.rs` — an axum `Router` acting as the backend that counts
  concurrent in-flight requests and holds them open until the test releases
  them (e.g. a `tokio::sync::Notify` or oneshot gate per request, or a
  `watch`/`Arc<AtomicBool>` release flag). The test needs: (a) a peak
  concurrent counter, (b) a way to admit N requests, (c) a way to complete
  selected ones. If building a fresh stub is awkward, reuse the existing
  helper builders in `tests/gateway.rs`/`tests/phase2_e2e.rs` where
  practical (they already take `max_active_flows` and return the
  `(app, metrics, scheduler)` tuple); but the monitor MUST be the injected
  one, so prefer constructing the scheduler via `Scheduler::new` with the
  injected monitor and wiring it into an `AppState` directly (see
  `tests/gateway.rs:843` for the exact construction + AppState shape).
- **Ladder for tests:** `max_active_flows: 4`,
  `KvPressure { enabled: true, thresholds: [ {at: 0.5, max_flows: 3},
  {at: 0.8, max_flows: 2}, {at: 0.95, max_flows: 1} ] }`.
- Timing: the admission loop wakes on `tx.send(...)` (watch `changed()`),
  so pressure updates are prompt — no sleeps beyond a short
  `tokio::time::timeout(...)` poll helper (≤ 2s) waiting for the expected
  active count.

## Tests (tests/pressure_cap.rs)

Each test: fresh scheduler + stub backend + injected monitor; fire requests
via the proxy app (axum `oneshot`) or directly via `scheduler.admit(...)` —
**direct `admit` is acceptable and simpler** for the concurrency-counting
tests (ticket = admitted; hold the ticket to keep the flow "active", drop
it to complete). Prefer direct `admit` + ticket holding; use the app only
if a test needs HTTP-level proof.

1. `low_pressure_full_concurrency` — regression guard:
   pressure 0.2, 4 concurrent admits with `max_active_flows: 4` → all 4
   tickets granted (peak active == 4).
2. `high_pressure_holds_at_cap` — pressure 0.9 (band: cap 2) set before any
   admit; fire 5 concurrent admits → exactly 2 tickets granted promptly;
   the other 3 remain awaiting (assert via `scheduler.queue_depth()` or by
   the fact their futures don't complete within a short timeout).
3. `cap_drop_drains_then_holds` — pressure 0.2 → admit 4 (all active).
   Send pressure 0.9 (cap 2). Complete 1 ticket → active 3; assert the
   5th request (fired while pressure high) is NOT admitted (active stays
   3 ≥ cap 2). Complete 1 more → active 2 < cap 3? No: cap 2, active 2 →
   still holds. Fire the sequence so that after completions active == 1,
   then assert refill back up to 2 (not 3). (Exact arithmetic: cap(0.9)=2;
   admits resume only while active < 2.)
4. `pressure_drop_reopens_without_completion` — the snapshot-wake proof:
   pressure 0.95 (cap 1) → admit 1 (active 1); fire a 2nd request (it
   awaits, active 1 ≥ cap 1). No completions. Send pressure 0.2 (cap 4) →
   the 2nd ticket is granted within the timeout. Proves the
   `snapshot_rx.changed()` wake arm (a notify-only loop would hang here
   because no completion occurs).
5. `in_flight_never_aborted` — pressure 0.2, admit 4; pressure → 0.95
   (cap 1); all 4 held tickets still complete normally (drop each ticket,
   `report_accounting` as existing tests do) with no errors; queue depth
   drains to 0.
6. `vllm_style_snapshot_uses_same_ladder` — identical to test 2 but the
   snapshot is labeled as vLLM (same struct; document that the cap is
   signal-source agnostic). (Optional if test 2 already suffices — the
   signal source is not distinguishable in the snapshot; may be folded
   into test 2's doc comment instead.)
7. `disabled_is_inert` — `KvPressure::default()` with pressure 0.99 → 4
   concurrent admits all granted (peak 4). (Covers the default-off
   contract at the scheduler level.)

## kv_bias ramp pinning (tests/scheduler_policies.rs or pressure_cap.rs)

Construct `KvBiasHandle::default()` config via
`Scheduler::new`… simpler: `KvBiasHandle` is public via
`tinyllb::scheduler::KvBiasHandle` — check visibility; if not constructible
from the test crate, test through `Scheduler`/config or add a
`#[cfg(test)]` in-file test in `src/scheduler/kv_bias.rs` (preferred —
in-file, no visibility change):

- `bias_weight_high_pressure_full`: default config, `bias_weight(0.95)
  == 1.0` (and `0.9 == 1.0` — at `bias_full_at` inclusive).
- `bias_weight_midpoint`: `bias_weight(0.7) ≈ 0.5` (default ramp
  0.5→0.9), tolerance 1e-9.
- `bias_weight_below_pressure_below_zero`: `bias_weight(0.3) == 0.0` and
  `bias_weight(0.5) == 0.0` (at `pressure_below` inclusive → 0).
- `bias_weight_disabled_zero`: `enabled: false` → 0.0 at any pressure.
- `select_high_pressure_prefers_footprint` (if `select` is reachable):
  two candidates with footprints 0 and 1000, pressure 0.95 → picks the
  1000-footprint flow regardless of enqueue order. (Skip if it requires
  private types — `FlowCandidate` visibility — in that case the
  weight-level tests are sufficient.)

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```

All new tests pass; no existing test modified except none (this task adds
tests only — if a test exposes a real bug in tasks 01-03, STOP and report
it; do not fix implementation in this task).
