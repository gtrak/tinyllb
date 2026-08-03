# Token Rate Task

Background metric that derives a smoothed tokens-per-second gauge from a monotonically-increasing total token counter.

## Purpose

This concept provides an operational view of LLM throughput by converting a cumulative token counter into a rate observable. It bridges the gap between point-in-time accounting (total tokens generated) and continuous monitoring (current throughput), enabling operators to detect performance trends without instrumenting every request path.

- Derives a rolling-average tokens-per-second value from a monotonically-increasing token counter.
- Reports the rate as a public Prometheus gauge for external consumption.
- Applies configurable temporal smoothing to suppress per-request spikes.
- Operates independently of request lifecycle; requires no per-request coordination.
- Assumes the token counter never decreases under normal operation.

## Non-goals

This concept deliberately excludes several common metric capabilities to remain focused on lightweight throughput observation.

- Does not support per-backend or per-model breakdowns; reports aggregate throughput only.
- Does not expose percentiles, histograms, or distribution shape; reports a single scalar rate.
- Does not provide graceful shutdown; the task runs until process termination.
- Does not backpressure or throttle based on the measured rate; observation only.
- Does not persist state across restarts; the rolling window resets on process start.

## Interface

This concept has no callable API. Its contract is defined entirely by its metric surface and configuration inputs.

- Accepts a token counter as input; the counter must be monotonically-increasing and updated either incrementally per-token during streaming responses or at request completion for non-streaming responses.
- Accepts a configurable smoothing window specifying the averaging period, measured in seconds.
- Exposes a single Prometheus gauge (`llm_tokens_per_second`) representing the current rolling-average rate in tokens per second.
- Clamps the smoothing window to a minimum of one second; sub-second configurations are promoted to the floor.
- The gauge converges to zero as non-zero deltas age out of the window; it reports exactly zero only after the window is entirely filled with zero deltas.

## Invariants

These properties hold regardless of implementation and must survive any rewrite.

- The gauge reflects a rolling average over at most `window_secs` consecutive seconds of observation; observations older than the window are excluded from the average.
- Counter decreases produce zero delta, never negative contributions; monotonicity violations are silently absorbed.
- During warmup before the window fills, the average is computed over fewer observations than the configured window size, and the initial observation may include any pre-existing counter accumulation.
- One observation is produced each second at a fixed cadence; the cadence is independent of counter update frequency.
- The task runs indefinitely; no external termination mechanism exists.

## Constraints

These are structural limitations imposed by the current design, not fundamental properties of the concept.

- The smoothing window is unsigned; negative values are impossible at the type level; only zero requires clamping to one.
- No upper bound on window size is enforced; extremely large windows may consume excessive resources.
- The task is fire-and-forget; no handle exists to join, abort, or observe the task lifecycle externally.
- The gauge reflects a point-in-time snapshot; concurrent reads return the last-written average.
- The token counter must provide consistent read access; inconsistent reads during concurrent increments could produce incorrect deltas.

## Rationale

These design choices reflect the operational needs of LLM monitoring, where smooth throughput trends matter more than precise per-request measurement.

- A rolling average over a cumulative counter avoids per-request instrumentation, keeping the measurement path decoupled from the request path.
- Treating counter decreases as zero-delta events prevents negative throughput values that would poison the average.
- Dividing by actual observation count during warmup preserves mathematical correctness rather than padding with zeros.
- A fixed one-second observation cadence keeps the gauge update rate predictable and independent of counter update frequency.
- Fire-and-forget execution matches the monitoring use case: the task observes but does not participate in request handling.

## Related

- [[c_telemetry]] — Metrics registry and gauge lifecycle
- [[c_metrics_families]] — Counter and gauge type system
- [[c_config_loading]] — Configuration injection for smoothing window
- [[c_gateway_proxy]] — Token counter increment site (batch update at request completion)
- [[c_gateway_stream]] — Token counter increment site (per-token streaming updates)
- [[src/metrics/mod.rs#27-28]] — Counter and gauge declarations
- [[src/config/mod.rs#324-325]] — Window configuration field
- [[src/gateway/proxy.rs#462]] — Counter increment site (non-streaming)
- [[src/gateway/stream.rs#187]] — Counter increment site (streaming)
