# 05 — Metrics

**Phase:** 3 (observability)
**Depends on:** `04`.
**Blocks:** `06`.

## Objective

Expose the new priority machinery via Prometheus so operators can
watch classification in real time and tune the gap thresholds.

## Files

| File | Change |
| --- | --- |
| `src/metrics/mod.rs` | Register 3 new collectors on `Metrics`. |
| `src/scheduler/mod.rs` | Increment counters / set gauges from `admit()` and the override helper. |
| `src/flow/cadence.rs` | Update `llm_flow_inter_request_seconds` histogram on each `record_arrival`. |
| `src/metrics/endpoint.rs` | (Verify only) render the new series in `/metrics`. |

## Steps

1. Add to `Metrics` struct (`src/metrics/mod.rs:16`):

   ```rust
   /// Per-flow numeric priority value (100/50/10).
   pub flow_priority_class: GaugeVec,             // labels: [flow_id]

   /// Source of the priority value: heuristic, header, admin.
   pub flow_priority_source_total: CounterVec,    // labels: [flow_id, source]

   /// Observed inter-request gap per flow (seconds).
   pub flow_inter_request_seconds: HistogramVec,  // labels: [flow_id]
   ```

   Reuse the histogram buckets already in use for `queue_wait_seconds`:
   `vec![0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0]`.

2. Register all three in `Metrics::new()` and add to the struct
   initializer (mirror the existing registration stanza exactly —
   `registry.register(Box::new(...))` then assignment).

3. Update sites:

   - `Scheduler::admit` (`src/scheduler/mod.rs`) — after
     `classify_and_apply`, set:
     ```rust
     self.metrics.flow_priority_class
         .with_label_values(&[flow_id.metric_label()])
         .set(flow.priority() as f64);
     ```
   - `Scheduler::apply_priority_override` — set the gauge the same
     way *and* increment the `flow_priority_source_total` counter
     with the appropriate `source` label:
     - `header` (the override-class branch),
     - `admin` (the `register_handler` path in `api/flows.rs`),
     - `auto` (the unset branch, counted as a "manual resume" event).
   - `CadenceRegistry::record_arrival` — when a previous arrival
     exists, compute the gap (`now - last`) and observe it under
     `flow_inter_request_seconds{flow_id=...}`. The first arrival
     for a flow is skipped (no prior gap).

4. Keep the existing `flow_id` label normalization: use
   `FlowId::metric_label()` (ephemeral aggregates to `"ephemeral"`)
   to avoid Prometheus cardinality blowup.

5. Verify via running upgrade:
   ```bash
   systemctl --user restart tinyllb.service
   curl -sS http://localhost:1234/metrics | grep -E \
     'llm_flow_priority_class|llm_flow_priority_source_total|llm_flow_inter_request_seconds'
   ```
   All three series must appear with at least one labeled series.

## Verification

- `cargo test --all` green (no metric dedup errors during init).
- The `/metrics` scrape above returns all three series with
  sensible values (e.g. `llm_flow_priority_class{flow_id=...}
  100`).
- `cargo test --test metrics_smoke` (if exists) — add a minimal
  assertion that all three collectors register without panicking.
