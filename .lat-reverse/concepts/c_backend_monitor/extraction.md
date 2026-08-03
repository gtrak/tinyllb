# c_backend_monitor — Extraction

## Responsibilities

- Parse Prometheus text-format metrics bodies from the vLLM `/metrics` endpoint into typed snapshots.
- Periodically poll a vLLM backend `/metrics` endpoint and publish updated snapshots via a watch channel.
- Expose the latest snapshot to callers with single-writer / multi-reader semantics.
- Block callers until a snapshot satisfies an arbitrary predicate.

## Interface Surfaces

### Metric name constants

- Four `pub const` strings identify vLLM metric names consumed by the parser: KV usage (v0), KV usage (v1), KV free, and cumulative preemptions. Callers may reference these to validate against their own vLLM deployment. Evidence: lines 28, 35, 39, 43.

### `BackendSnapshot`

- Public struct carrying three fields: `kv_usage: f64` (fraction [0..1]), `kv_free: f64` (fraction [0..1]), `preemptions: u64` (cumulative, best-effort). Implements `Default`: usage=0.0, free=1.0, preemptions=0. Evidence: lines 51-58, 60-68.

### `ParseSnapshotResult`

- Public struct carrying a `BackendSnapshot` plus two boolean flags (`found_usage`, `found_free`) distinguishing metric-present-zero from metric-absent. Implements `Default`. Evidence: lines 74-82.

### `parse_snapshot(body: &str) -> ParseSnapshotResult`

- Public function accepting raw Prometheus text body; returns a `ParseSnapshotResult`. Scans every line, matching known metric names. If usage is found but free is absent and usage < 1.0, derives free as `1.0 - usage`. Malformed lines are silently skipped. Evidence: lines 123-157.

### `BackendMonitor` (constructor surfaces)

- `empty()` — Returns a monitor with a static default snapshot; no background task. Evidence: lines 179-182.
- `from_receiver(receiver)` — Returns a monitor wrapping an existing watch receiver. Evidence: lines 188-190.
- `new(config, metrics, client) -> (Self, Option<Task>)` — Returns a monitor handle and an optional background polling task. When `config.metrics_interval` is zero, the task is `None` (monitoring disabled). Evidence: lines 195-219.

### `BackendMonitor` (read surfaces)

- `snapshot(&self) -> Option<BackendSnapshot>` — Returns the latest snapshot or `None` if the watch channel is closed. Evidence: lines 265-267.
- `wait_for(&self, predicate)` — Async method that blocks until the predicate evaluates to true on the current snapshot, or the channel closes. Evidence: lines 276-287.

### KV metrics reporting (Prometheus gauges)

- After each successful poll, the monitor writes `kv_usage` and `kv_free` into external `Metrics` gauges (`vllm_kv_cache_usage`, `vllm_kv_cache_free`). Evidence: lines 249-250.

## Invariants

### Usage + free = 1.0 (when both sourced)

- When the free gauge is absent but usage is present, the parser derives free as `1.0 - usage`. When both are present, their sum is whatever the backend reports (no enforcement). Evidence: lines 148-149.

### Default snapshot represents "idle, zero pressure"

- `BackendSnapshot::default()` sets `kv_usage=0.0`, `kv_free=1.0`, `preemptions=0`. Evidence: lines 60-68.

### Monitoring errors never affect admission

- HTTP failures and body-read failures preserve the last published snapshot; no default is injected on error. Evidence: lines 252-259.

### v0 and v1 metric names are interchangeable

- Both `METRIC_KV_USAGE` and `METRIC_KV_USAGE_V1` map to the same `kv_usage` field. When both appear, the last parsed value wins. Evidence: lines 131-134.

### Preemptions are best-effort

- Missing preemption metric leaves `preemptions=0` (default). Evidence: lines 139-141, 147.

## Failure Modes

### Backend unreachable

- The poll loop logs a warning and retains the last snapshot. No snapshot is cleared or reset. Evidence: lines 256-259.

### Body read failure

- The poll loop logs a warning and retains the last snapshot. Evidence: lines 252-254.

### Watch channel closed

- `snapshot()` returns `None`. `wait_for()` returns immediately. Evidence: lines 266, 282-284.

### Metric absent from body

- Missing usage → `kv_usage=0.0`, derived free=1.0. Missing free (with usage present) → derived from usage. Missing preemptions → 0. Evidence: lines 123-157, 60-68.

### Monitoring disabled

- When `metrics_interval` is zero, `new()` returns `None` for the task handle; the monitor holds a static default snapshot forever. Evidence: lines 202-204.

## Related

- `src/backend/mod.rs`
