# KV-Admission Policy — Final Audit (Cycle 3)

## Audit scope

Compares `.lat-reverse/concepts/c_spec_kv_admission/spec.md` (twice-corrected) against `src/scheduler/kv_admission.rs`. Verified against `src/backend/mod.rs` (`wait_for` semantics), `src/config/mod.rs` (config defaults), `src/scheduler/backpressure.rs` (`BackpressureRejected`, `fail_fast_retry_after`), and `tests/kv_admission.rs`.

## Classification key

| Class | Meaning |
|---|---|
| `bug` | Spec states something the implementation does not do (or vice versa) |
| `spec_error` | Spec statement violates LAT rules (No How, interface leakage, etc.) |
| `undocumented_behavior` | Implementation behavior not captured by the spec |
| `missing_interface` | Public surface in the code absent from the spec |

---

## Findings

### 1. `spec_error` — "No How" violation: relaxed memory ordering (spec.md L53)

**Claim**: "Approximate counts are acceptable due to relaxed memory ordering."

**Issue**: "Relaxed memory ordering" is implementation-specific terminology (`Ordering::Relaxed`) that leaks HOW into an invariant section. A rewrite could use a channel, a lock-free stack, or a database — the behavioral invariant is that approximate counts are acceptable, not that atomic relaxed ordering is used.

**Fix direction**: Replace with "Approximate counts are acceptable; strict consistency across concurrent increments and decrements is not required."

### 2. `spec_error` — Incorrect visibility claim for `KVMDecision` (spec.md L34)

**Claim**: "Crate-internal public — any module within the crate can reference it directly."

**Issue**: The implementation declares `pub enum KVMDecision` inside `src/scheduler/kv_admission.rs`, which is inside a library crate (`lib.rs` exposes `pub mod scheduler`). `pub` on an enum in a library crate is fully public to external consumers, not crate-internal. The visibility claim understates the actual scope.

**Fix direction**: State "Fully public — accessible to any consumer of the crate" or, if the intent was crate-internal visibility, the implementation should use `pub(crate)`.

### 3. `undocumented_behavior` — Debug-vs-release underflow semantics (spec.md L85)

**Claim**: "Underflow may wrap silently if decrement operations exceed increment operations on the delayed count."

**Issue**: `AtomicU32::fetch_sub` wraps silently in release builds but **panics** in debug builds (Rust's overflow checking). The spec's "may wrap silently" is only partially true — it omits the debug-build panic, which is a real observable behavior in development and CI environments.

**Fix direction**: Add "In debug builds, underflow panics; in release builds, it wraps silently. Neither mode guards against a negative effective count."

### 4. `undocumented_behavior` — Stale monitor (channel open, no updates) (spec.md L70-71)

**Claim**: "When the backend monitor becomes unavailable, the gate admits by default rather than rejecting. This applies both when the monitor is unavailable at the initial check and when it closes during an active delay wait."

**Issue**: The spec and implementation both handle the case where the monitor channel **closes** (returns `Ok(())` via `wait_for`'s early return on `is_err()`). However, the spec does not address the case where the monitor channel remains open but the backend stops sending snapshots (e.g., vLLM process hangs without terminating). In this scenario, `wait_for` blocks indefinitely in blocking mode. This is a distinct failure mode from "monitor becomes unavailable" and is not covered by the Safe Degradation invariant.

**Fix direction**: Add a caveat: "A stale monitor (channel open, no new snapshots) is not treated as unavailability; blocking mode may wait indefinitely if snapshots cease without channel closure."

---

## No-How lint summary

| Section | Violation | Severity |
|---|---|---|
| Invariants (L53) | "relaxed memory ordering" — implementation terminology | spec_error |
| Interface (L34) | "Crate-internal public" — visibility claim mismatches implementation | spec_error |
| All other sections | Clean | pass |

No control flow descriptions, no data structure details, and no function/method names used as concept identifiers were found outside the two violations above.

---

## Verified-correct claims (spot-checked)

- **Threshold ordering**: Spec correctly states misconfiguration is not enforced. Implementation has no validation. ✓
- **Strict inequality at boundaries**: `kv_usage > reject_threshold` and `kv_usage > delay_threshold` — at-exactly-delay → accept, at-exactly-reject → delay. Matches spec L84. ✓
- **Config defaults**: `enabled: false`, `reject_threshold: 0.95`, `delay_threshold: 0.80` verified in `src/config/mod.rs`. ✓
- **Retry-After formula (reject)**: `5.0 + excess * 10.0` seconds verified at `kv_admission.rs:122`. ✓
- **Retry-After formula (delay-timeout)**: `fail_fast_retry_after(depth, max_queue_depth, retry_after_base)` = `base * (1 + depth / max_depth)`. ✓
- **Metrics completeness**: One label per initial decision, disabled path bypasses metrics. ✓
- **Safe Degradation (initial check)**: `snapshot()` returning `None` → `Ok(())`. ✓
- **Safe Degradation (channel close during wait)**: `wait_for` returns on `rx.changed().is_err()` → proceeds to `Ok(())`. ✓
- **Delayed-Count Consistency**: `DelayGuard` RAII covers success, timeout, and cancellation paths. ✓
- **Backpressure mode semantics**: Blocking → unbounded wait; Hybrid/FailFast → bounded by `max_wait`. ✓

## Test coverage gaps

| Scenario | Covered |
|---|---|
| Below threshold → accept | ✓ `kv_admission_accept_below_threshold` |
| Delay band → drop → accept | ✓ `kv_admission_delay_until_drop` |
| Above reject → reject | ✓ `kv_admission_reject_above_threshold` |
| Disabled → always accept | ✓ `kv_admission_disabled_always_accepts` |
| Empty monitor → accept | ✓ `kv_admission_empty_monitor_accepts` |
| Metrics counter accuracy | ✓ `kv_admission_decision_counter_increments` |
| Delay → accept transition | ✓ `kv_admission_delay_to_accept_transition` |
| Hybrid timeout → reject | ✓ `kv_admission_hybrid_delay_timeout_rejected` |
| Delayed count in queue | ✓ `kv_admission_delayed_visible_in_queue_depth` |
| Monitor channel close during delay wait → admit | ✗ Not tested |
| At-exactly-delay-threshold → accept | ✗ Not tested |
| At-exactly-reject-threshold → delay | ✗ Not tested |

## Verdict

The spec is substantially correct. Two `spec_error` findings require correction before integration (No How violation and visibility claim). Two `undocumented_behavior` findings document edge cases that exist in the implementation but are not reflected in the spec. No `bug` or `missing_interface` findings were identified.
