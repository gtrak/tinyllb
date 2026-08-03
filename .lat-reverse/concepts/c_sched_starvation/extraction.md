# Starvation Detection (Extraction)

## Responsibilities

- Detect whether a flow has exceeded a configurable wait-time threshold while queued
- Record Prometheus metrics when a flow is force-admitted due to starvation

## Interface

### `is_starved` — Starvation Check

- **Inputs**: a flow reference; a `Duration` timeout threshold
- **Outputs**: `Some(wait_duration)` when the flow has waited longer than the timeout; `None` when the flow is not enqueued or has not exceeded the threshold
- **Error contracts**: may panic if the flow's enqueue-timestamp lock is poisoned or if the monotonic clock returns a value earlier than the recorded enqueue instant
- **Evidence**: `src/scheduler/starvation.rs:28-37` — reads `flow.enqueued_at`, computes `Instant::now().duration_since(queued_at)`, returns `Some(wait)` when `wait > timeout`
- **Callers**: `src/scheduler/drr.rs:313`, `src/scheduler/wfq.rs:316` — both invoke from within scheduler selection logic; `src/scheduler/completion_bias.rs:159-173` duplicates the same logic inline instead of calling this function

### `record_force_admit` — Metrics Recording

- **Inputs**: a metrics handle; a flow reference; the observed wait duration
- **Outputs**: none (side-effect only)
- **Error contracts**: none declared; relies on Prometheus metric handles being valid
- **Evidence**: `src/scheduler/starvation.rs:17-23` — sets `llm_flow_starvation_seconds{flow_id}` gauge to `wait.as_secs_f64()`; increments `llm_starvation_force_admits_total` counter
- **Callers**: `src/scheduler/drr.rs:315`, `src/scheduler/wfq.rs:318` — both invoke immediately after a positive `is_starved` result; `src/scheduler/completion_bias.rs:164-168` duplicates the same metric writes inline instead of calling this function

### Configuration Contract

- No configuration is consumed directly by this module; callers supply the timeout duration at invocation
- **Evidence**: `src/scheduler/starvation.rs:1` — module doc states "the actual starvation check is performed inline in each scheduler's `try_select`"

## Invariants

- A flow whose `enqueued_at` is `None` is never considered starved (`src/scheduler/starvation.rs:30-36`)
- Starvation requires strictly exceeding the threshold — equality does not trigger (`src/scheduler/starvation.rs:32`, `wait > timeout`)
- Wait time is measured against a monotonic clock (`Instant`), not wall clock (`src/scheduler/starvation.rs:31`)
- The `flow.enqueued_at` field is a `RwLock<Option<Instant>>` on the `Flow` struct (`src/flow/mod.rs:69`)

## Failure Modes

- **Poisoned lock panic**: `RwLock::read().unwrap()` panics if a prior read/write panicked while holding the lock (`src/scheduler/starvation.rs:29`)
- **Monotonic clock panic**: `Instant::duration_since()` panics if `Instant::now()` precedes `queued_at`, which can occur if the underlying clock implementation regresses (`src/scheduler/starvation.rs:31`)
- **Inline duplication**: `completion_bias.rs` replicates both the starvation check and metric recording without using this module (`src/scheduler/completion_bias.rs:159-173`), creating a second code path with identical logic and identical panic surfaces
