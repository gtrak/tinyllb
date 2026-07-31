# 11 — Deficit Round Robin Scheduler (V2) with Credit Bookkeeping

**Phase:** 2 (Agent Scheduling)
**Depends on:** `10`.
**Blocks:** `12`, `13`.

## Objective

Implement the V2 scheduler from PRD §6.4: **Deficit Round Robin (DRR)**.  Each
flow accumulates credit at a rate proportional to its weight; scheduling a
request consumes credit proportional to its work unit.  DRR is simpler and
more predictable than WFQ over variable workloads, which is why PRD prefers
it for the long-lived default.

Keep `algorithm: fifo` and `algorithm: wfq` selectable; `drr` becomes the
documented default.

## Files

| File | Change |
| --- | --- |
| `src/scheduler/drr.rs` | New: DRR impl with per-flow `credit: AtomicI64`. |
| `src/scheduler/mod.rs` | Edit: add `Drr` arm; default `algorithm` in config becomes `drr`. |
| `src/flow/mod.rs` | Edit: flow credit lives on `Flow` (moved from local to registry). |
| `src/metrics/queue.rs` | Edit: expose `llm_flow_credit{flow_id}` gauge per PRD §8. |
| `tests/scheduler_drr.rs` | New: credit accumulation, consumption, skip-when-deficit. |

## Steps

1. DRR rule per PRD §6.4:
   * Each tick (admission opportunity): for each waiting flow, `credit +=
     weight`.
   * Pick the flow at the head of the round-robin list whose `credit >=
     cost_of_head_request`; emit it, `credit -= cost`.  If no flow has enough
     credit, no admission this tick (wait for the next completion event).
2. `cost` of a head request = its `max_tokens` (Phase 1 / Phase 2 estimate).
   Real generated-token feedback is Phase 3.
3. Credit is per-flow, persistent in the registry — survives a flow's queue
   draining to empty.  When a flow's queue empties, reset credit to 0 (avoid
   unbounded growth per classic DRR discipline).
4. Add `llm_flow_credit{flow_id="..."}` gauge (PRD §8 Scheduling family).
   Update on every credit change.
5. Reset credit to its starting value on request cancel (`13` will call this).
6. Default `config.example.yaml`'s `algorithm: drr` already matches the PRD;
   confirm the default in code matches.
7. Tests:
   * Two flows weights `A=10, B=1`; both queues work-unit `10` each; with
     enough ticks, `A` gets ~10x the services of `B`,
   * Flow with deficit (credit < head cost) is skipped; next ready flow is
     selected instead (no stall of the whole queue),
   * Credit resets to 0 when a flow's queue empties.

## Verification

* `cargo test --test scheduler_drr` green; ratios respected within tolerance.
* `/metrics` exposes `llm_flow_credit{flow_id=...}` for each registered flow;
  value matches the scheduler's internal counter.
* Default config boots with `algorithm: drr` (assert in a config-load test).
* `algorithm: wfq` and `algorithm: fifo` still selectable without regressions.
