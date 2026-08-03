# Issue 11 — Prometheus Metrics

## Objective

Add Prometheus metrics for compression events, tokens saved, sidecar
latency, and estimated context size per flow. These provide operational
visibility into the compression subsystem.

## Files

| File | Change |
|------|--------|
| `src/metrics/mod.rs` | Add `ContextMetrics` struct with counters/gauges/histograms |
| `src/gateway/proxy.rs` | Instrument `rewrite_messages` path |
| `src/context/compressor.rs` | Instrument `process_job` |

## Prerequisites

- Issue 07 (body rewriting — where `rewrite_messages` runs)
- Issue 09 (compression worker — where sidecar calls happen)

## Steps

1. **`ContextMetrics` struct** (add to `src/metrics/mod.rs` alongside
   existing `Metrics`):
   ```rust
   pub struct ContextMetrics {
       // Counters
       pub compression_events_total: Counter,
       pub compression_errors_total: Counter,
       pub compression_tokens_saved_total: Counter,

       // Histograms
       pub compression_sidecar_latency: Histogram,
       pub compression_turns_per_event: Histogram,

       // Gauges
       pub context_estimated_tokens: Gauge,        // labeled by flow_id
       pub context_raw_estimated_tokens: Gauge,     // labeled by flow_id
       pub context_compressed_segments: Gauge,       // labeled by flow_id
       pub context_compression_queue_depth: Gauge,  // pending jobs in channel
   }
   ```

2. **Register metrics** in the `Metrics::new()` function or a separate
   `ContextMetrics::new()` called from `main.rs`:
   ```rust
   impl ContextMetrics {
       pub fn new(registry: &Registry) -> Self {
           Self {
               compression_events_total: register_counter!(
                   "llm_qdisc_context_compression_events_total",
                   "Total compression events (successful summaries produced)",
                   registry
               )?,
               compression_errors_total: register_counter!(
                   "llm_qdisc_context_compression_errors_total",
                   "Total compression failures (sidecar errors, store errors)",
                   registry
               )?,
               compression_tokens_saved_total: register_counter!(
                   "llm_qdisc_context_compression_tokens_saved_total",
                   "Total tokens saved by compression (raw - summary)",
                   registry
               )?,
               compression_sidecar_latency: register_histogram_with_labels!(
                   HistogramOpts::new(
                       "llm_qdisc_context_compression_sidecar_latency_seconds",
                       "Latency of sidecar summarization requests"
                   ).buckets(vec![0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]),
                   registry
               )?,
               compression_turns_per_event: register_histogram!(
                   "llm_qdisc_context_compression_turns_per_event",
                   "Turns compressed per compression event",
                   vec![1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
                   registry
               )?,
               context_estimated_tokens: register_gauge_with_labels!(
                   GaugeOpts::new(
                       "llm_qdisc_context_estimated_tokens",
                       "Estimated forwarded tokens for the flow"
                   ),
                   &["flow_id"],
                   registry
               )?,
               // ... similar for others ...
           }
       }
   }
   ```

3. **Instrument `rewrite_messages` (in proxy.rs)** — after `reconcile()`:
   ```rust
   if let Some(ref ctx) = state.context {
       let metrics = &state.context_metrics;
       metrics.context_estimated_tokens
           .with_label_values(&[&flow_id.to_string()])
           .set(result.total_est_tokens as f64);
       metrics.context_raw_estimated_tokens
           .with_label_values(&[&flow_id.to_string()])
           .set(result.total_raw_est_tokens as f64);
   }
   ```

4. **Instrument `process_job` (in compressor.rs)**:
   ```rust
   // Before sidecar call:
   let sidecar_start = Instant::now();

   // After successful compression:
   self.metrics.compression_events_total.inc();
   self.metrics.compression_tokens_saved_total
       .inc_by((raw_tokens - summary_tokens) as u64);
   self.metrics.compression_sidecar_latency
       .observe(sidecar_start.elapsed().as_secs_f64());
   self.metrics.compression_turns_per_event
       .observe((job.turn_range_end - job.turn_range_start) as f64);

   // Update gauges:
   self.metrics.context_compressed_segments
       .with_label_values(&[&job.flow_id])
       .set(new_compressed_count as f64);
   self.metrics.context_estimated_tokens
       .with_label_values(&[&job.flow_id])
       .set(new_total_tokens as f64);

   // On failure:
   self.metrics.compression_errors_total.inc();
   ```

5. **Queue depth gauge** — update periodically in a background task
   (or in the worker's `run()` loop):
   ```rust
   // In CompressionWorker::run(), after each job:
   self.metrics.context_compression_queue_depth
       .set(self.rx.len() as f64);  // rx.len() requires mpsc Receiver
   ```
   Note: `mpsc::Receiver::len()` is available but may be approximate.
   This gives a ballpark of pending compression work.

6. **Expose in `/metrics`** — the existing `/metrics` endpoint already
   uses the same `Registry`. The new metrics are automatically included
   as long as they're registered on the same `Registry`.

7. **Cardinality control** for `flow_id` labels:
   - The existing `FlowRegistry` already aggregates ephemeral flows to
     metric label `"ephemeral"` (avoid cardinality explosion).
   - Apply the same pattern: if a flow is ephemeral, use `"ephemeral"`
     as the label value for context metrics.
   - Named flows (explicit `X-LLM-Flow-ID`) use their actual ID — these
     are bounded by the number of active sessions.
   - Add a config option `context_metrics_cardinality_limit` (default 100):
     if the number of distinct flow_id labels exceeds this, switch to
     aggregating to `"other"`. This is a safety valve against unbounded
     label cardinality.

8. **Metric naming convention**: all context compression metrics use the
   `llm_qdisc_context_` prefix, consistent with existing
   `llm_qdisc_` prefixed metrics.

## Tests

- `test_metrics_registered` — after startup, `/metrics` contains
  `llm_qdisc_context_compression_events_total` etc.
- `test_compression_increments_counter` — process a job, verify
  `compression_events_total` incremented by 1
- `test_tokens_saved_accumulates` — process 2 jobs, verify
  `compression_tokens_saved_total` = sum of both savings
- `test_sidecar_latency_recorded` — process a job, verify histogram has
  one observation in the correct bucket
- `test_gauge_updates_per_flow` — send 3 requests for same flow, verify
  `context_estimated_tokens` gauge reflects the latest value
- `test_ephemeral_flow_aggregation` — unnamed flows all appear under
  `"ephemeral"` label, not individual UUIDs

## Verification

```bash
cargo test --lib context_metrics 2>&1 | tail -10
# Manual check once running:
curl http://localhost:1234/metrics | grep llm_qdisc_context
```
