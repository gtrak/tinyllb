# c_backend_metrics_parser

## Responsibilities

- Parse Prometheus text-format metric bodies into typed `BackendSnapshot` values
- Expose named constants for vLLM metric identifiers
- Maintain a watch channel providing the latest snapshot to concurrent readers
- Derive `kv_free` from `kv_usage` when the free gauge is absent

## Interface Surfaces

### Metric Name Constants

| Constant | Value | Line |
|---|---|---|
| `METRIC_KV_USAGE` | `"vllm:gpu_cache_usage_perc"` | 28 |
| `METRIC_KV_USAGE_V1` | `"vllm:kv_cache_usage_perc"` | 35 |
| `METRIC_KV_FREE` | `"vllm:gpu_cache_free_perc"` | 39 |
| `METRIC_NUM_PREEMPTION` | `"vllm:num_preemptions_total"` | 43 |

### BackendSnapshot Type (lines 51-68)

- Public fields: `kv_usage: f64`, `kv_free: f64`, `preemptions: u64`
- `Default` impl: `kv_usage=0.0`, `kv_free=1.0`, `preemptions=0` (lines 61-67)

### ParseSnapshotResult Type (lines 74-82)

- Public fields: `snapshot: BackendSnapshot`, `found_usage: bool`, `found_free: bool`
- `Default` impl derives from field defaults

### parse_snapshot (lines 123-157)

- Signature: `pub fn parse_snapshot(body: &str) -> ParseSnapshotResult`
- Accepts any `&str`; iterates lines; matches metric names against constants
- Returns `ParseSnapshotResult` with populated snapshot and found flags
- Malformed lines silently skipped; unknown metric names silently ignored

### BackendMonitor (lines 168-288)

- `empty() -> Self` — disabled monitor, default snapshot (lines 179-182)
- `from_receiver(receiver) -> Self` — construct from existing watch receiver (lines 188-190)
- `new(config, metrics, client) -> (Self, Option<JoinHandle<()>>)` — creates polling task or `None` if interval is zero (lines 195-218)
- `snapshot(&self) -> Option<BackendSnapshot>` — latest value, `None` if channel closed (lines 265-267)
- `wait_for(&self, predicate) -> ()` — async block until predicate is true or channel closed (lines 276-287)

### Prometheus Line Parser (lines 95-117)

- Signature: `fn parse_prometheus_line(line: &str) -> Option<(&str, f64)>`
- Skips empty lines and `#`-prefixed comment lines (line 97)
- Extracts metric name before `{` or first space (lines 102-106)
- Extracts value as last whitespace-delimited token parseable as `f64` (lines 111-114)
- Returns `None` when line is empty, commented, missing name boundary, or has non-numeric value

## Invariants

### kv_free derivation condition (lines 148-150)
When `found_usage == true` AND `found_free == false` AND `kv_usage < 1.0`, then `kv_free == 1.0 - kv_usage`. If `kv_usage >= 1.0`, derivation is skipped and `kv_free` remains at its default (1.0).

### v0 and v1 metric names map to same field (lines 131-134)
`METRIC_KV_USAGE` and `METRIC_KV_USAGE_V1` both write to `snapshot.kv_usage`; no distinction is preserved in the output.

### Last occurrence wins (lines 131-132, test lines 493-501)
When multiple lines match the same metric constant, the last parsed value overwrites the snapshot field. Verified by `parse_snapshot_v0_and_v1_both_present_v1_wins` test (lines 493-501).

### Preemption truncation (line 140)
`snapshot.preemptions` is assigned `value as u64`; fractional preemption values are truncated toward zero.

### Disabled monitor retains default (lines 202-204)
When `config.metrics_interval` is zero, no polling task is spawned and `snapshot()` always returns the default `BackendSnapshot`.

### Channel closure behavior (line 266)
`snapshot()` returns `None` when the watch channel sender is dropped.

### Preemptions default is zero (lines 65-67)
Default `preemptions` is `0`; absent metric lines leave it at zero (verified test line 417-418).

## Failure Modes

### Backend unreachable (lines 256-259)
HTTP request to `/metrics` fails: warning logged, last snapshot retained in watch channel. No reset to defaults.

### Response body read failure (lines 253-254)
`response.text()` errors: warning logged, snapshot unchanged.

### Watch channel send failure (line 247)
`tx.send()` result is discarded (`let _ =`); if the channel is closed, the receiver never sees the update. No error propagated.

### Malformed lines silently skipped (lines 95-117)
Lines without a valid metric name boundary or non-numeric trailing value return `None` from `parse_prometheus_line` and are skipped without error indication.

### Unknown metric names ignored (line 142)
Lines matching Prometheus format but not matching any known constant fall through to `_ => {}` arm; silently ignored.

### wait_for channel closure (line 283)
If the watch channel is closed during `wait_for`, the function returns immediately without satisfying the predicate.

### Missed ticks skipped (line 237)
`MissedTickBehavior::Skip` means polling intervals are dropped behind if processing takes longer than the interval.

## Related

- [[src/backend/mod.rs#BackendSnapshot]]
- [[src/backend/mod.rs#parse_snapshot]]
- [[src/backend/mod.rs#BackendMonitor]]
