# KV-Admission Policy — Extraction

## Responsibilities

- Reads the latest `BackendSnapshot` from `BackendMonitor` and decides whether to accept, delay, or reject a request based on KV-cache pressure.
- Exposes a `check()` async gate that callers invoke before admitting a request.
- Counts delayed requests in an atomic counter visible to queue-depth queries.
- Records admission decisions (`accept`, `delay`, `reject`) to `Metrics`.

## Interface Surfaces

### `KVMDecision` enum (public, lines 21-29)

| Variant | Meaning |
|---|---|
| `Accept` | KV pressure is below delay threshold — proceed. |
| `Delay` | KV pressure is above delay threshold — park the request. |
| `Reject(Duration)` | KV pressure exceeds reject threshold — return 429 with Retry-After equal to the embedded duration. |

**Inputs:** none (discriminant-only, data-carrying in `Reject`).
**Outputs:** decision to caller.
**Code evidence:** `src/scheduler/kv_admission.rs:21-29`

---

### `KvPolicy::new()` constructor (public, lines 90-111)

| Parameter | Type | Role |
|---|---|---|
| `config` | `&KvPolicyConfig` | Supplies `enabled`, `reject_threshold`, `delay_threshold`. |
| `monitor` | `Arc<BackendMonitor>` | Shared reference for snapshot reads and wait loops. |
| `metrics` | `Arc<Metrics>` | Handles for recording decisions. |
| `backpressure_mode` | `BackpressureMode` | Selects delay wait behavior (Blocking vs Hybrid/FailFast). |
| `max_wait` | `Duration` | Bounded timeout for Hybrid/FailFast delay waits. |
| `retry_after_base` | `Duration` | Base duration for Retry-After scaling on delay timeout. |
| `max_queue_depth` | `u32` | Upper bound for Retry-After scaling on delay timeout. |

**Postconditions:** `delayed_count` is initialized to 0. All thresholds and durations are stored from arguments.
**Code evidence:** `src/scheduler/kv_admission.rs:90-111`

---

### `KvPolicy::delayed_count()` method (public, lines 136-138)

**Preconditions:** none.
**Postconditions:** Returns the current number of requests waiting in the delay loop (atomic load, `Relaxed` ordering).
**Code evidence:** `src/scheduler/kv_admission.rs:136-138`

---

### `KvPolicy::check()` async method (public, lines 149-226)

**Preconditions:** none.
**Postconditions / Outputs:**
- Returns `Ok(())` when the request is admitted or the delay wait succeeds.
- Returns `Err(BackpressureRejected { retry_after })` when rejected.
- Increments `kv_admission_decisions_total` metric with label `accept`/`delay`/`reject`.
- Increments `delayed_count` while a request is in the delay wait; decrements on exit (success, timeout, or cancellation) via `DelayGuard`.

**Decision rules (observable):**
- If `enabled == false` → always returns `Ok(())` (line 151-153).
- If `monitor.snapshot()` returns `None` → returns `Ok(())` (line 158-161).
- If `kv_usage > reject_threshold` → returns `Err(BackpressureRejected)` with `retry_after = 5s + (excess * 10s)` where `excess = kv_usage - reject_threshold` (lines 118-123, 217-222).
- If `kv_usage > delay_threshold` → enters delay wait (lines 172-216):
  - **Blocking mode**: waits indefinitely until `kv_usage <= delay_threshold` (lines 185-189).
  - **Hybrid/FailFast mode**: waits up to `max_wait`; on timeout returns `Err(BackpressureRejected)` with `retry_after` from `fail_fast_retry_after(delayed_count, max_queue_depth, retry_after_base)` (lines 191-209).
- Otherwise (`kv_usage <= delay_threshold`) → returns `Ok(())` (lines 165-170).

**Code evidence:** `src/scheduler/kv_admission.rs:149-226`

---

### `DelayGuard` (private, lines 232-246)

RAII guard that increments `delayed_count` on construction and decrements on drop. Scope-limited to the delay branch of `check()`.
**Code evidence:** `src/scheduler/kv_admission.rs:232-246`

## Invariants

1. **Threshold ordering:** `delay_threshold <= reject_threshold` is assumed by the `decide()` branch order (line 118 rejects first, then line 124 delays). If `reject_threshold <= delay_threshold`, the delay branch is unreachable. Code evidence: `src/scheduler/kv_admission.rs:117-129`.

2. **Delayed count consistency:** `delayed_count` reflects exactly the set of requests currently inside the delay wait. It is incremented before entering the wait and decremented on any exit path (success, timeout, cancellation) via `DelayGuard::drop`. Code evidence: `src/scheduler/kv_admission.rs:178, 214, 243-246`.

3. **Bounded vs. unbounded waits:** Blocking mode produces unbounded delay waits; Hybrid/FailFast modes produce bounded waits (`max_wait`). Code evidence: `src/scheduler/kv_admission.rs:185-209`.

4. **Monitor-closed fallback:** When the monitor channel closes (`snapshot() == None`), `check()` returns `Ok(())` rather than rejecting. Code evidence: `src/scheduler/kv_admission.rs:156-161`.

## Failure Modes

| Trigger | Behavior | Code evidence |
|---|---|---|
| `monitor.snapshot()` returns `None` | Returns `Ok(())` (accept-by-default). Logs warning. | `src/scheduler/kv_admission.rs:156-161` |
| `kv_usage > reject_threshold` | Returns `Err(BackpressureRejected)` with computed Retry-After. | `src/scheduler/kv_admission.rs:118-123, 217-222` |
| Delay wait times out (Hybrid/FailFast) | Returns `Err(BackpressureRejected)` with `fail_fast_retry_after`-derived Retry-After. | `src/scheduler/kv_admission.rs:199-208` |
| `delayed_count` underflow | `AtomicU32::fetch_sub` wraps on underflow; no runtime panic. The guard is paired 1:1 with the increment, so this path requires a bug in guard construction/destruction ordering. | `src/scheduler/kv_admission.rs:245` |
| Cancellation during delay wait | `DelayGuard` is dropped on cancellation, correctly decrementing `delayed_count`. | `src/scheduler/kv_admission.rs:243-246` |

## Related

- `src/backend/` — `BackendMonitor`, `BackendSnapshot`
- `src/config/` — `KvPolicyConfig`, `BackpressureMode`
- `src/metrics/` — `Metrics`
- `src/scheduler/backpressure/` — `BackpressureRejected`, `fail_fast_retry_after`
