# Extraction: c_sched_backpressure

Source: `src/scheduler/backpressure.rs`

---

## Responsibilities (Observable)

- Defines a public error type (`BackpressureRejected`) that carries a retry-after duration, used to signal queue-full rejections to callers.
- Computes a `Retry-After` duration for fail-fast backpressure based on queue depth and capacity.
- Maps the `BackpressureMode` enum to a human-readable label string.

---

## Interface Surfaces

### `BackpressureRejected` (error struct)

- **Kind:** Public error type
- **Inputs:** Constructor accepts `retry_after: Duration`
- **Outputs:** Carries a single `Duration` field exposed as `pub retry_after`
- **Error contract:** Implements `std::error::Error` and `std::fmt::Display`; display format: `"queue full, retry after {seconds}s"` (line 14-15)
- **Code evidence:** Lines 5-8, 10-18, 20

### `fail_fast_retry_after` (free function)

- **Kind:** Public free function
- **Inputs:** `depth: u32`, `max_queue_depth: u32`, `retry_after_base: Duration`
- **Outputs:** `Duration` — a scaled retry-after value
- **Contract:** Returns `retry_after_base * (1 + depth / max_queue_depth)` (line 37-38)
- **Guard:** When `max_queue_depth == 0`, ratio is clamped to `2.0` instead of dividing (line 32-33)
- **Code evidence:** Lines 27-39

### `mode_label` (free function)

- **Kind:** Public free function
- **Inputs:** `BackpressureMode` (from `crate::config`)
- **Outputs:** `&'static str` — lowercase label string
- **Mapping:** `Blocking -> "blocking"`, `FailFast -> "fail_fast"`, `Hybrid -> "hybrid"` (lines 43-47)
- **Code evidence:** Lines 42-48

---

## Invariants

- `fail_fast_retry_after` never divides by zero; `max_queue_depth == 0` maps to a fixed ratio of `2.0` (line 32-33).
- `fail_fast_retry_after` always returns a value >= `retry_after_base` (factor is `1.0 + ratio`, ratio >= 0) (line 37).
- `mode_label` is exhaustive over `BackpressureMode`; no fallthrough or default branch (lines 43-47).
- `BackpressureRejected` is `Clone + Debug` (line 4).

---

## Failure Modes

- **Queue-full rejection:** `BackpressureRejected` is the error surfaced when the queue is at capacity; the `retry_after` field indicates the suggested wait before retrying.
- **Division-by-zero edge:** `max_queue_depth == 0` is explicitly handled in `fail_fast_retry_after` (line 32); without this guard, the function would panic on `f64` division by zero (produces `inf` but the guard ensures deterministic behavior).
- **Mode mismatch:** `mode_label` has no default branch; adding a new `BackpressureMode` variant without updating `mode_label` would cause a compile error (exhaustiveness check).

---

## Notes

- No HTTP/RPC endpoints in this file.
- No trait implementations beyond `Display`, `Error`, `Debug`, `Clone`, `Default` (on config types).
- All three public items are `pub` and directly callable by external modules.
- `BackpressureRejected` is the sole error type defined in this module.
