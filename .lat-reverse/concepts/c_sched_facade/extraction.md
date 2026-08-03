# Extraction: Scheduler Facade (`src/scheduler/mod.rs`)

## Responsibilities

The scheduler module provides a unified admission and queue-management facade that dispatches to one of three scheduling algorithms (FIFO, WFQ, DRR) based on configuration. It exposes shared policy state (completion bias, starvation timeout, flow progress tracking) to all variants and runs a KV-cache admission gate before every admit attempt.

## Interface Surfaces

### `Scheduler::new()` — Full constructor

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 104–187 |
| **Inputs** | `algorithm: Algorithm`, `max_active_flows: u32`, `metrics: Arc<Metrics>`, `registry: Arc<FlowRegistry>`, `backpressure_mode`, `max_queue_depth: u32`, `max_wait: Duration`, `retry_after_base: Duration`, `starvation_timeout: Duration`, `completion_bias: CompletionBias`, `kv_config: KvPolicyConfig`, `monitor: Arc<BackendMonitor>` |
| **Output** | `Scheduler` instance |
| **Error contract** | Infallible construction; no `Result` wrapper |
| **Evidence** | Lines 104–187: constructs `Policies`, `KvPolicy`, and one of three `SchedulerImpl` variants via exhaustive match on `algorithm` |

### `Scheduler::new_with_defaults()` — Backward-compatible constructor

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 196–221 |
| **Inputs** | Subset of `new()` parameters: `algorithm`, `max_active_flows`, `metrics`, `registry`, `backpressure_mode`, `max_queue_depth`, `max_wait`, `retry_after_base` |
| **Output** | `Scheduler` instance |
| **Default values** | `starvation_timeout = 300s`, `completion_bias = CompletionBias::default()` (enabled, target = max_active_flows), `kv_config = KvPolicyConfig::default()` (enabled=false), `monitor = BackendMonitor::empty()` |
| **Evidence** | Lines 206–220: delegates to `Self::new()` with hardcoded defaults |

### `Scheduler::admit()` — Attempt to admit a request

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 232–270 |
| **Inputs** | `flow_id: FlowId`, `work_unit: f64` |
| **Output** | `Result<QueueTicket, BackpressureRejected>` |
| **Error contract** | Rejects with `BackpressureRejected` carrying a `retry_after` duration on failure |
| **Evidence** | Line 236: return type. Line 243: `self.kv_policy.check().await?` — KV gate runs first and short-circuits on rejection. Lines 244–248: dispatch to underlying `SchedulerImpl` variant. Lines 252–267: emits tracing events for both accept and reject outcomes |

### `Scheduler::queue_depth()` — Current queue depth

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 275–282 |
| **Inputs** | None |
| **Output** | `u32` |
| **Guarantee** | Total includes both flow-scheduler queue depth and KV-delayed count |
| **Evidence** | Line 281: `inner_depth + self.kv_policy.delayed_count()` |

### `Scheduler::queue_snapshot()` — Queue state snapshot

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 288–301 |
| **Inputs** | None |
| **Output** | `QueueSnapshot` with fields `active`, `waiting`, `flows` |
| **Guarantee** | `waiting` field sums flow-scheduler waiting count and KV-delayed count |
| **Evidence** | Lines 295–298: `waiting: inner_snapshot.waiting + delayed as u64` |

### `Scheduler::service_done()` — Per-flow service total

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 305–311 |
| **Inputs** | `flow_id: &FlowId` |
| **Output** | `f64` |
| **Guarantee** | Returns `0.0` for FIFO and DRR; meaningful value only for WFQ |
| **Evidence** | Lines 306–309: match returns `0.0` for `Fifo` and `Drr`, delegates to `s.service_done(flow_id)` for `Wfq` |

### `Scheduler::credit()` — Per-flow credit

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 315–321 |
| **Inputs** | `flow_id: &FlowId` |
| **Output** | `i64` |
| **Guarantee** | Returns `0` for FIFO and WFQ; meaningful value only for DRR |
| **Evidence** | Lines 317–319: match returns `0` for `Fifo` and `Wfq`, delegates to `s.credit(flow_id)` for `Drr` |

### `Scheduler::report_accounting()` — Report accounting for completed request

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 327–333 |
| **Inputs** | `flow_id: &FlowId`, `report: AccountingReport` |
| **Output** | None |
| **Guarantee** | No-op for FIFO and WFQ; adjusts per-flow credit for DRR |
| **Evidence** | Lines 328–331: match arms for `Fifo` and `Wfq` are empty; `Drr` delegates to `s.report_accounting(flow_id, report)` |

