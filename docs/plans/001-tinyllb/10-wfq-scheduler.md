# 10 — Weighted Fair Queueing Scheduler (V1)

**Phase:** 2 (Agent Scheduling)
**Depends on:** `04`, `08`, `09`.
**Blocks:** `11`, `12`.

## Objective

Replace the global FIFO scheduler with **Weighted Fair Queueing** (PRD §6.4
V1).  Each flow has a weight; when an admission slot frees, the next request
chosen is from the flow that has received the **least** weighted service so
far (`service(flow_i) / weight_i`).

PRD's natural language: *"token allocation ∝ weight."* V1 crosses at request
granularity (PRD §5: scheduling unit is request-level for now).

## Files

| File | Change |
| --- | --- |
| `src/scheduler/wfq.rs` | New: WFQ impl driven by a `service_counter` per flow. |
| `src/scheduler/mod.rs` | Edit: `Scheduler` enum dispatches to `Fifo`/`Wfq` per config `algorithm`. |
| `src/gateway/proxy.rs` | Edit: report finished-request service back to scheduler. |
| `tests/scheduler_wfq.rs` | New: weight ratios honored, no flow starved. |

## Steps

1. Per flow, track `service_done: f64` (sum of "work units" of completed
   requests in that flow; for V1 the work unit is `max_tokens` of the
   request, a request-time-visible estimate — actual token feedback is
   Phase 3.
2. Admission pick: choose the flow with the **minimum** `service_done /
   weight`.  Ties broken FIFO by enqueue time.  Among that flow's queued
   requests, take the head (FIFO within flow, per PRD §6.4 MVP shape).
3. After a request finishes, add its `work_unit` to the flow's `service_done`
   and notify the admission loop.
4. Introduce a coordinated `SchedulerCmd` channel: requests push `Enqueue`,
   completed requests push `ReportService`, the admission task pops `Enqueue`
   when a slot is free and selects per WFQ rule.
5. `algorithm: fifo` still works (dispatches to `05`'s impl): no regression
   for existing tests.
6. Tests:
   * Two flows `A (weight 10)`, `B (weight 1)`, both queues pre-filled with
     10 equal-work requests, `max_active_flows=2`: over time, `A` completes
     ~10x the work units of `B` (within tolerance),
   * No flow goes indefinitely unserviced (PRD §G2),
   * A flow with weight 0 (rejection at register time in `09`) never
     bypasses — but also shouldn't dead-lock; validate explicitly.

## Verification

* `cargo test --test scheduler_wfq` green; ratio of completed work units
  ~ weight ratio within tolerance (e.g. ±15%).
* No flow starvation observed in tests; `llm_flow_starvation_seconds{flow_id}`
  added in `12` will harden this further.
* `algorithm: fifo` config still passes the Phase 1 `phase1_e2e` tests
  unchanged (no regression).
