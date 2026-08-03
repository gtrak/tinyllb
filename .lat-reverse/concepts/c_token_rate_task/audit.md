# Token Rate Task — Cycle 3 Audit

Final-cycle auditor report. Spec thrice-corrected; compared against `src/main.rs::spawn_token_rate_task` and supporting sources.

## Status

**PASS** — Spec is substantially accurate. Two minor findings remain.

---

## Findings

### spec_error (2)

1. **Interface § bullet 5 — Zero-reporting condition imprecise during warmup.**

   The spec states: *"it reports exactly zero only after the window is entirely filled with zero deltas."*

   During warmup (when `samples.len() < window_secs`), if all accumulated deltas are zero, the gauge reports zero even though the window is not full. The "only after entirely filled" claim is false for the warmup phase. Correct statement: *"the gauge reports zero whenever all deltas currently in the window are zero — which can occur at any time, including during warmup."*

2. **Interface § bullet 4 — "sub-second" misnomer for a u64 field.**

   The spec states: *"sub-second configurations are promoted to the floor."* The config field `tps_window_secs` is `u64` (integer seconds). Fractional seconds are impossible at the type level; the only edge case is `0`. The phrasing implies fractional configuration exists when it does not. Replace "sub-second configurations" with "a zero value."

### undocumented_behavior (1)

1. **Default window value not stated.**

   The spec describes the smoothing window as configurable but does not state the default value. The implementation defaults `tps_window_secs` to `10` (seconds) in `src/config/mod.rs:333-335`. This is a configuration contract that external consumers (operators reading config docs) would expect to find in the Interface section.

### No How lint

**PASS** with one borderline observation.

- Invariant bullet 4 — *"One observation is produced each second at a fixed cadence"* uses "produced" and "cadence," which lean toward implementation description. Acceptable as-is (describes observable gauge-update frequency), but tighter invariant phrasing would be: *"The gauge updates at a frequency of once per second."*

No other No How violations detected. No control flow, data structure names, or function names appear in Purpose / Interface / Invariants / Constraints.

### bug — None

The implementation faithfully implements every spec claim.

### missing_interface — None

The spec correctly declares no callable API. `spawn_token_rate_task` is a `pub fn` in a binary crate (`src/main.rs`), so it is not externally importable.

---

## Verdict

The spec is accurate to the implementation. Two spec_errors are minor precision issues (warmup zero-condition wording, u64 "sub-second" phrasing). One piece of undocumented_behavior (default window value) is useful to capture for completeness. No bugs or missing interfaces remain.
