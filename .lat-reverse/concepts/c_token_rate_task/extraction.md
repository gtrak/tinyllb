# Token Rate Task

Background task that computes a smoothed tokens-per-second gauge from a
monotonically-increasing total token counter.

## Purpose

Compute a rolling-average tokens-per-second metric from a monotonically
increasing total token counter.

## Interface

No external API. Internal to [[c_token_rate_task]].

- **Input: token counter** — Monotonically-increasing total count of generated tokens; batch-updated at request completion with multi-token increments.
- **Input: smoothing window** — Configurable window duration in seconds specifying the averaging period; default 10s.
- **Output: tokens-per-second gauge** — Public Prometheus gauge (`llm_tokens_per_second`) reporting the current rolling-average rate.
- **Window floor** — Window is clamped to minimum 1 second; values below 1 are raised to 1.

## Invariants

- **Rolling window bounds** — The gauge value always reflects an average of at most `window_secs` per-second samples. Code evidence: `if samples.len() > window_secs as usize { samples.remove(0); }` ([`src/main.rs#51-53`]).
- **Monotonic counter assumption** — A counter decrease produces a zero delta, not a negative value. Code evidence: `let delta = if current_count >= previous_count { current_count - previous_count } else { 0.0 };` ([`src/main.rs#43-47`]).
- **Gauge formula** — The gauge equals `sum(samples) / samples.len()`, i.e., the denominator is the actual sample count, not the window size (matters during warmup). Code evidence: `tokens_per_second.set(sum / samples.len() as f64);` ([`src/main.rs#55`]).
- **Fixed sampling period** — One sample is produced per second; the period is constant regardless of computation time. Code evidence: `tokio::time::sleep(std::time::Duration::from_secs(1)).await;` ([`src/main.rs#41`]).
- **No shutdown mechanism** — The task loops indefinitely; no channel, signal, or external handle exists to terminate it. Code evidence: the `loop { ... }` body at [`src/main.rs#40-56`] contains no break or select condition.

## Constraints

- **Window is unsigned seconds** — The window parameter is `u64`, so negative values are impossible at the type level; only the zero case requires runtime clamping. Code evidence: `let window_secs = window_secs.max(1);` ([`src/main.rs#35`]).
- **Counter is `f64`** — All arithmetic uses `f64`; the Prometheus `Counter::get()` returns `f64`. Floating-point precision applies to deltas, sums, and the division.
- **Samples stored in Vec** — The rolling window is a `Vec<f64>` with O(n) removal of the head element via `samples.remove(0)`.
- **Task runs as fire-and-forget** — The task is spawned via `tokio::spawn`; the returned `JoinHandle` is not stored, making the task unjoinable and unabortable. Code evidence: the `tokio::spawn` call at [`src/main.rs#37`] is not assigned.
- **Gauge is a Prometheus `Gauge`** — The output is a `prometheus::Gauge`, so the caller can read it at any time; the value is always the last-written average.

## Failure Modes

- **No graceful shutdown** — The task cannot be stopped without dropping the entire runtime.
- **Counter reset produces zero-rate window** — If the counter is reset (e.g., process restart with persisted state), the delta becomes zero for the reset interval, producing an artificially-low rate.
- **Warmup inaccuracy** — During the first `window_secs` seconds, the average divides by fewer samples than the window, producing a mathematically-correct but potentially misleading rate.
- **Timer drift tolerance** — The 1-second sleep is approximate (OS scheduler dependent); actual sampling intervals may drift slightly from 1s without affecting correctness.

## Related

- [[src/metrics/mod.rs#27-28]] — Counter and gauge declarations
- [[src/config/mod.rs#324-325]] — Window configuration field
- [[src/gateway/proxy.rs#462]] — Counter increment site (batch update at request completion)