### `Scheduler::flow_progress_tracker()` — Access flow progress tracker

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/mod.rs` lines 336–338 |
| **Inputs** | None |
| **Output** | `Arc<FlowProgressTracker>` |
| **Evidence** | Line 337: returns `self.flow_progress.clone()` |

### Re-exported symbols (module-level)

| Symbol | Source | Kind |
|---|---|---|
| `BackpressureRejected` | `backpressure` | Error struct, pub field `retry_after: Duration` |
| `fail_fast_retry_after` | `backpressure` | Free function |
| `mode_label` | `backpressure` | Free function |
| `DrrScheduler` | `drr` | Scheduler struct |
| `FifoScheduler` | `fifo` | Scheduler struct |
| `QueueTicket` | `fifo` | Type |
| `make_ticket` | `fifo` | Free function |
| `FlowProgressTracker` | `flow_progress` | Type |
| `KvPolicy` | `kv_admission` | Type |
| `AccountingReport` | `lifecycle` | Type |
| `WfqScheduler` | `wfq` | Scheduler struct |
| `lifecycle` | `pub mod lifecycle` | Public submodule |

### `BackpressureRejected` — Rejected admission error

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/backpressure.rs` lines 4–8 |
| **Shape** | Struct with `pub retry_after: Duration` |
| **Traits** | Implements `std::error::Error`, `std::fmt::Display` |
| **Evidence** | Lines 5–8 (struct), line 20 (`Error` impl), lines 10–18 (`Display` impl) |

### `fail_fast_retry_after()` — Compute retry-after duration

| Aspect | Detail |
|---|---|
| **Location** | `src/scheduler/backpressure.rs` lines 27–39 |
| **Inputs** | `depth: u32`, `max_queue_depth: u32`, `retry_after_base: Duration` |
| **Output** | `Duration` |
| **Formula** | `retry_after_base * (1 + depth / max_queue_depth)` |
| **Edge case** | When `max_queue_depth == 0`, returns `retry_after_base * 2` |
| **Evidence** | Lines 32–38: ratio computation with div-by-zero guard |

## Invariants

### I1. KV policy gate executes before flow-scheduler dispatch
Every `admit()` call runs `self.kv_policy.check()` before consulting the underlying scheduler. A KV-policy rejection short-circuits the entire admit path. Evidence: `src/scheduler/mod.rs` line 243 (`self.kv_policy.check().await?`) precedes lines 244–248 (scheduler dispatch).

### I2. Queue depth always sums flow-scheduler and KV-delayed counts
`queue_depth()` returns the sum of the inner scheduler's queue depth and the KV policy's delayed count. Evidence: `src/scheduler/mod.rs` line 281 (`inner_depth + self.kv_policy.delayed_count()`).

### I3. Queue snapshot waiting field always includes KV-delayed requests
`queue_snapshot().waiting` adds the KV-delayed count to the flow-scheduler's waiting total. Evidence: `src/scheduler/mod.rs` line 298 (`inner_snapshot.waiting + delayed as u64`).

### I4. Algorithm dispatch is exhaustive over `Algorithm` variants
Every public method that delegates to the inner scheduler (`admit`, `queue_depth`, `queue_snapshot`, `service_done`, `credit`, `report_accounting`) covers all three `SchedulerImpl` variants. Evidence: `src/scheduler/mod.rs` lines 140–144 (match on `Algorithm` for label), lines 146–179 (match on `Algorithm` for inner construction), lines 244–248, 276–280, 289–293, 306–310, 317–320, 328–332 (all use exhaustive `match &self.inner`).

### I5. `service_done()` returns zero for non-WFQ algorithms
For FIFO and DRR variants, `service_done()` always returns `0.0`. Evidence: `src/scheduler/mod.rs` lines 307, 309.

### I6. `credit()` returns zero for non-DRR algorithms
For FIFO and WFQ variants, `credit()` always returns `0`. Evidence: `src/scheduler/mod.rs` lines 317–318.

### I7. `report_accounting()` is a no-op for non-DRR algorithms
FIFO and WFQ arms are empty. Evidence: `src/scheduler/mod.rs` lines 329–330 (`{}`).

### I8. `new_with_defaults()` applies fixed default values
`starvation_timeout` is fixed at 300 seconds. `completion_bias` uses `CompletionBias::default()`. `kv_config` uses `KvPolicyConfig::default()` which disables KV policy. `monitor` is `BackendMonitor::empty()`. Evidence: `src/scheduler/mod.rs` lines 216–219.

## Failure Modes

| Mode | Description | Evidence |
|---|---|---|
| KV-policy rejection | `admit()` returns `Err(BackpressureRejected)` if KV cache pressure exceeds threshold. Caller receives `retry_after` duration. | `src/scheduler/mod.rs` line 243: `self.kv_policy.check().await?` |
| Flow-scheduler rejection | `admit()` returns `Err(BackpressureRejected)` from the underlying FIFO/WFQ/DRR scheduler when queue is full or backpressure triggers. | `src/scheduler/mod.rs` lines 244–248: delegate returns `Result<QueueTicket, BackpressureRejected>` |
| `fail_fast_retry_after` div-by-zero | Guarded: returns `2x` base when `max_queue_depth == 0`. | `src/scheduler/backpressure.rs` lines 32–33 |
