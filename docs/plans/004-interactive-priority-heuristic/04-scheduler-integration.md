# 04 — Scheduler integration: apply priority on every admit

**Phase:** 3 (wiring)
**Depends on:** `02`, `03`.
**Blocks:** `05`, `06`.

## Objective

Wire the cadence heuristic and the explicit override into every
`admit()` path (FIFO, WFQ, DRR) so that `flow.priority()` reflects the
latest classification by the time `priority::select_best` runs. This
is the smallest possible intervention in the hot path — one helper
call before the existing backpressure gate.

## Files

| File | Change |
| --- | --- |
| `src/scheduler/mod.rs` | Hold `Arc<PriorityPolicy>` + `Arc<Priorities>` + `Arc<CadenceRegistry>`; call `record_arrival` + `classify_and_apply` in the unified `admit()` wrapper. |
| `src/scheduler/drr.rs` | (Optional) none if the top-level `Scheduler::admit` handles it uniformly. If per-algorithm hooks are needed, mirror the changes in `fifo.rs`/`wfq.rs`. |
| `src/scheduler/fifo.rs` | Same. |
| `src/scheduler/wfq.rs` | Same. |
| `src/scheduler/completion_bias.rs` | Read `flow.priority()` for tie-break (already does via `priority.rs` downstream — no change). |

## Steps

1. The cleanest design is to centralize this in
   `Scheduler::admit` (`src/scheduler/mod.rs:232`) **before** the
   dispatch to `inner.admit(...)`. Doing it there means FIFO, WFQ,
   and DRR all benefit from a single edit.

   ```rust
   pub async fn admit(
       &self,
       flow_id: crate::flow::FlowId,
       work_unit: f64,
   ) -> Result<QueueTicket, BackpressureRejected> {
       tracing::Span::current().record("queue_depth_before", self.queue_depth());

       // ── NEW: priority cadence heuristic ──
       let flow = self.registry.get_or_create(flow_id.clone());
       self.cadence.record_arrival(&flow_id, Instant::now());
       self.cadence.classify_and_apply(&flow, &flow_id);
       // ──────────────────────────────────────

       // existing KV-policy gate + dispatch unchanged
       let enter = std::time::Instant::now();
       self.kv_policy.check().await?;
       let result = match &self.inner { /* ... unchanged ... */ };
       // ... existing event/log code ...
       result
   }
   ```

   Add the new fields to `Scheduler`:

   ```rust
   pub struct Scheduler {
       inner: SchedulerImpl,
       kv_policy: Arc<KvPolicy>,
       flow_progress: Arc<FlowProgressTracker>,
       registry: Arc<FlowRegistry>,             // already held by inner — lift to outer too
       cadence: Arc<CadenceRegistry>,           // NEW
       priorities: Arc<Priorities>,              // NEW
       algorithm_label: &'static str,
   }
   ```

2. The explicit header override applied in `proxy.rs` (issue 03)
   rewrites `flow.priority`/`flow.priority_source` *before* the
   `admit()` call. Inside `admit()` the
   `classify_and_apply` step sees `priority_source() != 0` and
   bails out — exactly the desired one-way precedence (explicit pin
   wins, heuristic is silent until the pin is cleared).

3. `record_arrival` must use a monotonic clock (`Instant::now()`) —
   same clock already used by `enqueued_at` and the throughput
   windows. Do **not** use `SystemTime`. Existing code uses
   `std::time::Instant` throughout the scheduler, so this is free.

4. Confirm that `FlowRegistry` is shared across all scheduler
   algorithms so mutating `flow.priority` from `Scheduler::admit`
   is observed by each algorithm's `try_select`/`select_best`. The
   current code in `Scheduler::new` already constructs one
   `Arc<FlowRegistry>` and passes clones down — verify this by
   reading the constructor, and lift a second clone to the outer
   `Scheduler` struct if not already done.

5. Add `tracing::debug!` fields inside the `admit` span for
   observability: `priority` (current value), `priority_source`
   (0/1/2). These cost nothing if no subscriber enables them.

6. (Optional, deferred) If profiling shows the per-admit
   `record_arrival` allocation is materially hot, replace the
   `DashMap<FlowId, Cadence>` with an `Arc<Flow>`-held `Cadence`
   inline on `Flow` in a later optimization. Not in this issue.

## Verification

- `cargo build --all-targets` clean.
- `cargo test --all` green (existing FIFO/WFQ/DRR tests cover
  fairness — they shouldn't regress because cold-start flows still
  default to `priority=50`, same as today).
- New unit test asserting `flow.priority()` updated after an
  `admit()` call with a fast sequence of arrivals (`tests/
  priority_live.rs`, see 06).
- Manual verification: replay the 2026-08-03 contention pattern
  (e.g. via `hey` or `k6` with 2 slow flows + 8 fast flows) and
  inspect `journalctl --user -u tinyllb` to confirm slow
  flows see `priority=100` and fast flows see `priority=10` in the
  `admit` debug log lines.

## Notes

- The DRR `try_select` path already calls `flow.priority()` via
  `priority::FlowCandidate`. No changes to selection logic here —
  the new values flow through naturally.
- The completion-bias gate (`completion_bias.rs`) keys off
  `active_flows` counter, not priority — so its behavior is
  unchanged. Interactive flows don't bypass the slot cap; they just
  win the next available slot when another flow completes.
- Starvation protection (`scheduler/starvation.rs`) still applies at
  the configured 300s deadline regardless of priority class — verify
  with the e2e test in 06.
